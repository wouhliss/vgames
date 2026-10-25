//! Conflict lookup and the four choices (06-cloud-saves-notes "Commands and
//! summaries"). Nothing here launches a game or picks a side by itself: a
//! changed head renews the conflict, and offline or quota failures keep it.
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::manifest::Platform;
use vgames_proto::saves::SaveSnapshotSummary;

use super::{SaveStatus, Saves, installed_saves};
use crate::db::{self, saves::ConflictRecord};
use crate::error::AppError;
use crate::events::{AppEvent, PackageRef};
use crate::launch::hooks::{LaunchContext, LaunchLayer};
use crate::saves::sync::Resolution;

/// Where a summarized save lives: a server snapshot or a local backup. Both
/// ids are opaque; neither is ever a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SaveSource {
    Cloud { snapshot_id: Uuid },
    Backup { backup_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct SaveSummary {
    pub source: SaveSource,
    /// RFC 3339.
    pub created_at: Option<String>,
    /// Plain text from the server; null for this device's own backups.
    pub device_name: Option<String>,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// A durable conflict, as the dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, tauri_specta::Event)]
pub struct SaveConflict {
    pub conflict_id: Uuid,
    pub package: PackageRef,
    pub cloud: SaveSummary,
    /// The device's files as they were when the conflict was found.
    pub device: SaveSummary,
    /// RFC 3339.
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConflictChoice {
    KeepCloud,
    KeepDevice,
    KeepBoth,
    Cancel,
}

impl Saves {
    /// An open conflict of the signed-in account.
    pub async fn conflict(&self, id: Uuid) -> Result<SaveConflict, AppError> {
        let record = self.open_record(id).await?;
        self.summarize(record).await
    }

    /// Applies the player's choice. Refused while the game runs; serialized
    /// with launches of the package. Never launches the game.
    pub async fn resolve(&self, id: Uuid, choice: ConflictChoice) -> Result<SaveStatus, AppError> {
        let record = self.open_record(id).await?;
        let package = record.scope.package;
        if choice == ConflictChoice::Cancel {
            return self.status_or_error(package).await;
        }
        let _one = self.lock(package).lock_owned().await;
        if (self.running)(package) {
            return Err(AppError::invalid(
                "conflict_id",
                "Close the game before choosing which saves to keep.",
            ));
        }
        // Re-read under the lock: a launch may have refreshed it.
        let record = self.open_record(id).await?;
        let ctx = self.context(package).await?;
        let cancel = CancellationToken::new();
        let prepared = self
            .hook
            .prepare(&ctx, &cancel)
            .await
            .map_err(|error| AppError::internal("prepare the save files", &error))?
            .filter(|prepared| prepared.scope == record.scope)
            .ok_or(AppError::Unauthenticated)?;
        let expected = record.cloud.id;
        self.set_syncing(package, true);
        let (sync, scope) = (&prepared.sync, prepared.scope);
        let (locations, protected) = (prepared.locations.clone(), prepared.protected.clone());
        let outcome = match choice {
            ConflictChoice::KeepCloud => {
                sync.keep_cloud(scope, locations, protected, expected, cancel)
                    .await
            }
            ConflictChoice::KeepDevice => {
                sync.keep_device(scope, locations, expected, cancel).await
            }
            ConflictChoice::KeepBoth => {
                sync.keep_both(scope, locations, protected, expected, cancel)
                    .await
            }
            ConflictChoice::Cancel => {
                self.set_syncing(package, false);
                return self.status_or_error(package).await;
            }
        };
        self.set_syncing(package, false);
        let result = match outcome {
            Ok(Resolution::Done) => self.settle(&record, false).await,
            Ok(Resolution::CloudGone) => self.settle(&record, true).await,
            Ok(Resolution::Renewed(head)) => {
                self.store
                    .open_conflict(scope, *head, record.device_backup_id)
                    .await
                    .map_err(|error| AppError::internal("record the save conflict", &error))?;
                if let Ok(conflict) = self.conflict(id).await {
                    self.bus.publish(AppEvent::SaveConflict(conflict));
                }
                Ok(())
            }
            Ok(Resolution::Offline) => Err(AppError::Network {
                detail: "the cloud-save server could not be reached".into(),
            }),
            Ok(Resolution::QuotaExceeded) => Err(AppError::Server {
                code: "save_quota_exceeded".into(),
                message: "Your cloud-save storage is full.".into(),
            }),
            Err(error) => Err(AppError::internal("apply the save choice", &error)),
        };
        self.announce(package).await;
        result?;
        self.status_or_error(package).await
    }

    /// Closes the conflict once the chosen side is in place.
    async fn settle(&self, record: &ConflictRecord, pending: bool) -> Result<(), AppError> {
        let failed =
            |error: &dyn std::error::Error| AppError::internal("close the save conflict", error);
        self.store
            .resolve_conflict(record.scope, record.id)
            .await
            .map_err(|e| failed(&e))?;
        if pending {
            self.store
                .mark_pending(record.scope)
                .await
                .map_err(|e| failed(&e))?;
        }
        Ok(())
    }

    /// Publishes `save-conflict` when a sync opened a conflict other than `before`.
    pub(super) async fn announce_new_conflict(&self, package: PackageRef, before: Option<Uuid>) {
        let Ok(status) = self.status(package).await else {
            return;
        };
        if let Some(id) = status.conflict_id.filter(|id| Some(*id) != before)
            && let Ok(conflict) = self.conflict(id).await
        {
            self.bus.publish(AppEvent::SaveConflict(conflict));
        }
    }

    async fn open_record(&self, id: Uuid) -> Result<ConflictRecord, AppError> {
        let record = self
            .store
            .conflict(id)
            .await
            .map_err(|error| AppError::internal("read the save conflict", &error))?
            .filter(|record| record.resolved_at.is_none())
            .ok_or(AppError::NotFound)?;
        // Only the signed-in account's conflicts are visible.
        let account = self
            .accounts
            .account(record.scope.package.server_id, CancellationToken::new())
            .await
            .ok_or(AppError::Unauthenticated)?;
        if account.user_id != record.scope.user_id {
            return Err(AppError::NotFound);
        }
        Ok(record)
    }

    async fn summarize(&self, record: ConflictRecord) -> Result<SaveConflict, AppError> {
        let backups = self
            .store
            .backups(record.scope)
            .await
            .map_err(|error| AppError::internal("read the save backups", &error))?;
        let backup = backups
            .iter()
            .find(|backup| backup.id == record.device_backup_id);
        Ok(SaveConflict {
            conflict_id: record.id,
            package: record.scope.package,
            cloud: cloud_summary(&record.cloud),
            device: SaveSummary {
                source: SaveSource::Backup {
                    backup_id: record.device_backup_id,
                },
                created_at: backup.and_then(|b| rfc3339(b.created_at)),
                device_name: None,
                file_count: backup.map_or(0, |b| u64::try_from(b.file_count).unwrap_or(0)),
                total_bytes: backup.map_or(0, |b| u64::try_from(b.total_bytes).unwrap_or(0)),
            },
            created_at: rfc3339(record.created_at),
        })
    }

    pub(super) async fn status_or_error(
        &self,
        package: PackageRef,
    ) -> Result<SaveStatus, AppError> {
        self.status(package)
            .await
            .map_err(|error| AppError::internal("read the cloud-save status", &error))
    }

    /// What a launch of the installed release would use, without launching.
    pub(super) async fn context(&self, package: PackageRef) -> Result<LaunchContext, AppError> {
        let row = db::installs::row(&self.db, package)
            .await
            .map_err(|error| AppError::internal("read the install", &error))?
            .ok_or(AppError::NotFound)?;
        let platform = Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == row.platform)
            .ok_or(AppError::NotFound)?;
        let layer =
            LaunchLayer::for_platform(platform, crate::launch::orchestrate::host_platform())
                .map_err(|_| AppError::NotFound)?;
        let root = row.root.clone();
        let saves = tokio::task::spawn_blocking(move || installed_saves(&root))
            .await
            .ok()
            .flatten()
            .ok_or(AppError::NotFound)?;
        Ok(LaunchContext {
            package,
            root: row.root,
            platform,
            layer,
            prefix_dir: (layer != LaunchLayer::Native)
                .then(|| crate::compat::prefix_dir(&self.data_dir, &package)),
            saves: Some(std::sync::Arc::new(saves)),
            cancellation: CancellationToken::new(),
        })
    }
}

pub(super) fn cloud_summary(head: &SaveSnapshotSummary) -> SaveSummary {
    SaveSummary {
        source: SaveSource::Cloud {
            snapshot_id: head.id,
        },
        created_at: Some(crate::catalog::rfc3339(head.created_at)),
        device_name: head.device_name.clone(),
        file_count: u64::try_from(head.file_count).unwrap_or(0),
        total_bytes: u64::try_from(head.total_size).unwrap_or(0),
    }
}

pub(super) fn rfc3339(unix: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .map(crate::catalog::rfc3339)
}
