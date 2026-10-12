//! The compatibility layer of a package on this machine, and the hook GAME-07
//! fills with signed compat profiles. Without a provider every compat route is
//! `untested` with no notes and no blockers.

use std::future::Future;
use std::pin::Pin;

use super::release::Route;
use super::types::{CompatBlocker, CompatInfo, CompatRunner, CompatStatus, ProtonDbTier};
use crate::events::PackageRef;
use uuid::Uuid;
use vgames_proto::packages::{Platform, ReleaseInfo};

/// What a provider is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatQuery {
    pub package: PackageRef,
    /// The build that would be installed.
    pub platform: Platform,
    /// The release that would be installed (profiles' `applies_to`, recorded D3D scans).
    pub version_id: Uuid,
    pub sequence: u64,
    pub route: Route,
}

/// A provider's answer (from a verified compat profile).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatProfile {
    pub status: CompatStatus,
    /// Plain text.
    pub notes: Option<String>,
    pub blockers: Vec<CompatBlocker>,
}

pub type ProfileFuture<'a> = Pin<Box<dyn Future<Output = Option<CompatProfile>> + Send + 'a>>;

/// Enriches compat routes (GAME-07). Called for Rosetta, Proton and Wine
/// routes only; native builds never ask.
pub trait CompatProvider: Send + Sync + 'static {
    fn profile(&self, query: CompatQuery) -> ProfileFuture<'_>;
}

/// The default: nothing known.
pub struct NoProfiles;

impl CompatProvider for NoProfiles {
    fn profile(&self, _query: CompatQuery) -> ProfileFuture<'_> {
        Box::pin(async { None })
    }
}

/// The compat layer for `route`, enriched by `provider`.
pub async fn compat_info(
    provider: &dyn CompatProvider,
    package: PackageRef,
    selected: Option<(&ReleaseInfo, Route)>,
    protondb_tier: Option<ProtonDbTier>,
) -> CompatInfo {
    let Some((release, route)) = selected else {
        return CompatInfo::Unavailable;
    };
    let layer = match route {
        Route::Native | Route::WindowsEmulation => return CompatInfo::Native,
        Route::Rosetta => None,
        Route::Proton => Some(CompatRunner::Proton),
        Route::Wine { .. } => Some(CompatRunner::Wine),
    };
    let profile = provider
        .profile(CompatQuery {
            package,
            platform: release.platform,
            version_id: release.version_id,
            sequence: u64::try_from(release.sequence).unwrap_or(0),
            route,
        })
        .await;
    let (status, notes, blockers) = match profile {
        Some(p) => (p.status, p.notes, p.blockers),
        None => (CompatStatus::Untested, None, Vec::new()),
    };
    match layer {
        None => CompatInfo::Rosetta { blockers },
        Some(layer) => CompatInfo::Compat {
            layer,
            status,
            notes,
            // Community data, Proton only (09 §4).
            protondb_tier: protondb_tier.filter(|_| layer == CompatRunner::Proton),
            blockers,
        },
    }
}
