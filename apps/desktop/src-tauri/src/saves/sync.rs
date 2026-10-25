//! The pull half of 06 §3, run before a launch: recover interrupted restores,
//! read the cloud head, scan local saves and apply the decision table. A
//! restore downloads only verified blobs and goes through a recorded
//! transaction; a conflict is reported, never resolved here. Offline pulls
//! leave local saves alone and mark the package "sync pending".
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::{BlobDownloadUrlsRequest, SaveFile, SaveSnapshot};

use super::blobs::{self, Blobs};
use super::http;
use super::policy::ProtectedInstalls;
use super::scan::{self, Location};
use super::transaction::{self, Request, Transactions};
use crate::api::ApiError;
use crate::db::DbError;
use crate::db::saves::{BackupReason, PullDecision, RestoredBase, Scope, Store, decide_pull};

mod conflict;
mod history;
mod push;
mod resolve;
pub use history::CloudHistory;
pub use push::Push;
pub use resolve::Resolution;

/// `blobs/download-urls` accepts at most this many hashes per call.
const URL_BATCH: usize = 1000;

#[derive(Debug)]
pub enum Pull {
    /// Local saves already equal the cloud head.
    Unchanged,
    /// Only this device changed: pushed after the session.
    LocalProgress,
    /// The cloud head was restored over local saves (backed up first).
    Restored { snapshot: Uuid },
    /// Both changed. The launch is vetoed until the player chooses.
    Conflict { head: Box<SaveSnapshot> },
    /// The server or storage could not be reached: launch with local saves.
    Offline,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Transaction(#[from] transaction::Error),
    #[error(transparent)]
    Files(#[from] scan::Error),
    #[error("cannot read the cloud-save state")]
    Db(#[from] DbError),
    #[error(transparent)]
    Api(http::Error),
    #[error(transparent)]
    Transfer(blobs::Error),
    #[error("the cloud-save server sent an invalid response")]
    Invalid,
}

/// What a restore writes, and the sync base afterwards (`None`: the snapshot).
struct RestoreTarget {
    snapshot: SaveSnapshot,
    base: Option<RestoredBase>,
}

impl From<SaveSnapshot> for RestoreTarget {
    fn from(snapshot: SaveSnapshot) -> Self {
        Self {
            snapshot,
            base: None,
        }
    }
}

#[derive(Clone)]
pub struct Sync {
    http: http::Client,
    blobs: Blobs,
    transactions: Transactions,
    store: Store,
    /// `<app data>/save-staging`.
    staging: PathBuf,
}

impl Sync {
    pub fn new(
        http: http::Client,
        blobs: Blobs,
        transactions: Transactions,
        store: Store,
        data_dir: &std::path::Path,
    ) -> Self {
        Self {
            http,
            blobs,
            transactions,
            store,
            staging: data_dir.join("save-staging"),
        }
    }

    fn staging(&self, scope: Scope) -> PathBuf {
        self.staging
            .join(scope.package.server_id.to_string())
            .join(scope.package.package_id.to_string())
            .join(scope.user_id.to_string())
    }

    /// Before a launch. `locations` come from the verified manifest only.
    pub async fn pull(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        cancel: CancellationToken,
    ) -> Result<Pull, Error> {
        self.transactions.recover(scope, locations.clone()).await?;
        let head = match self.http.head(scope.package.package_id).await {
            Ok(head) => head,
            Err(error) if offline_api(&error) => return self.offline(scope).await,
            Err(error) => return Err(Error::Api(error)),
        };
        let state = self.store.state(scope).await?;
        let local = scan_local(locations.clone(), state.files.clone(), cancel.clone()).await?;
        let Some(head) = head else {
            // The server has no saves. A device that once synced never deletes
            // local saves because of that: its progress is pushed instead.
            return Ok(match decide_pull(&state, &local, None) {
                PullDecision::Unchanged => Pull::Unchanged,
                _ => Pull::LocalProgress,
            });
        };
        match decide_pull(&state, &local, Some(head.summary.id)) {
            PullDecision::Unchanged => Ok(Pull::Unchanged),
            PullDecision::LocalProgress => Ok(Pull::LocalProgress),
            PullDecision::Conflict => Ok(Pull::Conflict {
                head: Box::new(head),
            }),
            PullDecision::Restore => {
                match self
                    .restore(scope, &local, head.into(), locations, protected, cancel)
                    .await
                {
                    Err(Error::Transfer(error)) if offline_transfer(&error) => {
                        self.offline(scope).await
                    }
                    Err(Error::Api(error)) if offline_api(&error) => self.offline(scope).await,
                    other => other,
                }
            }
        }
    }

    async fn offline(&self, scope: Scope) -> Result<Pull, Error> {
        self.store.mark_pending(scope).await?;
        Ok(Pull::Offline)
    }

    async fn restore(
        &self,
        scope: Scope,
        local: &BTreeMap<String, SaveFile>,
        target: RestoreTarget,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        cancel: CancellationToken,
    ) -> Result<Pull, Error> {
        let RestoreTarget {
            snapshot: head,
            base,
        } = target;
        let snapshot = head.summary.id;
        let files: BTreeMap<String, SaveFile> = head
            .files
            .into_iter()
            .map(|file| (format!("{}/{}", file.root, file.path), file))
            .collect();
        // Two entries for one root/path would collapse into one here.
        if usize::try_from(head.summary.file_count).ok() != Some(files.len()) {
            return Err(Error::Invalid);
        }
        let writes: Vec<SaveFile> = files
            .iter()
            .filter(|(key, file)| {
                local
                    .get(*key)
                    .is_none_or(|have| have.size != file.size || have.blake3 != file.blake3)
            })
            .map(|(_, file)| file.clone())
            .collect();
        let deletes: Vec<SaveFile> = local
            .iter()
            .filter(|(key, _)| !files.contains_key(*key))
            .map(|(_, file)| file.clone())
            .collect();
        let staging = self.staging(scope);
        crate::paths::create_private_dir(&staging)
            .map_err(|_| scan::Error::Io(std::io::Error::other("cannot create save staging")))?;
        let result = async {
            self.download(scope, &writes, &staging, &cancel).await?;
            self.transactions
                .restore(
                    scope,
                    locations,
                    protected,
                    Request {
                        writes,
                        deletes,
                        staging: staging.clone(),
                        reason: BackupReason::Restore,
                        base: Some(base.unwrap_or(RestoredBase {
                            snapshot_id: Some(snapshot),
                            files,
                        })),
                    },
                    cancel,
                )
                .await?;
            Ok(Pull::Restored { snapshot })
        }
        .await;
        // Staged blobs are only useful to this restore.
        let _ = tokio::fs::remove_dir_all(&staging).await;
        result
    }

    /// Downloads every distinct blob the restore writes into `staging`.
    async fn download(
        &self,
        scope: Scope,
        writes: &[SaveFile],
        staging: &std::path::Path,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let mut sizes = BTreeMap::new();
        for file in writes {
            let size = u64::try_from(file.size).map_err(|_| Error::Invalid)?;
            if sizes
                .insert(file.blake3.clone(), size)
                .is_some_and(|old| old != size)
            {
                return Err(Error::Invalid);
            }
        }
        let wanted: Vec<String> = sizes.keys().cloned().collect();
        for batch in wanted.chunks(URL_BATCH) {
            let urls = self
                .http
                .download_urls(
                    scope.package.package_id,
                    &BlobDownloadUrlsRequest {
                        blake3: batch.to_vec(),
                    },
                )
                .await
                .map_err(Error::Api)?;
            // Exactly the requested hashes, each once.
            let asked: BTreeSet<&String> = batch.iter().collect();
            let got: BTreeSet<&String> = urls.items.iter().map(|item| &item.blake3).collect();
            if urls.items.len() != batch.len() || got != asked {
                return Err(Error::Invalid);
            }
            for item in &urls.items {
                let size = sizes.get(&item.blake3).copied().ok_or(Error::Invalid)?;
                self.blobs
                    .download(item, size, staging, cancel)
                    .await
                    .map_err(Error::Transfer)?;
            }
        }
        Ok(())
    }
}

async fn scan_local(
    locations: Arc<Vec<Location>>,
    base: BTreeMap<String, SaveFile>,
    cancel: CancellationToken,
) -> Result<BTreeMap<String, SaveFile>, Error> {
    let scanned = tokio::task::spawn_blocking(move || scan::scan(&locations, &base, &cancel))
        .await
        .map_err(|_| scan::Error::Io(std::io::Error::other("save scan stopped")))??;
    Ok(scanned.files)
}

fn offline_api(error: &http::Error) -> bool {
    matches!(
        error,
        http::Error::Api(ApiError::Timeout | ApiError::Network(_) | ApiError::Tls(_))
    )
}

fn offline_transfer(error: &blobs::Error) -> bool {
    matches!(
        error,
        blobs::Error::Network | blobs::Error::Timeout | blobs::Error::Status(500..=599)
    )
}

#[cfg(test)]
pub(crate) mod tests;
