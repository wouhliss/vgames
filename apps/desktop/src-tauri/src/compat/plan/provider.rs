//! INS-02's catalog [`CompatProvider`], answered by the planner (GAME-07), so
//! package details show what a launch would do: the verified profile's status and
//! notes for the release that would be installed, and the blockers
//! `needs_rosetta`, `rosetta_sunset` and `d3d12_unsupported_on_mac` (from the D3D
//! scan recorded for that version; an unscanned build is checked while it
//! downloads). The runtime catalog is read, never refreshed, here (idle means idle).

use vgames_core::compat::Status;
use vgames_proto::packages::Platform as WirePlatform;

use super::*;
use crate::catalog::compat::{
    CompatProfile as CatalogProfile, CompatProvider, CompatQuery, ProfileFuture,
};
use crate::catalog::release::Route;
use crate::catalog::{CompatBlocker, CompatStatus};
use crate::compat::d3d;

/// Apple's last macOS with full Rosetta 2 (09 §3), when the catalog says nothing newer.
pub const APPLE_LAST_FULL_ROSETTA: &str = "27";

impl<S: ProfileSource, P: Probes + Send + Sync + 'static> CompatProvider for CompatPlanner<S, P> {
    fn profile(&self, query: CompatQuery) -> ProfileFuture<'_> {
        Box::pin(self.catalog_profile(query))
    }
}

impl<S: ProfileSource, P: Probes + Send + Sync + 'static> CompatPlanner<S, P> {
    async fn catalog_profile(&self, query: CompatQuery) -> Option<CatalogProfile> {
        let (target, through_rosetta) = match query.route {
            Route::Native | Route::WindowsEmulation => return None,
            Route::Rosetta => (None, true),
            Route::Proton => (Some(Target::Linux), false),
            Route::Wine { needs_rosetta } => (Some(Target::Macos), needs_rosetta),
        };
        let mut blockers = Vec::new();
        if matches!(query.route, Route::Wine { .. })
            && let Ok(Some(imports)) =
                d3d::recorded(&self.db, query.package, query.version_id).await
            && let Some(blocker) = d3d::blocker_for(&imports)
        {
            blockers.push(blocker);
        }
        if through_rosetta {
            let probes = Arc::clone(&self.probes);
            let runs = tokio::task::spawn_blocking(move || probes.x86_64_runs())
                .await
                .unwrap_or(false);
            if !runs {
                blockers.push(CompatBlocker::NeedsRosetta);
            }
            let wine_last = match query.route {
                Route::Wine { .. } => {
                    let catalog = self.catalog.current().await.ok().flatten();
                    rosetta_last_macos(catalog.as_ref().map(|a| a.catalog.as_ref()))
                }
                _ => None,
            };
            blockers.push(CompatBlocker::RosettaSunset {
                last_macos: wine_last.unwrap_or_else(|| APPLE_LAST_FULL_ROSETTA.to_owned()),
            });
        }
        let profile = match (target, query.platform) {
            (Some(target), WirePlatform::WindowsX86_64) => {
                let release = ExpectedRelease {
                    server_id: query.package.server_id,
                    package_id: query.package.package_id,
                    version_id: query.version_id,
                    platform: Platform::WindowsX86_64,
                    sequence: query.sequence,
                };
                self.profile(query.package, target, &release).await
            }
            _ => None,
        };
        let (status, notes) = profile.map_or((CompatStatus::Untested, None), |p| {
            (status_of(p.status), p.notes)
        });
        Some(CatalogProfile {
            status,
            notes,
            blockers,
        })
    }
}

const fn status_of(status: Status) -> CompatStatus {
    match status {
        Status::Verified => CompatStatus::Verified,
        Status::Playable => CompatStatus::Playable,
        Status::Unsupported => CompatStatus::Unsupported,
        Status::Untested => CompatStatus::Untested,
    }
}
