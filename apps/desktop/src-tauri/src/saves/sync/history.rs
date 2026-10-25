//! Cloud history and restoring an earlier snapshot (06-cloud-saves-notes
//! "Commands and summaries"). A restored snapshot becomes this device's
//! progress on top of the current head, so the next push keeps history linear.
use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::SaveSnapshotSummary;

use super::{Error, RestoreTarget, Sync, offline_api, offline_transfer, scan_local};
use crate::db::saves::{RestoredBase, Scope};
use crate::saves::policy::ProtectedInstalls;
use crate::saves::scan::Location;

/// History pages read per request, newest first (100 snapshots each).
const MAX_PAGES: usize = 5;

#[derive(Debug)]
pub struct CloudHistory {
    pub head: Option<Uuid>,
    /// Newest first, at most `MAX_PAGES` pages.
    pub snapshots: Vec<SaveSnapshotSummary>,
}

impl Sync {
    /// The server's snapshots of the package. `None` when unreachable.
    pub async fn history(&self, scope: Scope) -> Result<Option<CloudHistory>, Error> {
        let package = scope.package.package_id;
        let head = match self.http.head(package).await {
            Ok(head) => head.map(|head| head.summary.id),
            Err(error) if offline_api(&error) => return Ok(None),
            Err(error) => return Err(Error::Api(error)),
        };
        let mut snapshots = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = match self.http.history_page(package, cursor.as_deref()).await {
                Ok(page) => page,
                Err(error) if offline_api(&error) => return Ok(None),
                Err(error) => return Err(Error::Api(error)),
            };
            snapshots.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(Some(CloudHistory { head, snapshots }))
    }

    /// Writes snapshot `id`'s files over local saves (backed up first). The
    /// current head stays the base, so they are pushed after the next session.
    /// `Ok(false)` when the server or storage is unreachable: nothing changed.
    pub async fn restore_snapshot(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        id: Uuid,
        cancel: CancellationToken,
    ) -> Result<bool, Error> {
        self.transactions.recover(scope, locations.clone()).await?;
        let package = scope.package.package_id;
        let head = match self.http.head(package).await {
            Ok(head) => head,
            Err(error) if offline_api(&error) => return Ok(false),
            Err(error) => return Err(Error::Api(error)),
        };
        let base = match &head {
            Some(head) => RestoredBase {
                snapshot_id: Some(head.summary.id),
                files: head
                    .files
                    .iter()
                    .map(|file| (format!("{}/{}", file.root, file.path), file.clone()))
                    .collect(),
            },
            None => RestoredBase {
                snapshot_id: None,
                files: BTreeMap::new(),
            },
        };
        let head_id = base.snapshot_id;
        let snapshot = match head.filter(|head| head.summary.id == id) {
            Some(head) => head,
            None => match self.http.snapshot(package, id).await {
                Ok(snapshot) => snapshot,
                Err(error) if offline_api(&error) => return Ok(false),
                Err(error) => return Err(Error::Api(error)),
            },
        };
        let state = self.store.state(scope).await?;
        let local = scan_local(locations.clone(), state.files, cancel.clone()).await?;
        match self
            .restore(
                scope,
                &local,
                RestoreTarget {
                    snapshot,
                    base: Some(base),
                },
                locations,
                protected,
                cancel,
            )
            .await
        {
            Ok(_) => {
                // Not the head: these files are progress still to push.
                if head_id != Some(id) {
                    self.store.mark_pending(scope).await?;
                }
                Ok(true)
            }
            Err(Error::Transfer(error)) if offline_transfer(&error) => Ok(false),
            Err(Error::Api(error)) if offline_api(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Sync {
    /// Puts back what local backup `id` saved; the result is this device's
    /// progress, pushed after the next session.
    pub async fn restore_backup(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        id: Uuid,
        cancel: CancellationToken,
    ) -> Result<(), Error> {
        self.transactions
            .restore_backup(scope, locations, protected, id, cancel)
            .await?;
        self.store.mark_pending(scope).await?;
        Ok(())
    }
}
