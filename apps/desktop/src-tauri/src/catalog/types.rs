//! What the catalog commands return (`src/bindings.ts`). Shapes follow the
//! UI contract (`src/ipc/contract/catalog.ts` before INS-02 generated them).

use serde::{Deserialize, Serialize};
use specta::Type;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vgames_proto::packages as proto;

use super::release::Route;

/// A package build's target (OpenAPI `Platform`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub enum Platform {
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "windows-aarch64")]
    WindowsAarch64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
    #[serde(rename = "macos-aarch64")]
    MacosAarch64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
}

impl From<proto::Platform> for Platform {
    fn from(platform: proto::Platform) -> Self {
        match platform {
            proto::Platform::WindowsX86_64 => Self::WindowsX86_64,
            proto::Platform::WindowsAarch64 => Self::WindowsAarch64,
            proto::Platform::LinuxX86_64 => Self::LinuxX86_64,
            proto::Platform::LinuxAarch64 => Self::LinuxAarch64,
            proto::Platform::MacosAarch64 => Self::MacosAarch64,
            proto::Platform::MacosX86_64 => Self::MacosX86_64,
        }
    }
}

/// How the package would run on this machine, or why it can't (09 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Native,
    Rosetta,
    Proton,
    Wine,
    Unavailable,
}

impl Availability {
    pub fn of(route: Option<Route>) -> Self {
        match route {
            Some(Route::Native | Route::WindowsEmulation) => Self::Native,
            Some(Route::Rosetta) => Self::Rosetta,
            Some(Route::Proton) => Self::Proton,
            Some(Route::Wine { .. }) => Self::Wine,
            None => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSort {
    Title,
    Recent,
}

impl CatalogSort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Recent => "recent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct CatalogQuery {
    /// Title search, 0–100 characters (`q`).
    pub query: String,
    pub genre: Option<String>,
    pub sort: CatalogSort,
    /// From the previous page; null for the first.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct CatalogItem {
    pub package_id: String,
    pub slug: String,
    pub title: String,
    pub summary: Option<String>,
    pub genres: Vec<String>,
    /// `vgimg:` URL served by the Rust core.
    pub cover_url: Option<String>,
    pub platforms: Vec<Platform>,
    pub availability: Availability,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct GenreCount {
    pub genre: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogError {
    /// The package doesn't exist anymore, or isn't published.
    #[error("the package does not exist")]
    NotFound,
    #[error("the server cannot be reached")]
    Offline,
    #[error("sign-in required")]
    Unauthenticated,
    #[error("{message} ({code})")]
    Server { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct Screenshot {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// The release this machine would install (native first, then a compatibility layer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct HostRelease {
    pub platform: Platform,
    pub version_id: String,
    pub version_label: String,
    pub sequence: u64,
    /// Bytes of the whole release (the installed size).
    pub total_size: u64,
    pub published_at: String,
    pub via: Availability,
}

impl HostRelease {
    pub fn new(release: &proto::ReleaseInfo, route: Route) -> Self {
        Self {
            platform: release.platform.into(),
            version_id: release.version_id.to_string(),
            version_label: release.version_label.clone(),
            sequence: u64::try_from(release.sequence).unwrap_or(0),
            total_size: u64::try_from(release.total_size).unwrap_or(0),
            published_at: rfc3339(release.published_at),
            via: Availability::of(Some(route)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CompatStatus {
    Verified,
    Playable,
    Unsupported,
    Untested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProtonDbTier {
    Platinum,
    Gold,
    Silver,
    Bronze,
    Borked,
    Pending,
}

impl From<proto::ProtonDbTier> for ProtonDbTier {
    fn from(tier: proto::ProtonDbTier) -> Self {
        match tier {
            proto::ProtonDbTier::Platinum => Self::Platinum,
            proto::ProtonDbTier::Gold => Self::Gold,
            proto::ProtonDbTier::Silver => Self::Silver,
            proto::ProtonDbTier::Bronze => Self::Bronze,
            proto::ProtonDbTier::Borked => Self::Borked,
            proto::ProtonDbTier::Pending => Self::Pending,
        }
    }
}

/// Something that stops (or will stop) a compat launch on this machine (09 §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatBlocker {
    /// A DirectX 12 title on a Mac: D3DMetal is deferred, so no Mac runs DX12 yet.
    D3d12UnsupportedOnMac,
    /// Rosetta 2 isn't installed; `rosetta_install` installs it after the user confirms.
    NeedsRosetta,
    /// x86_64-only path that Apple's Rosetta policy may end. Not blocking.
    RosettaSunset { last_macos: String },
}

impl CompatBlocker {
    /// Refuses an install up front (`install_plan`, `install_start`).
    pub fn is_hard(&self) -> bool {
        matches!(self, Self::D3d12UnsupportedOnMac | Self::NeedsRosetta)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CompatRunner {
    Proton,
    Wine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatInfo {
    Native,
    /// An Intel macOS build on Apple silicon, through Rosetta 2.
    Rosetta {
        blockers: Vec<CompatBlocker>,
    },
    Compat {
        layer: CompatRunner,
        /// From the signed compat profile; `untested` without one.
        status: CompatStatus,
        /// Plain text from the signed profile.
        notes: Option<String>,
        /// Community rating, informational only (never used to decide anything).
        protondb_tier: Option<ProtonDbTier>,
        blockers: Vec<CompatBlocker>,
    },
    Unavailable,
}

impl CompatInfo {
    pub fn blockers(&self) -> &[CompatBlocker] {
        match self {
            Self::Rosetta { blockers } | Self::Compat { blockers, .. } => blockers,
            Self::Native | Self::Unavailable => &[],
        }
    }

    pub fn hard_blocker(&self) -> Option<&CompatBlocker> {
        self.blockers().iter().find(|b| b.is_hard())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct PackageDetails {
    pub package_id: String,
    pub slug: String,
    pub title: String,
    pub summary: Option<String>,
    /// CommonMark from the server's admins; render only through SafeMarkdown.
    pub description: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    /// YYYY-MM-DD
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub platforms: Vec<Platform>,
    pub cover_url: Option<String>,
    pub hero_url: Option<String>,
    pub logo_url: Option<String>,
    pub screenshots: Vec<Screenshot>,
    /// Null when no release can run here (see `compat.kind === "unavailable"`).
    pub release: Option<HostRelease>,
    pub compat: CompatInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct InstallPlan {
    pub package_id: String,
    pub release: HostRelease,
    /// Bytes to download.
    pub download_bytes: u64,
    /// Free space needed on the chosen library: total size + 64 MiB (02 §7).
    pub required_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallPlanError {
    #[error("the package does not exist")]
    NotFound,
    /// No build for this machine and no compatibility path.
    #[error("no build runs on this computer")]
    NoRelease,
    #[error("the package is already installed")]
    AlreadyInstalled,
    #[error("the server cannot be reached")]
    Offline,
    #[error("the package cannot run on this computer")]
    Blocked { blocker: CompatBlocker },
    #[error("{message} ({code})")]
    Server { code: String, message: String },
}

impl From<CatalogError> for InstallPlanError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::NotFound => Self::NotFound,
            CatalogError::Offline => Self::Offline,
            CatalogError::Unauthenticated => Self::Server {
                code: "unauthenticated".into(),
                message: "Sign in again to install".into(),
            },
            CatalogError::Server { code, message } => Self::Server { code, message },
        }
    }
}

pub fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}
