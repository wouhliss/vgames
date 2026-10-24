//! A restore as one recorded transaction (06-cloud-saves-notes "Ownership,
//! paths and durability"): recover the package's interrupted restores, back up,
//! record, apply, then complete (with the restored base) or roll back. Disk
//! work runs on blocking pools; the record is written before any file changes,
//! so a crash at any point leaves enough to roll back on the next use.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::fs::Dir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::SaveFile;

use super::policy::ProtectedInstalls;
use super::restore::{self, Plan};
use super::scan::{self, Location};
use crate::db::DbError;
use crate::db::saves::{BackupReason, RestoredBase, Scope, Store};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Files(#[from] scan::Error),
    #[error("cannot record the save restore")]
    Db(#[from] DbError),
    /// The restore failed and every file was put back from its backup.
    #[error("the save restore failed and was undone")]
    RolledBack(#[source] scan::Error),
    /// Neither the restore nor its roll back finished: the record stays
    /// unfinished and the next use of this package's saves retries recovery.
    #[error("the save restore could not be undone yet")]
    Unrecovered(#[source] scan::Error),
}

/// What to restore, all from a verified snapshot already staged by BLAKE3.
pub struct Request {
    pub writes: Vec<SaveFile>,
    pub deletes: Vec<SaveFile>,
    /// Folder holding the verified blobs, named by their BLAKE3.
    pub staging: PathBuf,
    pub reason: BackupReason,
    /// Becomes the sync base once every file is in place.
    pub base: Option<RestoredBase>,
}

#[derive(Clone)]
pub struct Transactions {
    store: Store,
    /// `<app data>/save-backups`.
    root: PathBuf,
    /// One transaction at a time: a restore and a recovery of the same
    /// package must never interleave their file work.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl Transactions {
    pub fn new(store: Store, data_dir: &Path) -> Self {
        Self {
            store,
            root: data_dir.join("save-backups"),
            lock: Arc::default(),
        }
    }

    /// `<root>/<server>/<package>/<account>`: backups never collide across
    /// servers, packages or accounts.
    fn dir(&self, scope: Scope) -> PathBuf {
        self.root
            .join(scope.package.server_id.to_string())
            .join(scope.package.package_id.to_string())
            .join(scope.user_id.to_string())
    }

    /// Rolls back every unfinished restore of this scope. Call before the
    /// package's save files are scanned, pushed or restored.
    pub async fn recover(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
    ) -> Result<usize, Error> {
        let _one = self.lock.lock().await;
        self.recover_locked(scope, locations).await
    }

    async fn recover_locked(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
    ) -> Result<usize, Error> {
        let pending: Vec<Uuid> = self
            .store
            .unfinished_restores()
            .await?
            .into_iter()
            .filter(|(owner, _)| *owner == scope)
            .map(|(_, id)| id)
            .collect();
        for &id in &pending {
            let dir = self.dir(scope);
            let locations = locations.clone();
            blocking(move || {
                let backups = open_dir(&dir, false)?;
                let backup = restore::read_backup(&backups, id)?;
                restore::roll_back(&locations, &backup, &backups)
            })
            .await
            .map_err(Error::Unrecovered)?;
            self.store.rolled_back(scope, id).await?;
            tracing::info!(backup = %id, "interrupted save restore rolled back");
        }
        Ok(pending.len())
    }

    /// Restores `request` over this scope's save files. Returns the backup id.
    pub async fn restore(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        request: Request,
        cancel: CancellationToken,
    ) -> Result<Uuid, Error> {
        let _one = self.lock.lock().await;
        self.recover_locked(scope, locations.clone()).await?;
        let dir = self.dir(scope);
        let Request {
            writes,
            deletes,
            staging,
            reason,
            base,
        } = request;
        let (plan, backup) = {
            let (dir, locations, cancel) = (dir.clone(), locations.clone(), cancel.clone());
            blocking(move || {
                let plan = Plan::new(&locations, writes, deletes, &protected, &cancel)?;
                let backup = restore::back_up(&locations, &plan, &open_dir(&dir, true)?, &cancel)?;
                Ok((Arc::new(plan), Arc::new(backup)))
            })
            .await?
        };
        let saved: Vec<_> = backup
            .entries
            .iter()
            .filter_map(|entry| entry.original.as_ref())
            .collect();
        let bytes = saved.iter().map(|original| original.size).sum::<u64>();
        // A crash before this line leaves only an unrecorded folder, which
        // retention removes; after it, recovery has the record it needs.
        self.store
            .begin_restore(
                scope,
                backup.id,
                reason,
                i64::try_from(saved.len()).map_err(|_| scan::Error::Limit)?,
                i64::try_from(bytes).map_err(|_| scan::Error::Limit)?,
            )
            .await?;
        let applied = {
            let (locations, plan) = (locations.clone(), plan.clone());
            blocking(move || {
                let staging = Dir::open_ambient_dir(&staging, cap_std::ambient_authority())?;
                restore::apply(&locations, &plan, &staging, &cancel)
            })
            .await
        };
        if let Err(failure) = applied {
            let undo = {
                let (dir, backup) = (dir.clone(), backup.clone());
                blocking(move || restore::roll_back(&locations, &backup, &open_dir(&dir, false)?))
                    .await
            };
            return match undo {
                Ok(()) => {
                    self.store.rolled_back(scope, backup.id).await?;
                    Err(Error::RolledBack(failure))
                }
                Err(error) => {
                    tracing::error!(error = %error, "save restore roll back failed; retrying on next use");
                    Err(Error::Unrecovered(failure))
                }
            };
        }
        self.store.finish_restore(scope, backup.id, base).await?;
        self.retain(scope).await;
        Ok(backup.id)
    }

    /// Backs up `files` (the device's current saves) without changing any of
    /// them, so a conflict can always return to this side. Returns the backup id.
    pub async fn protect(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        files: Vec<SaveFile>,
        cancel: CancellationToken,
    ) -> Result<Uuid, Error> {
        let _one = self.lock.lock().await;
        self.recover_locked(scope, locations.clone()).await?;
        let dir = self.dir(scope);
        let backup = blocking(move || {
            // Every file is a target of the plan, so it is backed up; nothing
            // is ever applied.
            let plan = Plan::new(&locations, Vec::new(), files, &protected, &cancel)?;
            restore::back_up(&locations, &plan, &open_dir(&dir, true)?, &cancel)
        })
        .await?;
        self.record_protected(scope, &backup).await
    }

    /// Backs up `files` from their verified blobs in `staging` (the cloud
    /// side a "keep both" resolution keeps locally). No save file changes.
    pub async fn protect_staged(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        files: Vec<SaveFile>,
        staging: PathBuf,
        cancel: CancellationToken,
    ) -> Result<Uuid, Error> {
        let _one = self.lock.lock().await;
        let dir = self.dir(scope);
        let backup = blocking(move || {
            // The same validation as a restore of these files.
            let plan = Plan::new(&locations, files, Vec::new(), &protected, &cancel)?;
            let staging = Dir::open_ambient_dir(&staging, cap_std::ambient_authority())?;
            restore::back_up_staged(&plan, &staging, &open_dir(&dir, true)?, &cancel)
        })
        .await?;
        self.record_protected(scope, &backup).await
    }

    async fn record_protected(
        &self,
        scope: Scope,
        backup: &restore::Backup,
    ) -> Result<Uuid, Error> {
        let saved: Vec<_> = backup
            .entries
            .iter()
            .filter_map(|entry| entry.original.as_ref())
            .collect();
        let bytes = saved.iter().map(|original| original.size).sum::<u64>();
        self.store
            .record_backup(
                scope,
                backup.id,
                BackupReason::Conflict,
                i64::try_from(saved.len()).map_err(|_| scan::Error::Limit)?,
                i64::try_from(bytes).map_err(|_| scan::Error::Limit)?,
            )
            .await?;
        self.retain(scope).await;
        Ok(backup.id)
    }

    /// Keeps the newest finished backups plus every unfinished one, and removes
    /// unrecorded folders. Retention never fails a restore that succeeded.
    async fn retain(&self, scope: Scope) {
        let kept = match self
            .store
            .retain_backups(scope, restore::KEEP_BACKUPS)
            .await
        {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(error = %error, "cannot apply save backup retention");
                return;
            }
        };
        let dir = self.dir(scope);
        if let Err(error) =
            blocking(move || restore::prune(&open_dir(&dir, false)?, 0, &kept)).await
        {
            tracing::warn!(error = %error, "cannot remove old save backups");
        }
    }
}

fn open_dir(path: &Path, create: bool) -> Result<Dir, scan::Error> {
    if create {
        crate::paths::create_private_dir(path).map_err(|_| {
            scan::Error::Io(std::io::Error::other("cannot create the backup folder"))
        })?;
    }
    Ok(Dir::open_ambient_dir(path, cap_std::ambient_authority())?)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, scan::Error> + Send + 'static,
) -> Result<T, scan::Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| scan::Error::Io(std::io::Error::other("save file work stopped")))?
}

#[cfg(test)]
mod tests;
