//! Recording a conflict (06-cloud-saves-notes "Commands and summaries"): the
//! device's current saves are backed up before anything else, so whichever side
//! the player later keeps, the other stays reachable. Nothing is resolved here.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::SaveSnapshotSummary;

use super::{Error, Sync, scan_local};
use crate::db::saves::Scope;
use crate::saves::policy::ProtectedInstalls;
use crate::saves::scan::Location;

impl Sync {
    /// Opens (or refreshes) the scope's conflict against `head`, or against the
    /// server's current head when `None` (a commit lost the race). Returns the
    /// conflict id, or `None` when the server has no saves left to conflict with.
    pub async fn record_conflict(
        &self,
        scope: Scope,
        locations: Arc<Vec<Location>>,
        protected: Arc<ProtectedInstalls>,
        head: Option<SaveSnapshotSummary>,
        cancel: CancellationToken,
    ) -> Result<Option<Uuid>, Error> {
        let head = match head {
            Some(head) => head,
            None => match self
                .http
                .head(scope.package.package_id)
                .await
                .map_err(Error::Api)?
            {
                Some(head) => head.summary,
                None => return Ok(None),
            },
        };
        // The first detection's backup keeps protecting the device side.
        let backup = match self.store.open_conflict_for(scope).await? {
            Some(open) => open.device_backup_id,
            None => {
                let state = self.store.state(scope).await?;
                let local = scan_local(locations.clone(), state.files, cancel.clone()).await?;
                self.transactions
                    .protect(
                        scope,
                        locations,
                        protected,
                        local.into_values().collect(),
                        cancel,
                    )
                    .await?
            }
        };
        Ok(Some(self.store.open_conflict(scope, head, backup).await?))
    }
}
