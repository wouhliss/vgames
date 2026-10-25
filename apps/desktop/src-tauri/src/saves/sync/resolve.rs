//! The file and server steps of a conflict choice (06-cloud-saves-notes
//! "Commands and summaries"). Each checks that the cloud head is still the one
//! the player saw; a changed head renews the conflict instead of overwriting.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::{CommitSnapshotRequest, SaveSnapshot, SaveSnapshotSummary};

use super::push::{PLATFORM, problem};
use super::{Error, Push, Sync, offline_api, offline_transfer, scan_local};
use crate::db::saves::Scope;
use crate::saves::policy::ProtectedInstalls;
use crate::saves::scan::{self, Location};

#[derive(Debug)]
pub enum Resolution {
    /// The chosen side is now both the cloud head and the local saves.
    Done,
    /// The cloud head moved meanwhile: the player chooses again.
    Renewed(Box<SaveSnapshotSummary>),
    /// The server no longer has saves for this package: nothing to choose.
    CloudGone,
    /// Unreachable: nothing changed, the conflict stays.
    Offline,
    /// The account's quota is full: nothing changed, the conflict stays.
    QuotaExceeded,
}

impl Sync {
    /// Keep cloud: restore the head over local saves, which are backed up first.
    pub async fn keep_cloud(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        expected: Uuid,
        cancel: CancellationToken,
    ) -> Result<Resolution, Error> {
        self.transactions.recover(scope, locations.clone()).await?;
        let head = match self.expect_head(scope, expected).await? {
            Ok(head) => head,
            Err(other) => return Ok(other),
        };
        let state = self.store.state(scope).await?;
        let local = scan_local(locations.clone(), state.files, cancel.clone()).await?;
        offline(
            self.restore(scope, &local, head.into(), locations, protected, cancel)
                .await
                .map(|_| Resolution::Done),
        )
    }

    /// Keep this device: push the current local files over the head.
    pub async fn keep_device(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        expected: Uuid,
        cancel: CancellationToken,
    ) -> Result<Resolution, Error> {
        self.transactions.recover(scope, locations.clone()).await?;
        let state = self.store.state(scope).await?;
        let local = scan_local(locations.clone(), state.files, cancel.clone()).await?;
        let request = CommitSnapshotRequest {
            // The server refuses this commit if the head moved.
            parent_snapshot_id: Some(expected),
            platform: PLATFORM,
            label: None,
            files: local.into_values().collect(),
        };
        let operation = self.store.begin_push(scope, request).await?;
        let pushed = self
            .push_operation(scope, &locations, &operation, &cancel)
            .await;
        match pushed {
            Ok(Push::Pushed { .. } | Push::Unchanged) => Ok(Resolution::Done),
            Ok(Push::QuotaExceeded) => Ok(Resolution::QuotaExceeded),
            Ok(Push::Offline) => Ok(Resolution::Offline),
            Ok(Push::Conflict) => match self.expect_head(scope, expected).await? {
                Err(other) => Ok(other),
                // The head is what we expected yet the commit was refused.
                Ok(_) => Err(Error::Invalid),
            },
            Err(Error::Api(error)) if problem(&error) == Some("save_quota_exceeded") => {
                Ok(Resolution::QuotaExceeded)
            }
            other => offline(other.map(|_| Resolution::Done)),
        }
    }

    /// Keep both: keep the head's files in a local backup, then push the
    /// device's files over it. History keeps both sides.
    pub async fn keep_both(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        expected: Uuid,
        cancel: CancellationToken,
    ) -> Result<Resolution, Error> {
        let head = match self.expect_head(scope, expected).await? {
            Ok(head) => head,
            Err(other) => return Ok(other),
        };
        let staging = self.staging(scope).join("keep-both");
        crate::paths::create_private_dir(&staging)
            .map_err(|_| scan::Error::Io(std::io::Error::other("cannot create save staging")))?;
        let kept = async {
            self.download(scope, &head.files, &staging, &cancel).await?;
            self.transactions
                .protect_staged(
                    scope,
                    locations.clone(),
                    protected,
                    head.files,
                    staging.clone(),
                    cancel.clone(),
                )
                .await?;
            Ok(())
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        if let Err(error) = kept {
            return offline(Err(error));
        }
        self.keep_device(scope, locations, expected, cancel).await
    }

    /// The current head when it is still `expected`; otherwise what the
    /// resolution must report instead.
    async fn expect_head(
        &self,
        scope: Scope,
        expected: Uuid,
    ) -> Result<Result<SaveSnapshot, Resolution>, Error> {
        match self.http.head(scope.package.package_id).await {
            Ok(Some(head)) if head.summary.id == expected => Ok(Ok(head)),
            Ok(Some(head)) => Ok(Err(Resolution::Renewed(Box::new(head.summary)))),
            Ok(None) => Ok(Err(Resolution::CloudGone)),
            Err(error) if offline_api(&error) => Ok(Err(Resolution::Offline)),
            Err(error) => Err(Error::Api(error)),
        }
    }
}

fn offline(result: Result<Resolution, Error>) -> Result<Resolution, Error> {
    match result {
        Err(Error::Transfer(error)) if offline_transfer(&error) => Ok(Resolution::Offline),
        Err(Error::Api(error)) if offline_api(&error) => Ok(Resolution::Offline),
        other => other,
    }
}
