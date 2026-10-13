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
#[derive(Clone, Copy)]
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
}

#[cfg(test)]
mod tests;
