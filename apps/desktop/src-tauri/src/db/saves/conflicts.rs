//! Durable conflict records (migration 0012). A conflict is never resolved
//! here: these functions only remember it until the player chooses.
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vgames_proto::saves::SaveSnapshotSummary;

use super::{BackupReason, Scope, Store, uuid};
use crate::db::{DbError, now_unix};
use crate::events::PackageRef;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRecord {
    pub id: Uuid,
    pub scope: Scope,
    /// The cloud head when the conflict was (last) detected.
    pub cloud: SaveSnapshotSummary,
    /// Protects the device's files as they were when detected.
    pub device_backup_id: Uuid,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

impl Store {
    /// Records a backup made only to protect files (nothing was restored).
    pub async fn record_backup(
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
                     VALUES (?1,?2,?3,?4,?5,'complete',?6,?7,?8)",
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

    /// Opens the scope's conflict, or refreshes its cloud side when one is
    /// already open (the head moved again): the id stays stable for the UI and
    /// the device backup of the first detection keeps protecting local files.
    /// Returns the conflict id.
    pub async fn open_conflict(
        &self,
        scope: Scope,
        cloud: SaveSnapshotSummary,
        device_backup: Uuid,
    ) -> Result<Uuid, DbError> {
        if cloud.package_id != scope.package.package_id {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        let json = serde_json::to_string(&cloud)?;
        self.0
            .call(move |conn| {
                let tx = conn.transaction()?;
                if let Some(open) = open_for(&tx, scope)? {
                    tx.execute(
                        "UPDATE save_conflicts SET head_snapshot_id=?1, cloud=?2 WHERE id=?3",
                        params![cloud.id.to_string(), json, open.id.to_string()],
                    )?;
                    tx.commit()?;
                    return Ok(open.id);
                }
                let id = Uuid::now_v7();
                tx.execute(
                    "INSERT INTO save_conflicts
                     (id,server_id,package_id,user_id,head_snapshot_id,cloud,device_backup_id,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        id.to_string(),
                        scope.package.server_id.to_string(),
                        scope.package.package_id.to_string(),
                        scope.user_id.to_string(),
                        cloud.id.to_string(),
                        json,
                        device_backup.to_string(),
                        now_unix()
                    ],
                )?;
                tx.commit()?;
                Ok(id)
            })
            .await
    }

    /// A conflict by id, open or resolved.
    pub async fn conflict(&self, id: Uuid) -> Result<Option<ConflictRecord>, DbError> {
        self.0
            .call(move |conn| {
                conn.query_row(&format!("{SELECT} WHERE id=?1"), [id.to_string()], row)
                    .optional()?
                    .map(parse)
                    .transpose()
            })
            .await
    }

    /// The scope's open conflict, if any.
    pub async fn open_conflict_for(&self, scope: Scope) -> Result<Option<ConflictRecord>, DbError> {
        self.0.call(move |conn| open_for(conn, scope)).await
    }

    /// Marks the scope's conflict resolved. `false` when it is not this
    /// scope's or was already resolved.
    pub async fn resolve_conflict(&self, scope: Scope, id: Uuid) -> Result<bool, DbError> {
        self.0
            .call(move |conn| {
                let changed = conn.execute(
                    "UPDATE save_conflicts SET resolved_at=?1
                     WHERE id=?2 AND server_id=?3 AND package_id=?4 AND user_id=?5
                     AND resolved_at IS NULL",
                    params![
                        now_unix(),
                        id.to_string(),
                        scope.package.server_id.to_string(),
                        scope.package.package_id.to_string(),
                        scope.user_id.to_string()
                    ],
                )?;
                Ok(changed == 1)
            })
            .await
    }
}

const SELECT: &str = "SELECT id, server_id, package_id, user_id, cloud, device_backup_id,
    created_at, resolved_at FROM save_conflicts";

type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
);

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn parse(
    (id, server_id, package_id, user_id, cloud, backup, created_at, resolved_at): Row,
) -> Result<ConflictRecord, DbError> {
    Ok(ConflictRecord {
        id: uuid(id)?,
        scope: Scope {
            package: PackageRef {
                server_id: uuid(server_id)?,
                package_id: uuid(package_id)?,
            },
            user_id: uuid(user_id)?,
        },
        cloud: serde_json::from_str(&cloud)?,
        device_backup_id: uuid(backup)?,
        created_at,
        resolved_at,
    })
}

fn open_for(conn: &Connection, scope: Scope) -> Result<Option<ConflictRecord>, DbError> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE server_id=?1 AND package_id=?2 AND user_id=?3 AND resolved_at IS NULL"
        ),
        params![
            scope.package.server_id.to_string(),
            scope.package.package_id.to_string(),
            scope.user_id.to_string()
        ],
        row,
    )
    .optional()?
    .map(parse)
    .transpose()
}

#[cfg(test)]
mod tests;
