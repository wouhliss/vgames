//! Cloud saves around a launch (06-cloud-saves-notes "Ownership, paths and
//! durability"): pull before the game starts, push after its process tree
//! exits. A conflict protects the device's saves and vetoes the launch; an
//! unreachable server or a failed pull launches with local saves, "sync
//! pending". Only a restore that could not be undone keeps the game closed.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::blobs::Blobs;
use super::locations::{self, KnownFolders, Target};
use super::policy::ProtectedInstalls;
use super::scan::{self, Location};
use super::sync::{self, Pull, Push, Sync};
use super::{http, transaction};
use crate::db::saves::{Scope, Store};
use crate::db::{self, Db};
use crate::events::GameExit;
use crate::launch::hooks::{
    HookEnv, HookFuture, LaunchContext, LaunchHooks, LaunchLayer, LaunchVeto,
};
use vgames_core::compat::RunnerKind;

/// Head lookup and staging must finish within this, or the game starts with
/// local saves (a restore already applying still completes or rolls back).
const PULL_BUDGET: Duration = Duration::from_secs(5);
/// Lets the game's last writes reach the disk before the scan.
const FLUSH_GRACE: Duration = Duration::from_secs(3);

/// The signed-in account of one server.
pub struct Account {
    pub user_id: Uuid,
    pub http: http::Client,
    /// The server's API itself is plain http (debug, loopback).
    pub plain_http: bool,
}

pub trait Accounts: Send + std::marker::Sync + 'static {
    /// `None` while signed out: no request is ever made for another account.
    fn account<'a>(
        &'a self,
        server: Uuid,
        cancel: CancellationToken,
    ) -> HookFuture<'a, Option<Account>>;
}

impl Accounts for crate::servers::Servers {
    fn account<'a>(
        &'a self,
        server: Uuid,
        cancel: CancellationToken,
    ) -> HookFuture<'a, Option<Account>> {
        Box::pin(async move {
            let profile = self.profile(server).await.ok().flatten()?;
            let user_id = profile.account?.user_id;
            let api = self.api(server).await.ok()?;
            Some(Account {
                user_id,
                http: http::Client::new(api, cancel),
                plain_http: url::Url::parse(&profile.url).is_ok_and(|url| url.scheme() == "http"),
            })
        })
    }
}

pub struct SaveHook {
    accounts: Arc<dyn Accounts>,
    https: Blobs,
    plain: Blobs,
    transactions: transaction::Transactions,
    store: Store,
    db: Db,
    folders: KnownFolders,
    data_dir: PathBuf,
}

/// One launch's resolved saves.
struct Prepared {
    scope: Scope,
    sync: Sync,
    locations: Arc<Vec<Location>>,
    protected: Arc<ProtectedInstalls>,
}

impl SaveHook {
    pub fn new(
        accounts: Arc<dyn Accounts>,
        db: Db,
        folders: KnownFolders,
        data_dir: PathBuf,
    ) -> reqwest::Result<Self> {
        let store = Store::new(db.clone());
        Ok(Self {
            accounts,
            https: Blobs::new(false)?,
            plain: Blobs::new(true)?,
            transactions: transaction::Transactions::new(store.clone(), &data_dir),
            store,
            db,
            folders,
            data_dir,
        })
    }

    /// `None` when the release declares no saves or nobody is signed in.
    async fn prepare(
        &self,
        ctx: &LaunchContext,
        cancel: &CancellationToken,
    ) -> Result<Option<Prepared>, sync::Error> {
        let Some(saves) = ctx.saves.clone().filter(|s| !s.locations.is_empty()) else {
            return Ok(None);
        };
        let Some(account) = self
            .accounts
            .account(ctx.package.server_id, cancel.clone())
            .await
        else {
            return Ok(None);
        };
        let target = match (ctx.layer, &ctx.prefix_dir) {
            (LaunchLayer::Native, _) => Target::Native,
            (LaunchLayer::Proton, Some(prefix)) => Target::Compat {
                prefix: prefix.clone(),
                runner: RunnerKind::Proton,
            },
            (LaunchLayer::Wine, Some(prefix)) => Target::Compat {
                prefix: prefix.clone(),
                runner: RunnerKind::Wine,
            },
            _ => return Err(scan::Error::Invalid.into()),
        };
        let roots = db::installs::roots(&self.db).await?;
        let (folders, root) = (self.folders.clone(), ctx.root.clone());
        let (locations, protected) = tokio::task::spawn_blocking(move || {
            let locations = locations::open_all(&saves.locations, &folders, &root, &target)?;
            // An offline library cannot be written through anyway.
            let present: Vec<PathBuf> = roots
                .into_iter()
                .map(|(_, root)| root)
                .filter(|root| root.is_dir())
                .collect();
            let protected = ProtectedInstalls::open(present.iter().map(PathBuf::as_path))?;
            Ok::<_, scan::Error>((Arc::new(locations), Arc::new(protected)))
        })
        .await
        .map_err(|_| scan::Error::Io(std::io::Error::other("save preparation stopped")))??;
        let blobs = if account.plain_http {
            self.plain.clone()
        } else {
            self.https.clone()
        };
        Ok(Some(Prepared {
            scope: Scope {
                package: ctx.package,
                user_id: account.user_id,
            },
            sync: Sync::new(
                account.http,
                blobs,
                self.transactions.clone(),
                self.store.clone(),
                &self.data_dir,
            ),
            locations,
            protected,
        }))
    }

    async fn pull(&self, ctx: &LaunchContext, cancel: CancellationToken) -> Result<(), LaunchVeto> {
        let prepared = match self.prepare(ctx, &cancel).await {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "cannot prepare cloud saves; launching with local saves");
                return Ok(());
            }
        };
        let scope = prepared.scope;
        // An unresolved conflict keeps vetoing until the player chooses.
        match self.store.open_conflict_for(scope).await {
            Ok(Some(open)) => {
                return Err(LaunchVeto::SaveConflict {
                    conflict_id: open.id.to_string(),
                });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %error, "cannot read cloud-save conflicts");
                return Err(LaunchVeto::SavesUnavailable);
            }
        }
        let pulled = prepared
            .sync
            .pull(
                scope,
                prepared.locations.clone(),
                prepared.protected.clone(),
                cancel.clone(),
            )
            .await;
        let head = match pulled {
            Ok(Pull::Conflict { head }) => head.summary,
            Ok(outcome) => {
                tracing::info!(?outcome, "cloud saves pulled");
                return Ok(());
            }
            Err(error) => return self.failed(scope, &error).await,
        };
        match prepared
            .sync
            .record_conflict(
                scope,
                prepared.locations,
                prepared.protected,
                Some(head),
                cancel,
            )
            .await
        {
            Ok(Some(id)) => Err(LaunchVeto::SaveConflict {
                conflict_id: id.to_string(),
            }),
            Ok(None) => Ok(()),
            // Never launch over saves whose other side is not protected yet.
            Err(error) => {
                tracing::warn!(error = %error, "cannot protect conflicting saves");
                Err(LaunchVeto::SavesUnavailable)
            }
        }
    }

    async fn failed(&self, scope: Scope, error: &sync::Error) -> Result<(), LaunchVeto> {
        if matches!(
            error,
            sync::Error::Transaction(transaction::Error::Unrecovered(_))
        ) {
            tracing::error!(error = %error, "save files are half restored; not launching");
            return Err(LaunchVeto::SavesUnavailable);
        }
        tracing::warn!(error = %error, "cloud-save pull failed; launching with local saves");
        if let Err(error) = self.store.mark_pending(scope).await {
            tracing::warn!(error = %error, "cannot mark cloud saves pending");
        }
        Ok(())
    }

    async fn push(&self, ctx: &LaunchContext, cancel: CancellationToken) {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(FLUSH_GRACE) => {}
        }
        let prepared = match self.prepare(ctx, &cancel).await {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %error, "cannot prepare cloud saves for upload");
                return;
            }
        };
        let scope = prepared.scope;
        let pushed = prepared
            .sync
            .push(scope, prepared.locations.clone(), cancel.clone())
            .await;
        match pushed {
            Ok(Push::Conflict) => {
                let recorded = prepared
                    .sync
                    .record_conflict(scope, prepared.locations, prepared.protected, None, cancel)
                    .await;
                if let Err(error) = recorded {
                    tracing::warn!(error = %error, "cannot record the cloud-save conflict");
                    self.pending(scope).await;
                }
            }
            Ok(outcome @ (Push::Offline | Push::QuotaExceeded)) => {
                tracing::info!(?outcome, "cloud saves kept locally");
                self.pending(scope).await;
            }
            Ok(outcome) => tracing::info!(?outcome, "cloud saves pushed"),
            Err(error) => {
                tracing::warn!(error = %error, "cloud-save push failed");
                self.pending(scope).await;
            }
        }
    }

    async fn pending(&self, scope: Scope) {
        if let Err(error) = self.store.mark_pending(scope).await {
            tracing::warn!(error = %error, "cannot mark cloud saves pending");
        }
    }
}

impl LaunchHooks for SaveHook {
    fn before_spawn<'a>(
        &'a self,
        ctx: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async move {
            // The budget cancels downloads and staging; a restore already
            // applying stops between files and rolls back before we return.
            let cancel = ctx.cancellation.child_token();
            let _guard = cancel.clone().drop_guard();
            let work = self.pull(ctx, cancel.clone());
            tokio::pin!(work);
            let result = tokio::select! {
                result = &mut work => result,
                () = tokio::time::sleep(PULL_BUDGET) => {
                    cancel.cancel();
                    work.await
                }
            };
            result.map(|()| HookEnv::default())
        })
    }

    fn after_exit<'a>(&'a self, ctx: &'a LaunchContext, _exit: &'a GameExit) -> HookFuture<'a, ()> {
        Box::pin(async move {
            let cancel = ctx.cancellation.child_token();
            let _guard = cancel.clone().drop_guard();
            self.push(ctx, cancel).await;
        })
    }

    fn aborted(&self, _ctx: &LaunchContext) {}

    /// Outlasts [`PULL_BUDGET`] so a cancelled restore can finish rolling back
    /// before the game may start.
    fn before_spawn_budget(&self) -> Duration {
        Duration::from_secs(60)
    }

    /// Uploads resume under the same commit key if this runs out.
    fn after_exit_budget(&self) -> Duration {
        Duration::from_secs(120)
    }
}

#[cfg(test)]
mod tests;
