//! Postgres-backed job queue (docs/architecture/04-database.md §4, A1-T06).
//!
//! - Claim with `FOR UPDATE SKIP LOCKED`, lease 5 min kept alive by a heartbeat.
//! - A reaper requeues jobs whose lease expired (crashed worker).
//! - Failures retry with exponential backoff (`2^attempts × 10 s`); after `max_attempts`
//!   the job is `dead` (visible in the admin UI with a retry button).
//! - `dedupe_key` prevents duplicate queued/running jobs.
//! - Periodic schedules are enqueued under an advisory lock, once per period cluster-wide.
//!
//! Modules register handlers by kind; Agent 4 adds `social.*` handlers through
//! `crate::social::job_handlers()`.

mod builtin;

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

pub const LEASE: Duration = Duration::from_secs(300);
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const REAPER_INTERVAL: Duration = Duration::from_secs(30);
pub const SCHEDULER_INTERVAL: Duration = Duration::from_secs(30);

/// Why a job attempt failed.
#[derive(Debug)]
pub enum JobError {
    /// Try again later (with backoff), up to `max_attempts`.
    Retry(String),
    /// Never retry; the job becomes `dead` now.
    Fatal(String),
}

impl<E: std::fmt::Display> From<E> for JobError {
    fn from(e: E) -> Self {
        JobError::Retry(e.to_string())
    }
}

/// What a handler receives.
#[derive(Clone)]
pub struct JobContext {
    pub state: AppState,
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    /// 1 on the first run.
    pub attempt: i32,
}

pub type JobHandler =
    Arc<dyn Fn(JobContext) -> BoxFuture<'static, Result<(), JobError>> + Send + Sync>;

/// Wraps an async fn as a [`JobHandler`].
pub fn handler<F, Fut>(f: F) -> JobHandler
where
    F: Fn(JobContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), JobError>> + Send + 'static,
{
    Arc::new(move |ctx| Box::pin(f(ctx)))
}

/// A job run every `period` (cluster-wide, once per period).
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    pub kind: &'static str,
    pub period: Duration,
}

pub const SCHEDULES: &[Schedule] = &[
    Schedule {
        kind: "sweep.expired",
        period: Duration::from_secs(60),
    },
    Schedule {
        kind: "saves.gc",
        period: Duration::from_secs(86_400),
    },
];

#[derive(Clone, Default)]
pub struct Registry {
    handlers: HashMap<String, JobHandler>,
}

impl Registry {
    /// Every handler of this service.
    pub fn standard() -> Self {
        let mut r = Self::default();
        for (kind, h) in builtin::handlers() {
            r.register(kind, h);
        }
        for (kind, h) in crate::versions::job_handlers() {
            r.register(kind, h);
        }
        for (kind, h) in crate::metadata::job_handlers() {
            r.register(kind, h);
        }
        for (kind, h) in crate::social::job_handlers() {
            r.register(kind, h);
        }
        r
    }

    pub fn register(&mut self, kind: &str, h: JobHandler) {
        self.handlers.insert(kind.to_string(), h);
    }

    pub fn kinds(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }
}

/// Options for [`enqueue`].
#[derive(Clone, Debug, Default)]
pub struct Enqueue {
    pub run_at: Option<OffsetDateTime>,
    pub priority: i16,
    pub max_attempts: Option<i32>,
    pub dedupe_key: Option<String>,
}

/// Enqueues a job. Returns `None` when a queued/running job with the same `dedupe_key` exists.
pub async fn enqueue(
    conn: &mut PgConnection,
    kind: &str,
    payload: Value,
    opts: Enqueue,
) -> Result<Option<Uuid>, ApiError> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO jobs (kind, payload, run_at, priority, max_attempts, dedupe_key)
           VALUES ($1, $2, COALESCE($3, now()), $4, COALESCE($5, 5), $6)
           ON CONFLICT (dedupe_key) WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running') DO NOTHING
           RETURNING id"#,
        kind,
        payload,
        opts.run_at,
        opts.priority,
        opts.max_attempts,
        opts.dedupe_key
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(id)
}

/// [`enqueue`] on a pool connection.
pub async fn enqueue_now(
    pool: &PgPool,
    kind: &str,
    payload: Value,
    opts: Enqueue,
) -> Result<Option<Uuid>, ApiError> {
    let mut conn = pool.acquire().await?;
    enqueue(&mut conn, kind, payload, opts).await
}

struct Claimed {
    id: Uuid,
    kind: String,
    payload: Value,
    attempts: i32,
    max_attempts: i32,
}

async fn claim(
    state: &AppState,
    worker: &str,
    kinds: &[String],
) -> Result<Option<Claimed>, sqlx::Error> {
    let row = sqlx::query!(
        r#"UPDATE jobs
           SET state = 'running', locked_by = $1, locked_until = now() + make_interval(secs => $2),
               attempts = attempts + 1
           WHERE id = (
             SELECT id FROM jobs
             WHERE state = 'queued' AND run_at <= now() AND kind = ANY($3)
             ORDER BY priority DESC, run_at
             LIMIT 1
             FOR UPDATE SKIP LOCKED
           )
           RETURNING id, kind, payload, attempts, max_attempts"#,
        worker,
        LEASE.as_secs_f64(),
        kinds
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| Claimed {
        id: r.id,
        kind: r.kind,
        payload: r.payload,
        attempts: r.attempts,
        max_attempts: r.max_attempts,
    }))
}

/// Seconds to wait before retry `attempt` (1-based): `2^attempt × 10`, capped at 6 h.
pub fn backoff_secs(attempt: i32) -> f64 {
    let exp = attempt.clamp(0, 20) as u32;
    (2f64.powi(exp as i32) * 10.0).min(6.0 * 3600.0)
}

async fn finish(
    state: &AppState,
    job: &Claimed,
    worker: &str,
    result: Result<(), JobError>,
) -> Result<(), sqlx::Error> {
    match result {
        Ok(()) => {
            sqlx::query!(
                r#"UPDATE jobs SET state = 'succeeded', finished_at = now(), locked_by = NULL, locked_until = NULL, last_error = NULL
                   WHERE id = $1 AND locked_by = $2"#,
                job.id,
                worker
            )
            .execute(&state.db)
            .await?;
        }
        Err(err) => {
            let (msg, fatal) = match err {
                JobError::Retry(m) => (m, false),
                JobError::Fatal(m) => (m, true),
            };
            let msg: String = msg.chars().take(2000).collect();
            let dead = fatal || job.attempts >= job.max_attempts;
            tracing::warn!(job_id = %job.id, kind = %job.kind, attempt = job.attempts, dead, error = %msg, "job failed");
            sqlx::query!(
                r#"UPDATE jobs
                   SET state = CASE WHEN $3 THEN 'dead' ELSE 'queued' END,
                       run_at = CASE WHEN $3 THEN run_at ELSE now() + make_interval(secs => $4) END,
                       finished_at = CASE WHEN $3 THEN now() ELSE NULL END,
                       last_error = $5, locked_by = NULL, locked_until = NULL
                   WHERE id = $1 AND locked_by = $2"#,
                job.id,
                worker,
                dead,
                backoff_secs(job.attempts),
                msg
            )
            .execute(&state.db)
            .await?;
        }
    }
    Ok(())
}

/// Runs one claimed job with a lease heartbeat. Returns `false` when nothing was queued.
pub async fn run_one(
    state: &AppState,
    registry: &Registry,
    worker: &str,
) -> Result<bool, sqlx::Error> {
    let Some(job) = claim(state, worker, &registry.kinds()).await? else {
        return Ok(false);
    };
    let Some(h) = registry.handlers.get(&job.kind).cloned() else {
        finish(
            state,
            &job,
            worker,
            Err(JobError::Fatal(format!("no handler for {}", job.kind))),
        )
        .await?;
        return Ok(true);
    };
    let ctx = JobContext {
        state: state.clone(),
        id: job.id,
        kind: job.kind.clone(),
        payload: job.payload.clone(),
        attempt: job.attempts,
    };
    let mut work = tokio::spawn(h(ctx));
    let mut beat = tokio::time::interval_at(tokio::time::Instant::now() + LEASE / 3, LEASE / 3);
    let result = loop {
        tokio::select! {
            res = &mut work => break match res {
                Ok(r) => r,
                Err(e) => Err(JobError::Retry(format!("handler panicked: {e}"))),
            },
            _ = beat.tick() => {
                sqlx::query!(
                    "UPDATE jobs SET locked_until = now() + make_interval(secs => $3) WHERE id = $1 AND locked_by = $2",
                    job.id, worker, LEASE.as_secs_f64()
                )
                .execute(&state.db)
                .await?;
            }
        }
    };
    finish(state, &job, worker, result).await?;
    Ok(true)
}

/// Requeues jobs whose worker died (lease expired); dead-letters those out of attempts.
pub async fn reap(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let r = sqlx::query!(
        r#"UPDATE jobs
           SET state = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
               finished_at = CASE WHEN attempts >= max_attempts THEN now() ELSE NULL END,
               last_error = COALESCE(last_error, 'lease expired'),
               locked_by = NULL, locked_until = NULL, run_at = now()
           WHERE state = 'running' AND locked_until < now()"#
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Enqueues each schedule at most once per period across all instances.
pub async fn schedule_tick(pool: &PgPool, schedules: &[Schedule]) -> Result<(), ApiError> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    for s in schedules {
        let period = s.period.as_secs().max(1) as i64;
        let bucket = now / period;
        let mut tx = pool.begin().await?;
        // Serialize schedulers of every instance for this kind.
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtext($1))",
            format!("schedule:{}", s.kind)
        )
        .execute(&mut *tx)
        .await?;
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND payload->>'schedule_bucket' = $2) AS "e!""#,
            s.kind,
            bucket.to_string()
        )
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            enqueue(
                &mut tx,
                s.kind,
                serde_json::json!({ "schedule_bucket": bucket.to_string() }),
                Enqueue {
                    max_attempts: Some(3),
                    ..Default::default()
                },
            )
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Runs workers, the reaper and the scheduler until shutdown.
pub async fn run_workers(state: AppState) {
    run_workers_with(state, Registry::standard(), SCHEDULES).await;
}

pub async fn run_workers_with(state: AppState, registry: Registry, schedules: &'static [Schedule]) {
    let mut tasks = Vec::new();
    for n in 0..state.config.worker_concurrency {
        let (state, registry) = (state.clone(), registry.clone());
        let worker = format!("{}#{n}", state.instance_id);
        tasks.push(tokio::spawn(async move {
            while !state.shutdown.is_cancelled() {
                let busy = match run_one(&state, &registry, &worker).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "job worker error");
                        false
                    }
                };
                if !busy {
                    tokio::select! {
                        _ = state.shutdown.cancelled() => {}
                        _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    }
                }
            }
        }));
    }
    let housekeeping = {
        let state = state.clone();
        tokio::spawn(async move {
            let mut reaper = tokio::time::interval(REAPER_INTERVAL);
            let mut sched = tokio::time::interval(SCHEDULER_INTERVAL);
            loop {
                tokio::select! {
                    _ = state.shutdown.cancelled() => return,
                    _ = reaper.tick() => {
                        if let Err(e) = reap(&state.db).await { tracing::warn!(error = %e, "job reaper error"); }
                    }
                    _ = sched.tick() => {
                        if let Err(e) = schedule_tick(&state.db, schedules).await { tracing::warn!(error = %e, "job scheduler error"); }
                    }
                }
            }
        })
    };
    for t in tasks {
        let _ = t.await;
    }
    let _ = housekeeping.await;
}
