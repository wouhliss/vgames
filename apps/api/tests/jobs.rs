//! A1-T06: Postgres job queue.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::jobs::{self, Enqueue, JobError, Registry, Schedule};

fn recording_registry(seen: Arc<Mutex<HashMap<Uuid, u32>>>) -> Registry {
    let mut r = Registry::default();
    r.register(
        "test.record",
        jobs::handler(move |ctx| {
            let seen = seen.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(2)).await;
                *seen.lock().unwrap().entry(ctx.id).or_default() += 1;
                Ok(())
            }
        }),
    );
    r.register(
        "test.retry",
        jobs::handler(|_| async { Err(JobError::Retry("try later".into())) }),
    );
    r.register(
        "test.fatal",
        jobs::handler(|_| async { Err(JobError::Fatal("broken input".into())) }),
    );
    r
}

async fn job_row(pool: &PgPool, id: Uuid) -> (String, i32, f64, Option<String>) {
    sqlx::query_as("SELECT state, attempts, EXTRACT(EPOCH FROM run_at - now())::float8, last_error FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn two_workers_never_process_a_job_twice(pool: PgPool) {
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let registry = recording_registry(seen.clone());
    for i in 0..60 {
        jobs::enqueue_now(&pool, "test.record", json!({ "i": i }), Enqueue::default())
            .await
            .unwrap()
            .unwrap();
    }
    let a = state(pool.clone());
    let b = state(pool.clone());
    let mut tasks = Vec::new();
    for (st, name) in [(a, "a"), (b, "b"), (state(pool.clone()), "c")] {
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            while jobs::run_one(&st, &registry, &format!("worker-{name}"))
                .await
                .unwrap()
            {}
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let counts = seen.lock().unwrap().clone();
    assert_eq!(counts.len(), 60);
    assert!(counts.values().all(|n| *n == 1), "a job ran twice: {counts:?}");
    let done: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state = 'succeeded'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(done, 60);
}

#[sqlx::test(migrations = "./migrations")]
async fn failures_back_off_then_dead_letter(pool: PgPool) {
    let st = state(pool.clone());
    let registry = recording_registry(Arc::default());
    let id = jobs::enqueue_now(
        &pool,
        "test.retry",
        json!({}),
        Enqueue {
            max_attempts: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert!(jobs::run_one(&st, &registry, "w").await.unwrap());
    let (state_, attempts, wait, err) = job_row(&pool, id).await;
    assert_eq!((state_.as_str(), attempts), ("queued", 1));
    assert!(
        (15.0..=21.0).contains(&wait),
        "first retry after ~20 s, got {wait}"
    );
    assert_eq!(err.as_deref(), Some("try later"));
    // Not claimable before its run_at.
    assert!(!jobs::run_one(&st, &registry, "w").await.unwrap());

    sqlx::query("UPDATE jobs SET run_at = now() WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(jobs::run_one(&st, &registry, "w").await.unwrap());
    let (state_, attempts, _, _) = job_row(&pool, id).await;
    assert_eq!((state_.as_str(), attempts), ("dead", 2));

    let fatal = jobs::enqueue_now(&pool, "test.fatal", json!({}), Enqueue::default())
        .await
        .unwrap()
        .unwrap();
    assert!(jobs::run_one(&st, &registry, "w").await.unwrap());
    assert_eq!(job_row(&pool, fatal).await.0, "dead");

    assert_eq!(jobs::backoff_secs(1), 20.0);
    assert_eq!(jobs::backoff_secs(3), 80.0);
    assert_eq!(jobs::backoff_secs(40), 6.0 * 3600.0);
}

#[sqlx::test(migrations = "./migrations")]
async fn expired_leases_are_recovered(pool: PgPool) {
    let st = state(pool.clone());
    let seen = Arc::new(Mutex::new(HashMap::new()));
    let registry = recording_registry(seen.clone());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO jobs (kind, state, attempts, locked_by, locked_until) VALUES ('test.record', 'running', 1, 'crashed', now() - interval '1 second') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let out_of_attempts: Uuid = sqlx::query_scalar(
        "INSERT INTO jobs (kind, state, attempts, max_attempts, locked_by, locked_until) VALUES ('test.record', 'running', 5, 5, 'crashed', now() - interval '1 second') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // A live lease is left alone.
    sqlx::query("INSERT INTO jobs (kind, state, attempts, locked_by, locked_until) VALUES ('test.record', 'running', 1, 'alive', now() + interval '5 minutes')")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(jobs::reap(&pool).await.unwrap(), 2);
    assert_eq!(job_row(&pool, out_of_attempts).await.0, "dead");
    assert!(jobs::run_one(&st, &registry, "w").await.unwrap());
    assert_eq!(job_row(&pool, id).await.0, "succeeded");
    assert_eq!(seen.lock().unwrap().get(&id), Some(&1));
}

#[sqlx::test(migrations = "./migrations")]
async fn dedupe_keys_hold_one_pending_job(pool: PgPool) {
    let st = state(pool.clone());
    let registry = recording_registry(Arc::default());
    let opts = || Enqueue {
        dedupe_key: Some("metadata.fetch:pkg-1".into()),
        ..Default::default()
    };
    assert!(
        jobs::enqueue_now(&pool, "test.record", json!({}), opts())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        jobs::enqueue_now(&pool, "test.record", json!({}), opts())
            .await
            .unwrap()
            .is_none()
    );
    assert!(jobs::run_one(&st, &registry, "w").await.unwrap());
    // Once finished, the key is free again.
    assert!(
        jobs::enqueue_now(&pool, "test.record", json!({}), opts())
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn schedules_enqueue_once_per_period_across_instances(pool: PgPool) {
    static SCHED: &[Schedule] = &[Schedule {
        kind: "test.record",
        period: Duration::from_secs(3600),
    }];
    let mut ticks = Vec::new();
    for _ in 0..6 {
        let pool = pool.clone();
        ticks.push(tokio::spawn(async move {
            jobs::schedule_tick(&pool, SCHED).await.unwrap()
        }));
    }
    for t in ticks {
        t.await.unwrap();
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'test.record'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Still one after the job ran.
    let st = state(pool.clone());
    assert!(
        jobs::run_one(&st, &recording_registry(Arc::default()), "w")
            .await
            .unwrap()
    );
    jobs::schedule_tick(&pool, SCHED).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'test.record'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn unknown_kinds_are_not_claimed(pool: PgPool) {
    let st = state(pool.clone());
    jobs::enqueue_now(&pool, "someone.else", json!({}), Enqueue::default())
        .await
        .unwrap();
    assert!(
        !jobs::run_one(&st, &recording_registry(Arc::default()), "w")
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn sweep_removes_expired_rows(pool: PgPool) {
    let (_, session, _) = seed_session(&pool, "user").await;
    sqlx::query("INSERT INTO oauth_flows (state_hash, client_kind, expires_at) VALUES ($1, 'web', now() - interval '2 days')")
        .bind(vec![1u8; 32])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO realtime_tickets (digest, session_id, expires_at) VALUES ($1, $2, now() - interval '1 minute')")
        .bind(vec![2u8; 32])
        .bind(session)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE sessions SET revoked_at = now() - interval '31 days', revoked_reason = 'logout'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let st = state(pool.clone());
    jobs::enqueue_now(&pool, "sweep.expired", json!({}), Enqueue::default())
        .await
        .unwrap();
    assert!(
        jobs::run_one(&st, &Registry::standard(), "w")
            .await
            .unwrap()
    );
    for table in ["oauth_flows", "realtime_tickets", "sessions"] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}
