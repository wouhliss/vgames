//! Save history and restoring an earlier save (06-cloud-saves-notes "Commands
//! and summaries"): server snapshots plus this device's retained backups, and
//! a confirmed restore of either, refused while the game runs or a conflict
//! waits for the player.
use serde::Serialize;
use specta::Type;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::resolve::{cloud_summary, rfc3339};
use super::{SaveSource, SaveStatus, SaveSummary, Saves};
use crate::db::saves::BackupState;
use crate::error::AppError;
use crate::events::PackageRef;
use crate::saves::hook::Prepared;
use crate::saves::{sync, transaction};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct SaveHistory {
    pub package: PackageRef,
    /// Newest first: cloud snapshots and this device's backups.
    pub entries: Vec<SaveSummary>,
    /// Null when the server has no saves or could not be reached.
    pub head_snapshot_id: Option<Uuid>,
}

impl Saves {
    /// Cloud snapshots (when reachable) and the retained local backups.
    pub async fn history(&self, package: PackageRef) -> Result<SaveHistory, AppError> {
        let prepared = self.prepared(package).await?;
        let cloud = prepared
            .sync
            .history(prepared.scope)
            .await
            .map_err(|error| AppError::internal("read the save history", &error))?;
        let backups = self
            .store
            .backups(prepared.scope)
            .await
            .map_err(|error| AppError::internal("read the save backups", &error))?;
        let mut entries: Vec<(i64, SaveSummary)> = backups
            .into_iter()
            .filter(|backup| backup.state == BackupState::Complete)
            .map(|backup| {
                let summary = SaveSummary {
                    source: SaveSource::Backup {
                        backup_id: backup.id,
                    },
                    created_at: rfc3339(backup.created_at),
                    device_name: None,
                    file_count: u64::try_from(backup.file_count).unwrap_or(0),
                    total_bytes: u64::try_from(backup.total_bytes).unwrap_or(0),
                };
                (backup.created_at, summary)
            })
            .collect();
        let head = cloud.as_ref().and_then(|cloud| cloud.head);
        if let Some(cloud) = cloud {
            entries.extend(cloud.snapshots.iter().map(|snapshot| {
                (
                    snapshot.created_at.unix_timestamp(),
                    cloud_summary(snapshot),
                )
            }));
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        Ok(SaveHistory {
            package,
            entries: entries.into_iter().map(|(_, summary)| summary).collect(),
            head_snapshot_id: head,
        })
    }

    /// Puts an earlier save back after backing up the current files. It is
    /// pushed as this device's progress after the next session.
    pub async fn restore(
        &self,
        package: PackageRef,
        source: SaveSource,
    ) -> Result<SaveStatus, AppError> {
        let _one = self.lock(package).lock_owned().await;
        if (self.running)(package) {
            return Err(AppError::invalid(
                "pkg",
                "Close the game before restoring its saves.",
            ));
        }
        let prepared = self.prepared(package).await?;
        let scope = prepared.scope;
        let open = self
            .store
            .open_conflict_for(scope)
            .await
            .map_err(|error| AppError::internal("read the save conflict", &error))?;
        if open.is_some() {
            return Err(AppError::invalid(
                "pkg",
                "Choose which saves to keep before restoring an older one.",
            ));
        }
        let (locations, protected) = (prepared.locations.clone(), prepared.protected.clone());
        let cancel = CancellationToken::new();
        self.set_syncing(package, true);
        let result = match source {
            SaveSource::Cloud { snapshot_id } => prepared
                .sync
                .restore_snapshot(scope, locations, protected, snapshot_id, cancel)
                .await
                .map(|restored| restored.then_some(snapshot_id)),
            SaveSource::Backup { backup_id } => {
                let known = self
                    .store
                    .backups(scope)
                    .await
                    .map_err(|error| AppError::internal("read the save backups", &error))?
                    .iter()
                    .any(|b| b.id == backup_id && b.state == BackupState::Complete);
                if !known {
                    self.set_syncing(package, false);
                    return Err(AppError::NotFound);
                }
                prepared
                    .sync
                    .restore_backup(scope, locations, protected, backup_id, cancel)
                    .await
                    .map(|()| Some(backup_id))
            }
        };
        self.set_syncing(package, false);
        self.announce(package).await;
        match result {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(AppError::Network {
                    detail: "the cloud-save server could not be reached".into(),
                });
            }
            Err(sync::Error::Api(crate::saves::http::Error::Api(error)))
                if error.status() == Some(404) =>
            {
                return Err(AppError::NotFound);
            }
            Err(sync::Error::Transaction(transaction::Error::RolledBack(error))) => {
                tracing::warn!(error = %error, "save restore failed and was undone");
                return Err(AppError::Io {
                    path: None,
                    detail: "The saves could not be restored. Nothing changed.".into(),
                });
            }
            Err(error) => return Err(AppError::internal("restore the saves", &error)),
        }
        self.status_or_error(package).await
    }

    /// The signed-in account's view of the installed package's saves.
    async fn prepared(&self, package: PackageRef) -> Result<Prepared, AppError> {
        let ctx = self.context(package).await?;
        self.hook
            .prepare(&ctx, &CancellationToken::new())
            .await
            .map_err(|error| AppError::internal("prepare the save files", &error))?
            .ok_or(AppError::Unauthenticated)
    }
}
