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
    #[error("download job was not found")]
    NotFound,
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
    fn as_str(self) -> &'static str {
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
    })
}

fn read_all(conn: &rusqlite::Connection) -> Result<Vec<Job>, JobStoreError> {
    let mut query = conn.prepare(
        "SELECT id, server_id, package_id, version_id, library_id, kind, state,
                position, options, error, created_at, updated_at
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
            })
        })())
    })
    .await?
}

/// Atomically claims the first queued job, unless another one is active.
pub async fn claim_next(db: &Db) -> Result<Option<Job>, JobStoreError> {
    db.call(|conn| {
        Ok((|| -> Result<Option<Job>, JobStoreError> {
            let tx = conn.transaction()?;
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM download_jobs WHERE state = 'active')", [], |row| row.get(0),
            )?;
            if active {
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
        let (a, b) = tokio::join!(claim_next(&db), claim_next(&db));
        let claimed = a.unwrap().or(b.unwrap()).unwrap();
        assert_eq!(claimed.id, first.id);
        assert_eq!(claimed.state, JobState::Active);
        assert_eq!(list(&db).await.unwrap()[0].id, first.id);
        assert_eq!(recover_active(&db).await.unwrap(), 1);
        assert_eq!(recover_active(&db).await.unwrap(), 0);
        assert_eq!(claim_next(&db).await.unwrap().unwrap().id, first.id);
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
        let active = claim_next(&db).await.unwrap().unwrap();
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
}
