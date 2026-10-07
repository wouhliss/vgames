//! What the publish screen sees (`src/ipc/contract/publishing.ts` until the bindings carry it).

use serde::{Deserialize, Serialize};
use specta::Type;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vgames_proto::packages::{AdminPackage, PackageStatus as WireStatus, Platform as WirePlatform};
use vgames_proto::versions::{Version, VersionState as WireState};

use crate::api::ApiError;
use crate::catalog::types::Platform;
use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Draft,
    Published,
    Hidden,
    Archived,
}

/// A package as the admin API returns it (any status). Text is plain text from the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct PublishPackage {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: PackageStatus,
    /// Platforms with a current release (from the package's `releases`).
    pub released_platforms: Vec<Platform>,
}

impl From<AdminPackage> for PublishPackage {
    fn from(package: AdminPackage) -> Self {
        Self {
            id: package.detail.summary.id,
            slug: package.detail.summary.slug,
            title: package.detail.summary.title,
            status: match package.status {
                WireStatus::Draft => PackageStatus::Draft,
                WireStatus::Published => PackageStatus::Published,
                WireStatus::Hidden => PackageStatus::Hidden,
                WireStatus::Archived => PackageStatus::Archived,
            },
            released_platforms: package
                .detail
                .releases
                .iter()
                .map(|r| r.platform.into())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct PublishPackagePage {
    pub items: Vec<PublishPackage>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
pub struct PackageCreate {
    pub title: String,
    /// Derived from the title by the server when null.
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    Uploading,
    Verifying,
    Ready,
    Published,
    Failed,
    Yanked,
    Aborted,
}

impl From<WireState> for VersionState {
    fn from(state: WireState) -> Self {
        match state {
            WireState::Uploading => Self::Uploading,
            WireState::Verifying => Self::Verifying,
            WireState::Ready => Self::Ready,
            WireState::Published => Self::Published,
            WireState::Failed => Self::Failed,
            WireState::Yanked => Self::Yanked,
            WireState::Aborted => Self::Aborted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct PublishVersion {
    pub id: Uuid,
    pub package_id: Uuid,
    pub platform: Platform,
    pub sequence: i64,
    pub version_label: String,
    pub state: VersionState,
    pub is_current_release: bool,
    /// Set when `state` is `failed` (plain text from the server).
    pub failure_reason: Option<String>,
    pub total_size: Option<i64>,
    /// 0–1 while `verifying`.
    pub verify_progress: Option<f64>,
    /// RFC 3339.
    pub created_at: String,
    pub published_at: Option<String>,
    pub yanked_at: Option<String>,
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

impl From<Version> for PublishVersion {
    fn from(v: Version) -> Self {
        Self {
            id: v.id,
            package_id: v.package_id,
            platform: v.platform.into(),
            sequence: v.sequence,
            version_label: v.version_label,
            state: v.state.into(),
            is_current_release: v.is_current_release.unwrap_or(false),
            failure_reason: v.failure_reason,
            total_size: v.total_size,
            verify_progress: v.verify_progress.map(f64::from),
            created_at: rfc3339(v.created_at),
            published_at: v.published_at.map(rfc3339),
            yanked_at: v.yanked_at.map(rfc3339),
        }
    }
}

impl From<Platform> for WirePlatform {
    fn from(platform: Platform) -> Self {
        match platform {
            Platform::WindowsX86_64 => Self::WindowsX86_64,
            Platform::WindowsAarch64 => Self::WindowsAarch64,
            Platform::LinuxX86_64 => Self::LinuxX86_64,
            Platform::LinuxAarch64 => Self::LinuxAarch64,
            Platform::MacosAarch64 => Self::MacosAarch64,
            Platform::MacosX86_64 => Self::MacosX86_64,
        }
    }
}

/// Why an entry can't be packed (02-package-format §2, `vgames-core` path rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InvalidPathReason {
    Symlink,
    SpecialFile,
    NotUtf8,
    NotNfc,
    BadStructure,
    TooLong,
    ForbiddenCharacter,
    TrailingDotOrSpace,
    ReservedName,
    ReservedFolder,
    UnsafeCompatibilityForm,
    CaseCollision,
    FileIsFolder,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct InvalidPath {
    pub path: String,
    pub reason: InvalidPathReason,
    /// The other path of a case collision.
    pub other: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct PublishPlan {
    pub folder: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub pack_count: u32,
    /// Files that look runnable, for the launch picker.
    pub executables: Vec<String>,
    /// Non-empty = publishing is blocked. At most [`super::plan::MAX_LISTED`]; `invalid_count` is the total.
    pub invalid_paths: Vec<InvalidPath>,
    pub invalid_count: u64,
}

/// What the game runs when a player presses Play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PublishLaunch {
    /// Relative path inside the folder, `/`-separated.
    pub executable: String,
    pub args: Vec<String>,
    /// Relative to the folder; null = the executable's folder.
    pub working_dir: Option<String>,
}

#[derive(Deserialize, Type)]
pub struct PublishStart {
    pub package_id: Uuid,
    pub platform: Platform,
    pub version_label: String,
    pub folder: String,
    pub launch: Option<PublishLaunch>,
    pub key_path: String,
    pub passphrase: String,
}

impl std::fmt::Debug for PublishStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishStart")
            .field("package_id", &self.package_id)
            .field("platform", &self.platform)
            .field("version_label", &self.version_label)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Type)]
pub struct PublishResume {
    pub key_path: String,
    pub passphrase: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PublishPhase {
    Preparing,
    Uploading,
    Signing,
    UploadingManifest,
    Finalizing,
    Verifying,
    /// Verified; waits for `publish_release`.
    Ready,
    Publishing,
    Published,
    Failed,
    Cancelled,
}

impl PublishPhase {
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Uploading
                | Self::Signing
                | Self::UploadingManifest
                | Self::Finalizing
                | Self::Verifying
                | Self::Publishing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PackState {
    Waiting,
    Uploading,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PackProgress {
    pub index: u32,
    pub bytes_confirmed: u64,
    pub bytes_total: u64,
    pub state: PackState,
}

/// Why a publisher key can't sign here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyError {
    #[error("the passphrase does not decrypt the key file")]
    WrongPassphrase,
    #[error("not a publisher key file")]
    InvalidKeyFile,
    #[error("the server does not trust this key now ({reason:?})")]
    UntrustedKey { reason: UntrustedReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UntrustedReason {
    Unknown,
    Revoked,
    OtherHolder,
    NotValidNow,
    NoBundle,
}

/// Why a job stopped in `failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublishJobError {
    /// The server could not verify the upload; `reason` is the server's text, if any.
    VerificationFailed {
        reason: Option<String>,
    },
    /// The folder changed since the upload started.
    SourceChanged,
    /// Network or server trouble; `retryable` = resuming may work.
    Remote {
        retryable: bool,
    },
    /// The version ended on the server (aborted, yanked or failed by someone else).
    VersionGone {
        state: VersionState,
    },
    Io {
        detail: String,
    },
    Internal {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PublishJob {
    pub id: Uuid,
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub package_title: String,
    pub platform: Platform,
    pub version_label: String,
    pub version_id: Option<Uuid>,
    pub phase: PublishPhase,
    pub bytes_confirmed: u64,
    pub bytes_total: u64,
    pub bytes_per_second: f64,
    pub packs: Vec<PackProgress>,
    /// 0–1 while `verifying`.
    pub verification: Option<f64>,
    pub error: Option<PublishJobError>,
    /// True while resuming needs the key again (the manifest is not signed yet).
    pub resume_needs_key: bool,
}

/// A job's state changed. At most 4 per second per job, plus every phase change.
#[derive(Debug, Clone, Serialize, Type, tauri_specta::Event)]
#[serde(transparent)]
pub struct PublishProgress(pub PublishJob);

/// Every publishing command fails with one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublishCommandError {
    #[error("only admins can publish")]
    Forbidden,
    #[error("sign-in required")]
    Unauthenticated,
    #[error("not found")]
    NotFound,
    #[error("the server could not be reached: {detail}")]
    Network { detail: String },
    #[error("{message} ({code})")]
    Server { code: String, message: String },
    #[error("another package has this slug")]
    SlugTaken,
    #[error("{field}: {message}")]
    InvalidField { field: String, message: String },
    #[error("the version is {state:?}")]
    VersionConflict { state: VersionState },
    #[error("the job is {phase:?}")]
    JobConflict { phase: PublishPhase },
    #[error("the folder has no files")]
    EmptyFolder,
    #[error("the version label must be 1 to 64 characters")]
    InvalidLabel,
    #[error("{count} entries cannot be packed")]
    InvalidPaths { count: u64 },
    #[error("the launch executable is not a file in the folder")]
    InvalidLaunch,
    #[error("the publisher key is needed again")]
    KeyRequired,
    #[error("{error}")]
    Key { error: KeyError },
    #[error("{detail}")]
    Io { detail: String },
    #[error("{detail}")]
    Internal { detail: String },
}

impl PublishCommandError {
    pub fn internal(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(error = %crate::error::DisplayChain(error), "cannot {context}");
        Self::Internal {
            detail: format!("Cannot {context}. See the log for details."),
        }
    }

    pub fn io(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::warn!(error = %crate::error::DisplayChain(error), "cannot {context}");
        Self::Io {
            detail: format!("Cannot {context}."),
        }
    }
}

impl From<KeyError> for PublishCommandError {
    fn from(error: KeyError) -> Self {
        Self::Key { error }
    }
}

impl From<AppError> for PublishCommandError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Network { detail } => Self::Network { detail },
            AppError::Server { code, message } => match code.as_str() {
                "forbidden" => Self::Forbidden,
                "not_found" => Self::NotFound,
                _ => Self::Server { code, message },
            },
            AppError::Unauthenticated => Self::Unauthenticated,
            AppError::NotFound => Self::NotFound,
            AppError::InvalidInput { field, message } => Self::InvalidField { field, message },
            AppError::Io { detail, .. } => Self::Io { detail },
            AppError::Internal { detail } => Self::Internal { detail },
        }
    }
}

impl From<ApiError> for PublishCommandError {
    fn from(error: ApiError) -> Self {
        match error.status() {
            Some(403) => Self::Forbidden,
            Some(404) => Self::NotFound,
            _ => AppError::from(error).into(),
        }
    }
}
