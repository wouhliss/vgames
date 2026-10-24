//! The cloud-save service (06-cloud-saves-notes "Commands and summaries"): the
//! launch hook plus each package's `SaveStatus`, read from durable state so a
//! missed `save-sync-changed` event is repaired by asking again. Status never
//! makes a network request.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use specta::Type;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::hook::{Accounts, SaveHook};
use super::locations::KnownFolders;
use crate::db::saves::{Scope, Store};
use crate::db::{self, Db, DbError};
use crate::events::{AppEvent, EventBus, GameExit, PackageRef};
use crate::installs::list::{CloudSaveProvider, CloudSaveState};
use crate::launch::hooks::{HookEnv, HookFuture, LaunchContext, LaunchHooks, LaunchVeto};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct SaveStatus {
    pub package: PackageRef,
    pub state: CloudSaveState,
    /// RFC 3339, null when this device never synced.
    pub last_synced_at: Option<String>,
    /// Set exactly when `state` is `conflict`.
    pub conflict_id: Option<Uuid>,
}

/// A package's save status changed; re-read only that package.
#[derive(Debug, Clone, Serialize, Type, tauri_specta::Event)]
#[serde(transparent)]
pub struct SaveSyncChanged(pub SaveStatus);

#[derive(Debug, thiserror::Error)]
#[error("cannot read the cloud-save state")]
pub struct StatusError(#[from] DbError);

pub struct Saves {
    hook: SaveHook,
    accounts: Arc<dyn Accounts>,
    store: Store,
    db: Db,
    bus: EventBus,
    /// Packages whose pull or push is running.
    syncing: Mutex<HashSet<PackageRef>>,
}

impl Saves {
    pub fn new(
        accounts: Arc<dyn Accounts>,
        db: Db,
        bus: EventBus,
        folders: KnownFolders,
        data_dir: PathBuf,
    ) -> reqwest::Result<Self> {
        Ok(Self {
            hook: SaveHook::new(accounts.clone(), db.clone(), folders, data_dir)?,
            accounts,
            store: Store::new(db.clone()),
            db,
            bus,
            syncing: Mutex::default(),
        })
    }

    /// From durable state, the running syncs and the installed manifest.
    pub async fn status(&self, package: PackageRef) -> Result<SaveStatus, StatusError> {
        let status = |state, last_synced_at, conflict_id| SaveStatus {
            package,
            state,
            last_synced_at,
            conflict_id,
        };
        if !self.declares_saves(package).await? {
            return Ok(status(CloudSaveState::Unsupported, None, None));
        }
        // Signed out: nothing can sync until the player signs in again.
        let Some(account) = self
            .accounts
            .account(package.server_id, CancellationToken::new())
            .await
        else {
            return Ok(status(CloudSaveState::Pending, None, None));
        };
        let scope = Scope {
            package,
            user_id: account.user_id,
        };
        let state = self.store.state(scope).await?;
        let last = state.last_synced_at.and_then(|at| {
            time::OffsetDateTime::from_unix_timestamp(at)
                .ok()
                .map(crate::catalog::rfc3339)
        });
        if let Some(open) = self.store.open_conflict_for(scope).await? {
            return Ok(status(CloudSaveState::Conflict, last, Some(open.id)));
        }
        let kind = if self.is_syncing(package) {
            CloudSaveState::Syncing
        } else if state.pending || state.last_synced_at.is_none() {
            CloudSaveState::Pending
        } else {
            CloudSaveState::Synced
        };
        Ok(status(kind, last, None))
    }

    async fn declares_saves(&self, package: PackageRef) -> Result<bool, StatusError> {
        let Some(row) = db::installs::row(&self.db, package).await? else {
            return Ok(false);
        };
        Ok(
            tokio::task::spawn_blocking(move || manifest_declares_saves(&row.root))
                .await
                .unwrap_or(false),
        )
    }

    fn is_syncing(&self, package: PackageRef) -> bool {
        self.syncing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&package)
    }

    fn set_syncing(&self, package: PackageRef, on: bool) {
        let mut syncing = self
            .syncing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if on {
            syncing.insert(package);
        } else {
            syncing.remove(&package);
        }
    }

    /// Publishes the package's current status (best effort: the UI rereads).
    async fn announce(&self, package: PackageRef) {
        match self.status(package).await {
            Ok(status) => self
                .bus
                .publish(AppEvent::SaveSyncChanged(SaveSyncChanged(status))),
            Err(error) => tracing::warn!(error = %error, "cannot read the cloud-save status"),
        }
    }

    async fn around<T>(&self, ctx: &LaunchContext, work: impl Future<Output = T>) -> T {
        // Packages without saves never show a sync.
        if ctx.saves.as_ref().is_none_or(|s| s.locations.is_empty()) {
            return work.await;
        }
        self.set_syncing(ctx.package, true);
        self.announce(ctx.package).await;
        let result = work.await;
        self.set_syncing(ctx.package, false);
        self.announce(ctx.package).await;
        result
    }
}

/// The installed release's verified manifest declares save locations.
fn manifest_declares_saves(root: &Path) -> bool {
    let path = root
        .join(vgames_transfer::install::META_DIR)
        .join(vgames_transfer::install::MANIFEST_FILE);
    std::fs::read(path)
        .ok()
        .and_then(|bytes| vgames_core::manifest::parse_and_validate(&bytes).ok())
        .and_then(|manifest| manifest.saves)
        .is_some_and(|saves| !saves.locations.is_empty())
}

impl LaunchHooks for Saves {
    fn before_spawn<'a>(
        &'a self,
        ctx: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(self.around(ctx, self.hook.before_spawn(ctx)))
    }

    fn after_exit<'a>(&'a self, ctx: &'a LaunchContext, exit: &'a GameExit) -> HookFuture<'a, ()> {
        Box::pin(self.around(ctx, self.hook.after_exit(ctx, exit)))
    }

    fn aborted(&self, ctx: &LaunchContext) {
        // A timed-out or dropped sync no longer runs.
        self.set_syncing(ctx.package, false);
        self.hook.aborted(ctx);
    }

    fn before_spawn_budget(&self) -> Duration {
        self.hook.before_spawn_budget()
    }

    fn after_exit_budget(&self) -> Duration {
        self.hook.after_exit_budget()
    }
}

/// `installs_list` reads this from its blocking pool.
impl CloudSaveProvider for Saves {
    fn state(&self, package: PackageRef) -> CloudSaveState {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return CloudSaveState::Unsupported;
        };
        runtime
            .block_on(self.status(package))
            .map_or(CloudSaveState::Pending, |status| status.state)
    }
}

#[cfg(test)]
mod tests;
