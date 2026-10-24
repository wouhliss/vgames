//! Account-scoped sync bases and durable snapshot retry identities (PLAY-06).
//! Access through the database thread; no filesystem or network work here.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_proto::saves::{CommitSnapshotRequest, SaveFile};

use super::{Db, DbError, now_unix};
use crate::events::PackageRef;

/// Constructed by native authenticated code, never from a WebView user id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    pub package: PackageRef,
    pub user_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOperation {
    pub id: Uuid,
    pub request: CommitSnapshotRequest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncState {
    pub base_snapshot_id: Option<Uuid>,
    pub files: BTreeMap<String, SaveFile>,
    pub pending: bool,
    pub last_synced_at: Option<i64>,
    pub push: Option<PushOperation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullDecision {
    Unchanged,
    LocalProgress,
    Restore,
    Conflict,
}

/// Content equality ignores mtime-only changes. An absent head compares equal
/// only to an absent base; scanning still refreshes the metadata cache.
pub fn decide_pull(
    base: &SyncState,
    local: &BTreeMap<String, SaveFile>,
    head: Option<Uuid>,
) -> PullDecision {
    let local_changed = local.len() != base.files.len()
        || local.iter().any(|(key, file)| {
            base.files.get(key).is_none_or(|old| {
                old.root != file.root
                    || old.path != file.path
                    || old.size != file.size
                    || old.blake3 != file.blake3
            })
        });
    match (local_changed, head == base.base_snapshot_id) {
        (false, true) => PullDecision::Unchanged,
        (true, true) => PullDecision::LocalProgress,
        (false, false) => PullDecision::Restore,
        (true, false) => PullDecision::Conflict,
    }
}

fn load(conn: &Connection, scope: Scope) -> Result<SyncState, DbError> {
    let row = conn
        .query_row(
            "SELECT base_snapshot_id, files, sync_pending, last_synced_at, push_operation
         FROM save_sync_state WHERE server_id=?1 AND package_id=?2 AND user_id=?3",
            params![
                scope.package.server_id.to_string(),
                scope.package.package_id.to_string(),
                scope.user_id.to_string()
            ],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, files, pending, last_synced_at, push)) = row else {
        return Ok(SyncState::default());
    };
    // Corrupt state fails closed, never silently becomes an empty base.
    let base_snapshot_id = id
        .map(|id| serde_json::from_value(serde_json::Value::String(id)))
        .transpose()?;
    Ok(SyncState {
        base_snapshot_id,
        files: serde_json::from_str(&files)?,
        pending,
        last_synced_at,
        push: push.map(|raw| serde_json::from_str(&raw)).transpose()?,
    })
}

fn write(conn: &Connection, scope: Scope, state: &SyncState) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO save_sync_state
         (server_id,package_id,user_id,base_snapshot_id,files,sync_pending,updated_at,last_synced_at,push_operation)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT (server_id,package_id,user_id) DO UPDATE SET
         base_snapshot_id=excluded.base_snapshot_id, files=excluded.files,
         sync_pending=excluded.sync_pending, updated_at=excluded.updated_at,
         last_synced_at=excluded.last_synced_at, push_operation=excluded.push_operation",
        params![scope.package.server_id.to_string(), scope.package.package_id.to_string(), scope.user_id.to_string(),
            state.base_snapshot_id.map(|id| id.to_string()), serde_json::to_string(&state.files)?, state.pending,
            now_unix(), state.last_synced_at, state.push.as_ref().map(serde_json::to_string).transpose()?],
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct Store(Db);
impl Store {
    pub fn new(db: Db) -> Self {
        Self(db)
    }
    pub async fn state(&self, scope: Scope) -> Result<SyncState, DbError> {
        self.0.call(move |conn| load(conn, scope)).await
    }
    pub async fn mark_pending(&self, scope: Scope) -> Result<(), DbError> {
        self.0
            .call(move |conn| {
                let mut state = load(conn, scope)?;
                state.pending = true;
                write(conn, scope, &state)
            })
            .await
    }
    /// Persist before sending. Identical payloads keep their id after a
    /// timeout/restart; changed payloads receive a fresh id.
    pub async fn begin_push(
        &self,
        scope: Scope,
        request: CommitSnapshotRequest,
    ) -> Result<PushOperation, DbError> {
        self.0
            .call(move |conn| {
                let mut state = load(conn, scope)?;
                if let Some(operation) = &state.push
                    && operation.request == request
                {
                    return Ok(operation.clone());
                }
                let operation = PushOperation {
                    id: Uuid::now_v7(),
                    request,
                };
                state.push = Some(operation.clone());
                state.pending = true;
                write(conn, scope, &state)?;
                Ok(operation)
            })
            .await
    }
    /// A delayed response cannot acknowledge a replaced operation. Apply the
    /// committed content and clear the retry record in one SQLite statement.
    pub async fn finish_push(
        &self,
        scope: Scope,
        operation_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<bool, DbError> {
        self.0
            .call(move |conn| {
                let mut state = load(conn, scope)?;
                let Some(operation) = state.push.as_ref().filter(|push| push.id == operation_id)
                else {
                    return Ok(false);
                };
                state.files = operation
                    .request
                    .files
                    .iter()
                    .map(|file| (format!("{}/{}", file.root, file.path), file.clone()))
                    .collect();
                state.base_snapshot_id = Some(snapshot_id);
                state.push = None;
                state.pending = false;
                state.last_synced_at = Some(now_unix());
                write(conn, scope, &state)?;
                Ok(true)
            })
            .await
    }

    /// Records a backup whose restore is about to start, before any save
    /// file changes. Until `finish_restore` or `rolled_back`, startup
    /// recovery rolls it back.
    pub async fn begin_restore(
        &self,
        scope: Scope,
        backup: Uuid,
        reason: BackupReason,
        file_count: i64,
        total_bytes: i64,
    ) -> Result<(), DbError> {
        self.0
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO save_backups
                     (id,server_id,package_id,user_id,reason,state,file_count,total_bytes,created_at)
                     VALUES (?1,?2,?3,?4,?5,'restoring',?6,?7,?8)",
                    params![
                        backup.to_string(),
                        scope.package.server_id.to_string(),
                        scope.package.package_id.to_string(),
                        scope.user_id.to_string(),
                        reason.as_str(),
                        file_count,
                        total_bytes,
                        now_unix()
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Completes a restore and, with `base`, makes the restored snapshot the
    /// sync base in the same transaction. `false` when the record is not an
    /// unfinished restore of this scope (already completed or rolled back).
    pub async fn finish_restore(
        &self,
        scope: Scope,
        backup: Uuid,
        base: Option<RestoredBase>,
    ) -> Result<bool, DbError> {
        self.0
            .call(move |conn| {
                let tx = conn.transaction()?;
                if !settle(&tx, scope, backup, BackupState::Complete)? {
                    return Ok(false);
                }
                if let Some(base) = base {
                    let mut state = load(&tx, scope)?;
                    state.base_snapshot_id = base.snapshot_id;
                    state.files = base.files;
                    // Local files now equal that snapshot; an older retry is void.
                    state.push = None;
                    state.pending = false;
                    state.last_synced_at = Some(now_unix());
                    write(&tx, scope, &state)?;
                }
                tx.commit()?;
                Ok(true)
            })
            .await
    }

    /// Marks an interrupted or failed restore as rolled back from its backup.
    pub async fn rolled_back(&self, scope: Scope, backup: Uuid) -> Result<bool, DbError> {
        self.0
            .call(move |conn| settle(conn, scope, backup, BackupState::RolledBack))
            .await
    }

    /// Every restore that started and never finished, for startup recovery.
    pub async fn unfinished_restores(&self) -> Result<Vec<(Scope, Uuid)>, DbError> {
        self.0
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT server_id, package_id, user_id, id FROM save_backups
                     WHERE state='restoring' ORDER BY id",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                let mut unfinished = Vec::new();
                for row in rows {
                    let (server_id, package_id, user_id, id) = row?;
                    unfinished.push((
                        Scope {
                            package: PackageRef {
                                server_id: uuid(server_id)?,
                                package_id: uuid(package_id)?,
                            },
                            user_id: uuid(user_id)?,
                        },
                        uuid(id)?,
                    ));
                }
                Ok(unfinished)
            })
            .await
    }

    /// Local backups of this scope, newest first (Settings → Cloud saves history).
    pub async fn backups(&self, scope: Scope) -> Result<Vec<BackupRecord>, DbError> {
        self.0.call(move |conn| backups(conn, scope)).await
    }

    /// Forgets finished backups beyond the `keep` newest. Returns every id
    /// still recorded, unfinished ones included, so the caller removes all
    /// other backup folders (`restore::prune(dir, 0, &kept)`).
    pub async fn retain_backups(&self, scope: Scope, keep: usize) -> Result<Vec<Uuid>, DbError> {
        self.0
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut kept = Vec::new();
                let mut finished = 0usize;
                for record in backups(&tx, scope)? {
                    if record.state != BackupState::Restoring {
                        finished += 1;
                        if finished > keep {
                            tx.execute(
                                "DELETE FROM save_backups WHERE id=?1",
                                [record.id.to_string()],
                            )?;
                            continue;
                        }
                    }
                    kept.push(record.id);
                }
                tx.commit()?;
                Ok(kept)
            })
            .await
    }
}

/// Why a local backup was made: before restoring a snapshot, or to protect one
/// side of a conflict resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupReason {
    Restore,
    Conflict,
}
impl BackupReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupState {
    Restoring,
    Complete,
    RolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupRecord {
    pub id: Uuid,
    pub reason: BackupReason,
    pub state: BackupState,
    pub file_count: i64,
    pub total_bytes: i64,
    pub created_at: i64,
}

/// The snapshot a finished restore leaves on disk; `None` id for an empty head.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoredBase {
    pub snapshot_id: Option<Uuid>,
    pub files: BTreeMap<String, SaveFile>,
}

fn uuid(raw: String) -> Result<Uuid, DbError> {
    Ok(serde_json::from_value(serde_json::Value::String(raw))?)
}

fn settle(conn: &Connection, scope: Scope, backup: Uuid, to: BackupState) -> Result<bool, DbError> {
    let state = match to {
        BackupState::Complete => "complete",
        BackupState::RolledBack => "rolled_back",
        BackupState::Restoring => return Ok(false),
    };
    let changed = conn.execute(
        "UPDATE save_backups SET state=?1
         WHERE id=?2 AND server_id=?3 AND package_id=?4 AND user_id=?5 AND state='restoring'",
        params![
            state,
            backup.to_string(),
            scope.package.server_id.to_string(),
            scope.package.package_id.to_string(),
            scope.user_id.to_string()
        ],
    )?;
    Ok(changed == 1)
}

fn backups(conn: &Connection, scope: Scope) -> Result<Vec<BackupRecord>, DbError> {
    let mut statement = conn.prepare(
        "SELECT id, reason, state, file_count, total_bytes, created_at FROM save_backups
         WHERE server_id=?1 AND package_id=?2 AND user_id=?3 ORDER BY id DESC",
    )?;
    let rows = statement.query_map(
        params![
            scope.package.server_id.to_string(),
            scope.package.package_id.to_string(),
            scope.user_id.to_string()
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        },
    )?;
    let mut records = Vec::new();
    for row in rows {
        let (id, reason, state, file_count, total_bytes, created_at) = row?;
        // The schema's CHECKs make any other value a corrupt database: fail closed.
        let reason = match reason.as_str() {
            "restore" => BackupReason::Restore,
            "conflict" => BackupReason::Conflict,
            _ => return Err(rusqlite::Error::InvalidQuery.into()),
        };
        let state = match state.as_str() {
            "restoring" => BackupState::Restoring,
            "complete" => BackupState::Complete,
            "rolled_back" => BackupState::RolledBack,
            _ => return Err(rusqlite::Error::InvalidQuery.into()),
        };
        records.push(BackupRecord {
            id: uuid(id)?,
            reason,
            state,
            file_count,
            total_bytes,
            created_at,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests;
