//! The push half of 06 §3, run after the game's process tree exited: scan,
//! persist the commit with a stable operation id, upload missing blobs from
//! private hashed copies, then commit with the base as parent. Every failure
//! keeps local saves and leaves the package "sync pending".
use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::{
    BlobRef, BlobUploadTarget, CommitSnapshotRequest, OsFamily, PrepareBlobsRequest, SaveFile,
};

use super::{Error, Sync, offline_api, offline_transfer, scan_local};
use crate::db::saves::{PushOperation, Scope};
use crate::saves::restore;
use crate::saves::scan::{self, Location};

#[derive(Debug, PartialEq, Eq)]
pub enum Push {
    /// Nothing changed since the last synced state.
    Unchanged,
    /// The new snapshot is the cloud head and this device's base.
    Pushed { snapshot: Uuid },
    /// Another device pushed meanwhile: the player chooses.
    Conflict,
    /// The account's save quota is full: local saves are kept.
    QuotaExceeded,
    /// The server or storage could not be reached: retried later.
    Offline,
}

pub(super) const PLATFORM: OsFamily = if cfg!(windows) {
    OsFamily::Windows
} else if cfg!(target_os = "macos") {
    OsFamily::Macos
} else {
    OsFamily::Linux
};

impl Sync {
    /// After a session. `locations` come from the verified manifest only.
    pub async fn push(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        cancel: CancellationToken,
    ) -> Result<Push, Error> {
        self.transactions.recover(scope, locations.clone()).await?;
        let state = self.store.state(scope).await?;
        let local = scan_local(locations.clone(), state.files.clone(), cancel.clone()).await?;
        let same = |a: &SaveFile, b: &SaveFile| a.size == b.size && a.blake3 == b.blake3;
        let changed = local.len() != state.files.len()
            || local
                .iter()
                .any(|(key, file)| state.files.get(key).is_none_or(|old| !same(old, file)));
        if !changed && state.push.is_none() {
            return Ok(Push::Unchanged);
        }
        let request = CommitSnapshotRequest {
            parent_snapshot_id: state.base_snapshot_id,
            platform: PLATFORM,
            label: None,
            files: local.values().cloned().collect(),
        };
        // Persisted before any request: an identical retry reuses its key.
        let operation = self.store.begin_push(scope, request).await?;
        let result = self
            .push_operation(scope, &locations, &operation, &cancel)
            .await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(Error::Api(error)) if offline_api(&error) => Ok(Push::Offline),
            Err(Error::Api(error)) if problem(&error) == Some("save_quota_exceeded") => {
                Ok(Push::QuotaExceeded)
            }
            Err(Error::Transfer(error)) if offline_transfer(&error) => Ok(Push::Offline),
            Err(error) => Err(error),
        }
    }

    pub(super) async fn push_operation(
        &self,
        scope: Scope,
        locations: &Arc<Vec<Location>>,
        operation: &PushOperation,
        cancel: &CancellationToken,
    ) -> Result<Push, Error> {
        let package = scope.package.package_id;
        // One extra round when the server says a prepared blob never arrived.
        for attempt in 0..2 {
            self.upload_missing(scope, locations, &operation.request, cancel)
                .await?;
            match self
                .http
                .commit(package, &operation.request, operation.id)
                .await
            {
                Ok(snapshot) => {
                    let id = snapshot.summary.id;
                    if !self.store.finish_push(scope, operation.id, id).await? {
                        // A newer push replaced this one; its own commit wins.
                        return Ok(Push::Unchanged);
                    }
                    return Ok(Push::Pushed { snapshot: id });
                }
                Err(error) => match problem(&error) {
                    Some("save_head_conflict") => return Ok(Push::Conflict),
                    Some("save_quota_exceeded") => return Ok(Push::QuotaExceeded),
                    Some("blob_not_uploaded") if attempt == 0 => continue,
                    _ => return Err(Error::Api(error)),
                },
            }
        }
        Err(Error::Invalid)
    }

    /// Prepares every distinct blob and uploads the ones the server lacks.
    async fn upload_missing(
        &self,
        scope: Scope,
        locations: &Arc<Vec<Location>>,
        request: &CommitSnapshotRequest,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let mut by_hash: BTreeMap<&str, &SaveFile> = BTreeMap::new();
        for file in &request.files {
            by_hash.entry(file.blake3.as_str()).or_insert(file);
        }
        let prepared = self
            .http
            .prepare(
                scope.package.package_id,
                &PrepareBlobsRequest {
                    blobs: by_hash
                        .values()
                        .map(|file| BlobRef {
                            blake3: file.blake3.clone(),
                            size: file.size,
                        })
                        .collect(),
                },
            )
            .await
            .map_err(Error::Api)?;
        if prepared.missing.is_empty() {
            return Ok(());
        }
        let staging = self.staging(scope).join("upload");
        crate::paths::create_private_dir(&staging)
            .map_err(|_| scan::Error::Io(std::io::Error::other("cannot create save staging")))?;
        let result = async {
            for target in &prepared.missing {
                // Only hashes this push asked for; the file is ours, not the server's choice.
                let file = by_hash
                    .get(target.blake3.as_str())
                    .copied()
                    .ok_or(Error::Invalid)?;
                self.upload_one(locations, file, target, &staging, cancel)
                    .await?;
            }
            Ok(())
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        result
    }

    async fn upload_one(
        &self,
        locations: &Arc<Vec<Location>>,
        file: &SaveFile,
        target: &BlobUploadTarget,
        staging: &std::path::Path,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let copy = staging.join(format!("{}.blob", Uuid::now_v7().simple()));
        let size = u64::try_from(file.size).map_err(|_| Error::Invalid)?;
        let staged = {
            let (locations, file, copy, cancel) = (
                locations.clone(),
                file.clone(),
                copy.clone(),
                cancel.clone(),
            );
            tokio::task::spawn_blocking(move || {
                restore::stage_copy(&locations, &file, &copy, &cancel)
            })
            .await
            .map_err(|_| scan::Error::Io(std::io::Error::other("save staging stopped")))??
        };
        // The game rewrote the file after the scan: push again later.
        if staged.size != size || staged.blake3 != target.blake3 {
            return Err(Error::Files(scan::Error::Changed));
        }
        self.blobs
            .upload(target, &copy, size, cancel)
            .await
            .map_err(Error::Transfer)
    }
}

pub(super) fn problem(error: &super::http::Error) -> Option<&str> {
    match error {
        super::http::Error::Api(error) => error.problem_code(),
        super::http::Error::Cancelled => None,
    }
}

#[cfg(test)]
mod tests;
