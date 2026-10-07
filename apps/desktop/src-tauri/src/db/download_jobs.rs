//! Durable queue records. The launcher claims at most one active job through
//! this single SQLite thread; startup puts interrupted active jobs back in the
//! queue so their on-disk journals can be verified before work resumes.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::{Db, DbError, now_unix};
use crate::events::PackageRef;

#[derive(Debug, thiserror::Error)]
pub enum JobStoreError {
    #[error("this package already has a download job")]
    AlreadyQueued,
    #[error("this package is already installed")]
    AlreadyInstalled,
    #[error("download job was not found")]
    NotFound,
    #[error("download job is not in the required state")]
    InvalidState,
    #[error("stored download job has an invalid ID, kind, or state")]
    InvalidStoredJob,
    #[error("stored download options are invalid")]
    InvalidOptions(#[source] serde_json::Error),
    #[error("too many download jobs")]
    PositionOverflow,
    #[error("reorder must contain each waiting job exactly once")]
    InvalidOrder,
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Install,
    Update,
    Repair,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Repair => "repair",
        }
    }

    fn parse(value: &str) -> Result<Self, JobStoreError> {
        match value {
            "install" => Ok(Self::Install),
            "update" => Ok(Self::Update),
            "repair" => Ok(Self::Repair),
            _ => Err(JobStoreError::InvalidStoredJob),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Active,
    Paused,
    Failed,
}

/// Allowed durable queue changes. The worker must stop before recording an
/// active pause or failure, so another worker cannot claim the same package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobTransition {
    PauseQueued,
    PauseActive {
        reason: String,
    },
    Resume,
    Retry,
    FailActive {
        error: String,
    },
    /// Back in the queue without being paused (checkpoint before an update).
    RequeueActive,
}

impl JobTransition {
    fn states(&self) -> (&'static str, &'static str, Option<&str>) {
        match self {
            Self::PauseQueued => ("queued", "paused", Some("user")),
            Self::PauseActive { reason } => ("active", "paused", Some(reason)),
            Self::Resume => ("paused", "queued", None),
            Self::Retry => ("failed", "queued", None),
            Self::FailActive { error } => ("active", "failed", Some(error)),
            Self::RequeueActive => ("active", "queued", None),
        }
    }
}

impl JobState {
    fn parse(value: &str) -> Result<Self, JobStoreError> {
        match value {
            "queued" => Ok(Self::Queued),
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "failed" => Ok(Self::Failed),
            _ => Err(JobStoreError::InvalidStoredJob),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: Uuid,
    pub package: PackageRef,
    pub version_id: Uuid,
    pub library_id: Uuid,
    pub kind: JobKind,
    pub state: JobState,
    pub position: i64,
    pub options: JobOptions,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Last recorded progress (live progress comes from events).
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Only non-secret decisions needed to resume a job after a restart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobOptions {
    #[serde(default)]
    pub allow_in_place: bool,
}

struct StoredJob {
    id: String,
    server_id: String,
    package_id: String,
    version_id: String,
    library_id: String,
    kind: String,
    state: String,
    position: i64,
    options: String,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    bytes_done: i64,
    bytes_total: i64,
}

fn decode(row: StoredJob) -> Result<Job, JobStoreError> {
    Ok(Job {
        id: Uuid::parse_str(&row.id).map_err(|_| JobStoreError::InvalidStoredJob)?,
        package: PackageRef {
            server_id: Uuid::parse_str(&row.server_id)
                .map_err(|_| JobStoreError::InvalidStoredJob)?,
            package_id: Uuid::parse_str(&row.package_id)
                .map_err(|_| JobStoreError::InvalidStoredJob)?,
        },
        version_id: Uuid::parse_str(&row.version_id)
            .map_err(|_| JobStoreError::InvalidStoredJob)?,
        library_id: Uuid::parse_str(&row.library_id)
            .map_err(|_| JobStoreError::InvalidStoredJob)?,
        kind: JobKind::parse(&row.kind)?,
        state: JobState::parse(&row.state)?,
        position: row.position,
        options: serde_json::from_str(&row.options).map_err(JobStoreError::InvalidOptions)?,
        error: row.error,
        created_at: row.created_at,
        updated_at: row.updated_at,
        bytes_done: u64::try_from(row.bytes_done).unwrap_or(0),
        bytes_total: u64::try_from(row.bytes_total).unwrap_or(0),
    })
}

fn read_all(conn: &rusqlite::Connection) -> Result<Vec<Job>, JobStoreError> {
    let mut query = conn.prepare(
        "SELECT id, server_id, package_id, version_id, library_id, kind, state,
                position, options, error, created_at, updated_at, bytes_done, bytes_total
         FROM download_jobs
         ORDER BY (state != 'active'), position, created_at, id",
    )?;
    let rows = query.query_map([], |row| {
        Ok(StoredJob {
            id: row.get(0)?,
            server_id: row.get(1)?,
            package_id: row.get(2)?,
            version_id: row.get(3)?,
            library_id: row.get(4)?,
            kind: row.get(5)?,
            state: row.get(6)?,
            position: row.get(7)?,
            options: row.get(8)?,
            error: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            bytes_done: row.get(12)?,
            bytes_total: row.get(13)?,
        })
    })?;
    rows.map(|row| decode(row?)).collect()
}

pub async fn list(db: &Db) -> Result<Vec<Job>, JobStoreError> {
    db.call(|conn| Ok(read_all(conn))).await?
}

/// Queues one package, rejecting a second job for the same server and package.
pub async fn enqueue(
    db: &Db,
    package: PackageRef,
    version_id: Uuid,
    library_id: Uuid,
    kind: JobKind,
    options: JobOptions,
) -> Result<Job, JobStoreError> {
    let options_json = serde_json::to_string(&options).map_err(JobStoreError::InvalidOptions)?;
    db.call(move |conn| {
        Ok((|| -> Result<Job, JobStoreError> {
            let tx = conn.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM download_jobs WHERE server_id = ?1 AND package_id = ?2)",
                params![package.server_id.to_string(), package.package_id.to_string()],
                |row| row.get(0),
            )?;
            if exists {
                return Err(JobStoreError::AlreadyQueued);
            }
            let max: Option<i64> = tx.query_row(
                "SELECT max(position) FROM download_jobs", [], |row| row.get(0),
            )?;
            let position = max.unwrap_or(-1).checked_add(1).ok_or(JobStoreError::PositionOverflow)?;
            let id = Uuid::now_v7();
            let now = now_unix();
            tx.execute(
                "INSERT INTO download_jobs (id, server_id, package_id, version_id, library_id,
                 kind, state, position, options, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, ?9)",
                params![id.to_string(), package.server_id.to_string(), package.package_id.to_string(),
                    version_id.to_string(), library_id.to_string(), kind.as_str(), position, options_json, now],
            )?;
            tx.commit()?;
            Ok(Job {
                id, package, version_id, library_id, kind, state: JobState::Queued,
                position, options,
                error: None, created_at: now, updated_at: now,
                bytes_done: 0, bytes_total: 0,
            })
        })())
    })
    .await?
}

/// Atomically claims the first queued job while fewer than `max_active` jobs
/// are active (the download setting allows 1–3).
pub async fn claim_next(db: &Db, max_active: usize) -> Result<Option<Job>, JobStoreError> {
    let max_active = i64::try_from(max_active).unwrap_or(i64::MAX);
    db.call(move |conn| {
        Ok((|| -> Result<Option<Job>, JobStoreError> {
            let tx = conn.transaction()?;
            let active: i64 = tx.query_row(
                "SELECT count(*) FROM download_jobs WHERE state = 'active'", [], |row| row.get(0),
            )?;
            if active >= max_active {
                return Ok(None);
            }
            let id: Option<String> = tx.query_row(
                "SELECT id FROM download_jobs WHERE state = 'queued' ORDER BY position, created_at, id LIMIT 1",
                [], |row| row.get(0),
            ).optional()?;
            let Some(id) = id else { return Ok(None); };
            tx.execute(
                "UPDATE download_jobs SET state = 'active', updated_at = ?2 WHERE id = ?1",
                params![id, now_unix()],
            )?;
            tx.commit()?;
            read_all(conn)?.into_iter().find(|job| job.id.to_string() == id).map(Some).ok_or(JobStoreError::NotFound)
        })())
    })
    .await?
}

/// Startup recovery: no worker survives a process restart, so the old active
/// job is queued again and its transfer journal can be checked by the worker.
pub async fn recover_active(db: &Db) -> Result<u64, JobStoreError> {
    db.call(|conn| {
        Ok((|| -> Result<u64, JobStoreError> {
            Ok(conn.execute(
                "UPDATE download_jobs SET state = 'queued', updated_at = ?1 WHERE state = 'active'",
                [now_unix()],
            )? as u64)
        })())
    })
    .await?
}

/// Changes a job only from the state required by the requested transition.
/// A paused or failed job keeps its queue position across resume and retry.
pub async fn transition(
    db: &Db,
    package: PackageRef,
    action: JobTransition,
) -> Result<(), JobStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), JobStoreError> {
            let (from, to, reason) = action.states();
            let changed = conn.execute(
                "UPDATE download_jobs SET state = ?3, error = ?4, updated_at = ?5
                 WHERE server_id = ?1 AND package_id = ?2 AND state = ?6",
                params![package.server_id.to_string(), package.package_id.to_string(),
                    to, reason, now_unix(), from],
            )?;
            if changed == 1 {
                return Ok(());
            }
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM download_jobs WHERE server_id = ?1 AND package_id = ?2)",
                params![package.server_id.to_string(), package.package_id.to_string()],
                |row| row.get(0),
            )?;
            Err(if exists { JobStoreError::InvalidState } else { JobStoreError::NotFound })
        })())
    })
    .await?
}

/// Removes only a waiting job. Active work must first be cancelled and joined.
pub async fn remove_waiting(db: &Db, package: PackageRef) -> Result<(), JobStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), JobStoreError> {
            let changed = conn.execute(
                "DELETE FROM download_jobs WHERE server_id = ?1 AND package_id = ?2 AND state != 'active'",
                params![package.server_id.to_string(), package.package_id.to_string()],
            )?;
            if changed == 1 {
                return Ok(());
            }
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM download_jobs WHERE server_id = ?1 AND package_id = ?2)",
                params![package.server_id.to_string(), package.package_id.to_string()],
                |row| row.get(0),
            )?;
            Err(if exists { JobStoreError::InvalidState } else { JobStoreError::NotFound })
        })())
    })
    .await?
}

/// Reorders all waiting jobs. The active job keeps its place and state; the
/// submitted list must contain every queued, paused, and failed job once.
pub async fn reorder(db: &Db, packages: Vec<PackageRef>) -> Result<(), JobStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), JobStoreError> {
            let tx = conn.transaction()?;
            let waiting: Vec<_> = read_all(&tx)?
                .into_iter()
                .filter(|job| job.state != JobState::Active)
                .collect();
            if waiting.len() != packages.len() {
                return Err(JobStoreError::InvalidOrder);
            }
            let positions: Vec<_> = waiting.iter().map(|job| job.position).collect();
            let ids: HashMap<_, _> = waiting.iter().map(|job| (job.package, job.id)).collect();
            let mut seen = HashSet::new();
            for (package, position) in packages.into_iter().zip(positions) {
                let Some(id) = ids.get(&package) else {
                    return Err(JobStoreError::InvalidOrder);
                };
                if !seen.insert(package) {
                    return Err(JobStoreError::InvalidOrder);
                }
                tx.execute(
                    "UPDATE download_jobs SET position = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.to_string(), position, now_unix()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })())
    })
    .await?
}

/// What `install_start` records about a new install (INS-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInstall {
    pub package: PackageRef,
    pub library_id: Uuid,
    /// `<slug>` or `<slug>-N`, free in the library.
    pub dir_name: String,
    pub version_id: Uuid,
    pub sequence: i64,
    pub platform: String,
    pub size_bytes: i64,
    pub title: String,
    pub slug: String,
    pub version_label: String,
    pub cover_asset_id: Option<Uuid>,
}

/// Registers an install (`installing`) and queues its job, atomically: a
/// package is never half-registered.
pub async fn begin_install(db: &Db, new: NewInstall) -> Result<Job, JobStoreError> {
    let options_json =
        serde_json::to_string(&JobOptions::default()).map_err(JobStoreError::InvalidOptions)?;
    db.call(move |conn| {
        Ok((|| -> Result<Job, JobStoreError> {
            let tx = conn.transaction()?;
            let package = new.package;
            let keys = params![package.server_id.to_string(), package.package_id.to_string()];
            let installed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM installs WHERE server_id = ?1 AND package_id = ?2)",
                keys, |row| row.get(0),
            )?;
            if installed {
                return Err(JobStoreError::AlreadyInstalled);
            }
            let queued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM download_jobs WHERE server_id = ?1 AND package_id = ?2)",
                keys, |row| row.get(0),
            )?;
            if queued {
                return Err(JobStoreError::AlreadyQueued);
            }
            tx.execute(
                "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
                 sequence, platform, state, size_bytes, title, slug, version_label, cover_asset_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'installing', ?8, ?9, ?10, ?11, ?12)",
                params![package.server_id.to_string(), package.package_id.to_string(),
                    new.library_id.to_string(), new.dir_name, new.version_id.to_string(), new.sequence,
                    new.platform, new.size_bytes, new.title, new.slug, new.version_label,
                    new.cover_asset_id.map(|id| id.to_string())],
            )?;
            let max: Option<i64> = tx.query_row(
                "SELECT max(position) FROM download_jobs", [], |row| row.get(0),
            )?;
            let position = max.unwrap_or(-1).checked_add(1).ok_or(JobStoreError::PositionOverflow)?;
            let id = Uuid::now_v7();
            let now = now_unix();
            let bytes_total = new.size_bytes.max(0);
            tx.execute(
                "INSERT INTO download_jobs (id, server_id, package_id, version_id, library_id,
                 kind, state, position, options, created_at, updated_at, bytes_total)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'install', 'queued', ?6, ?7, ?8, ?8, ?9)",
                params![id.to_string(), package.server_id.to_string(), package.package_id.to_string(),
                    new.version_id.to_string(), new.library_id.to_string(), position, options_json,
                    now, bytes_total],
            )?;
            tx.commit()?;
            Ok(Job {
                id, package, version_id: new.version_id, library_id: new.library_id,
                kind: JobKind::Install, state: JobState::Queued, position,
                options: JobOptions::default(), error: None, created_at: now, updated_at: now,
                bytes_done: 0, bytes_total: u64::try_from(bytes_total).unwrap_or(0),
            })
        })())
    })
    .await?
}

/// Records the last known progress of a job.
pub async fn set_progress(
    db: &Db,
    package: PackageRef,
    bytes_done: u64,
    bytes_total: u64,
) -> Result<(), JobStoreError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE download_jobs SET bytes_done = ?3, bytes_total = ?4
             WHERE server_id = ?1 AND package_id = ?2",
            params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                i64::try_from(bytes_done).unwrap_or(i64::MAX),
                i64::try_from(bytes_total).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(())
    })
    .await?;
    Ok(())
}

/// Finished jobs kept in the history.
pub const HISTORY_LIMIT: i64 = 100;

/// A finished install, update or repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub package: PackageRef,
    pub kind: JobKind,
    pub title: String,
    pub version_label: String,
    pub bytes_total: u64,
    pub finished_at: i64,
    /// JSON of the outcome the UI shows (`InstallOutcome`).
    pub outcome: String,
}

/// Ends a job, whatever its state: removes it from the queue and records it
/// in the history (keeping the newest [`HISTORY_LIMIT`]), atomically.
pub async fn finish(
    db: &Db,
    package: PackageRef,
    title: String,
    version_label: String,
    outcome_json: String,
) -> Result<HistoryEntry, JobStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<HistoryEntry, JobStoreError> {
            let tx = conn.transaction()?;
            let keys = params![package.server_id.to_string(), package.package_id.to_string()];
            let found: Option<(String, i64)> = tx.query_row(
                "SELECT kind, bytes_total FROM download_jobs WHERE server_id = ?1 AND package_id = ?2",
                keys, |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((kind, bytes_total)) = found else {
                return Err(JobStoreError::NotFound);
            };
            tx.execute("DELETE FROM download_jobs WHERE server_id = ?1 AND package_id = ?2", keys)?;
            let entry = HistoryEntry {
                id: Uuid::now_v7(),
                package,
                kind: JobKind::parse(&kind)?,
                title,
                version_label,
                bytes_total: u64::try_from(bytes_total).unwrap_or(0),
                finished_at: now_unix(),
                outcome: outcome_json,
            };
            tx.execute(
                "INSERT INTO download_history (id, server_id, package_id, kind, title, version_label,
                 bytes_total, finished_at, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![entry.id.to_string(), package.server_id.to_string(),
                    package.package_id.to_string(), kind, entry.title, entry.version_label,
                    bytes_total, entry.finished_at, entry.outcome],
            )?;
            tx.execute(
                "DELETE FROM download_history WHERE id NOT IN (
                     SELECT id FROM download_history ORDER BY finished_at DESC, id DESC LIMIT ?1)",
                [HISTORY_LIMIT],
            )?;
            tx.commit()?;
            Ok(entry)
        })())
    })
    .await?
}

/// The history, newest first.
pub async fn history(db: &Db) -> Result<Vec<HistoryEntry>, JobStoreError> {
    db.call(|conn| {
        Ok((|| -> Result<Vec<HistoryEntry>, JobStoreError> {
            let mut query = conn.prepare(
                "SELECT id, server_id, package_id, kind, title, version_label, bytes_total,
                        finished_at, outcome
                 FROM download_history ORDER BY finished_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = query.query_map([HISTORY_LIMIT], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;
            let parse =
                |text: &str| Uuid::parse_str(text).map_err(|_| JobStoreError::InvalidStoredJob);
            rows.map(|row| {
                let (
                    id,
                    server,
                    package,
                    kind,
                    title,
                    version_label,
                    bytes_total,
                    finished_at,
                    outcome,
                ) = row?;
                Ok(HistoryEntry {
                    id: parse(&id)?,
                    package: PackageRef {
                        server_id: parse(&server)?,
                        package_id: parse(&package)?,
                    },
                    kind: JobKind::parse(&kind)?,
                    title,
                    version_label,
                    bytes_total: u64::try_from(bytes_total).unwrap_or(0),
                    finished_at,
                    outcome,
                })
            })
            .collect()
        })())
    })
    .await?
}

pub async fn clear_history(db: &Db) -> Result<(), JobStoreError> {
    db.call(|conn| {
        conn.execute("DELETE FROM download_history", [])?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::libraries;

    async fn setup() -> (Db, tempfile::TempDir, Uuid, Uuid) {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let library = libraries::add(&db, dir.path(), "Games").await.unwrap();
        let server_id = Uuid::now_v7();
        let id = server_id.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        (db, dir, server_id, library.root.id)
    }

    fn package(server_id: Uuid) -> PackageRef {
        PackageRef {
            server_id,
            package_id: Uuid::now_v7(),
        }
    }

    #[tokio::test]
    async fn duplicate_package_is_refused_and_options_survive_restart() {
        let (db, dir, server_id, library_id) = setup().await;
        let package = package(server_id);
        let options = JobOptions {
            allow_in_place: false,
        };
        let job = enqueue(
            &db,
            package,
            Uuid::now_v7(),
            library_id,
            JobKind::Install,
            options,
        )
        .await
        .unwrap();
        assert_eq!(job.options, options);
        assert!(matches!(
            enqueue(
                &db,
                package,
                Uuid::now_v7(),
                library_id,
                JobKind::Update,
                JobOptions::default()
            )
            .await,
            Err(JobStoreError::AlreadyQueued)
        ));
        drop(db);
        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let jobs = list(&db).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job.id);
        assert_eq!(jobs[0].options, options);
        let id = job.id.to_string();
        db.call(move |conn| {
            conn.execute(
                "UPDATE download_jobs SET options = '{\"unexpected\":true}' WHERE id = ?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(matches!(
            list(&db).await,
            Err(JobStoreError::InvalidOptions(_))
        ));
    }

    #[tokio::test]
    async fn only_one_worker_can_claim_and_recovery_requeues_it() {
        let (db, _dir, server_id, library_id) = setup().await;
        let first = enqueue(
            &db,
            package(server_id),
            Uuid::now_v7(),
            library_id,
            JobKind::Install,
            JobOptions::default(),
        )
        .await
        .unwrap();
        enqueue(
            &db,
            package(server_id),
            Uuid::now_v7(),
            library_id,
            JobKind::Repair,
            JobOptions::default(),
        )
        .await
        .unwrap();
        let (a, b) = tokio::join!(claim_next(&db, 1), claim_next(&db, 1));
        let claimed = a.unwrap().or(b.unwrap()).unwrap();
        assert_eq!(claimed.id, first.id);
        assert_eq!(claimed.state, JobState::Active);
        assert_eq!(list(&db).await.unwrap()[0].id, first.id);
        assert_eq!(recover_active(&db).await.unwrap(), 1);
        assert_eq!(recover_active(&db).await.unwrap(), 0);
        assert_eq!(claim_next(&db, 1).await.unwrap().unwrap().id, first.id);
    }

    #[tokio::test]
    async fn reorder_is_atomic_and_keeps_the_active_job_running() {
        let (db, _dir, server_id, library_id) = setup().await;
        let mut packages = Vec::new();
        for _ in 0..4 {
            let reference = package(server_id);
            enqueue(
                &db,
                reference,
                Uuid::now_v7(),
                library_id,
                JobKind::Install,
                JobOptions::default(),
            )
            .await
            .unwrap();
            packages.push(reference);
        }
        let active = claim_next(&db, 1).await.unwrap().unwrap();
        db.call(|conn| {
            conn.execute(
                "UPDATE download_jobs SET state = 'paused' WHERE position = 2",
                [],
            )?;
            conn.execute(
                "UPDATE download_jobs SET state = 'failed' WHERE position = 3",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let before = list(&db).await.unwrap();
        assert!(matches!(
            reorder(&db, vec![packages[3], packages[3], packages[1]]).await,
            Err(JobStoreError::InvalidOrder)
        ));
        assert!(matches!(
            reorder(&db, vec![packages[3], packages[2]]).await,
            Err(JobStoreError::InvalidOrder)
        ));
        assert!(matches!(
            reorder(&db, vec![packages[3], packages[2], package(server_id)]).await,
            Err(JobStoreError::InvalidOrder)
        ));
        assert_eq!(list(&db).await.unwrap(), before);

        reorder(&db, vec![packages[3], packages[2], packages[1]])
            .await
            .unwrap();
        let after = list(&db).await.unwrap();
        assert_eq!(after[0].id, active.id);
        assert_eq!(after[0].state, JobState::Active);
        assert_eq!(
            after[1..].iter().map(|job| job.package).collect::<Vec<_>>(),
            vec![packages[3], packages[2], packages[1]]
        );
        assert_eq!(after[1].state, JobState::Failed);
        assert_eq!(after[2].state, JobState::Paused);
        assert_eq!(after[3].state, JobState::Queued);
    }

    #[tokio::test]
    async fn transitions_preserve_order_and_refuse_active_removal() {
        let (db, dir, server_id, library_id) = setup().await;
        let first = package(server_id);
        let second = package(server_id);
        let missing = package(server_id);
        for reference in [first, second] {
            enqueue(
                &db,
                reference,
                Uuid::now_v7(),
                library_id,
                JobKind::Install,
                JobOptions::default(),
            )
            .await
            .unwrap();
        }
        assert!(matches!(
            transition(&db, missing, JobTransition::PauseQueued).await,
            Err(JobStoreError::NotFound)
        ));
        transition(&db, second, JobTransition::PauseQueued)
            .await
            .unwrap();
        assert!(matches!(
            transition(&db, second, JobTransition::Retry).await,
            Err(JobStoreError::InvalidState)
        ));
        let active = claim_next(&db, 1).await.unwrap().unwrap();
        assert_eq!(active.package, first);
        assert!(matches!(
            remove_waiting(&db, first).await,
            Err(JobStoreError::InvalidState)
        ));
        transition(
            &db,
            first,
            JobTransition::FailActive {
                error: "integrity".into(),
            },
        )
        .await
        .unwrap();
        transition(&db, first, JobTransition::Retry).await.unwrap();
        transition(&db, second, JobTransition::Resume)
            .await
            .unwrap();
        drop(db);

        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let jobs = list(&db).await.unwrap();
        assert_eq!(
            jobs.iter().map(|job| job.package).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(
            jobs.iter()
                .all(|job| job.state == JobState::Queued && job.error.is_none())
        );
        assert_eq!(claim_next(&db, 1).await.unwrap().unwrap().package, first);
        transition(
            &db,
            first,
            JobTransition::PauseActive {
                reason: "disk_full".into(),
            },
        )
        .await
        .unwrap();
        let paused = list(&db).await.unwrap();
        assert_eq!(paused[0].state, JobState::Paused);
        assert_eq!(paused[0].error.as_deref(), Some("disk_full"));
        assert_eq!(claim_next(&db, 1).await.unwrap().unwrap().package, second);
        remove_waiting(&db, first).await.unwrap();
        assert!(matches!(
            remove_waiting(&db, first).await,
            Err(JobStoreError::NotFound)
        ));
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn foreign_keys_prevent_queuing_into_an_unregistered_library() {
        let (db, _dir, server_id, _library_id) = setup().await;
        assert!(matches!(
            enqueue(
                &db,
                package(server_id),
                Uuid::now_v7(),
                Uuid::now_v7(),
                JobKind::Install,
                JobOptions::default()
            )
            .await,
            Err(JobStoreError::Sqlite(rusqlite::Error::SqliteFailure(_, _)))
        ));
        assert!(list(&db).await.unwrap().is_empty());
    }

    fn new_install(package: PackageRef, library_id: Uuid) -> NewInstall {
        NewInstall {
            package,
            library_id,
            dir_name: "garden".into(),
            version_id: Uuid::now_v7(),
            sequence: 3,
            platform: "linux-x86_64".into(),
            size_bytes: 5_000,
            title: "Gilded Garden".into(),
            slug: "garden".into(),
            version_label: "1.3".into(),
            cover_asset_id: None,
        }
    }

    #[tokio::test]
    async fn begin_install_registers_the_install_and_its_job_once() {
        let (db, _dir, server_id, library_id) = setup().await;
        let package = package(server_id);
        let job = begin_install(&db, new_install(package, library_id))
            .await
            .unwrap();
        assert_eq!(
            (job.kind, job.state, job.bytes_total),
            (JobKind::Install, JobState::Queued, 5_000)
        );
        let info = crate::db::installs::catalog_info(&db, package)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (info.title.as_str(), info.version_label.as_str()),
            ("Gilded Garden", "1.3")
        );
        let row = crate::db::installs::row(&db, package)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "installing");
        assert!(matches!(
            begin_install(&db, new_install(package, library_id)).await,
            Err(JobStoreError::AlreadyInstalled)
        ));
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn claims_respect_the_concurrency_setting() {
        let (db, _dir, server_id, library_id) = setup().await;
        for _ in 0..4 {
            enqueue(
                &db,
                package(server_id),
                Uuid::now_v7(),
                library_id,
                JobKind::Install,
                JobOptions::default(),
            )
            .await
            .unwrap();
        }
        assert!(claim_next(&db, 2).await.unwrap().is_some());
        assert!(claim_next(&db, 2).await.unwrap().is_some());
        assert!(claim_next(&db, 2).await.unwrap().is_none());
        assert!(claim_next(&db, 3).await.unwrap().is_some());
        assert!(claim_next(&db, 1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn finished_jobs_move_to_a_bounded_history() {
        let (db, _dir, server_id, library_id) = setup().await;
        let first = package(server_id);
        begin_install(&db, new_install(first, library_id))
            .await
            .unwrap();
        set_progress(&db, first, 2_000, 5_000).await.unwrap();
        assert_eq!(list(&db).await.unwrap()[0].bytes_done, 2_000);
        let entry = finish(
            &db,
            first,
            "Gilded Garden".into(),
            "1.3".into(),
            r#"{"kind":"installed"}"#.into(),
        )
        .await
        .unwrap();
        assert!(list(&db).await.unwrap().is_empty());
        assert_eq!(history(&db).await.unwrap(), vec![entry]);
        assert!(matches!(
            finish(&db, first, String::new(), String::new(), "{}".into()).await,
            Err(JobStoreError::NotFound)
        ));
        for _ in 0..HISTORY_LIMIT + 5 {
            let other = package(server_id);
            enqueue(
                &db,
                other,
                Uuid::now_v7(),
                library_id,
                JobKind::Repair,
                JobOptions::default(),
            )
            .await
            .unwrap();
            finish(
                &db,
                other,
                "Other".into(),
                "2".into(),
                r#"{"kind":"installed"}"#.into(),
            )
            .await
            .unwrap();
        }
        let kept = history(&db).await.unwrap();
        assert_eq!(kept.len() as i64, HISTORY_LIMIT);
        assert!(
            kept.windows(2)
                .all(|w| (w[0].finished_at, w[0].id) >= (w[1].finished_at, w[1].id))
        );
        assert_eq!(kept[0].kind, JobKind::Repair);
        clear_history(&db).await.unwrap();
        assert!(history(&db).await.unwrap().is_empty());
    }
}
