//! What the download commands and events carry (`src/bindings.ts`). Shapes
//! follow the UI contract (`src/ipc/contract/downloads.ts` before INS-03
//! generated them). Pause reasons and errors are also what the queue stores
//! (`download_jobs.error`, as JSON).

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::catalog::CompatBlocker;
use crate::db::download_jobs::JobKind;
use crate::events::{InstallOutcome, PackageRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DownloadKind {
    Install,
    Update,
    Repair,
}

impl From<JobKind> for DownloadKind {
    fn from(kind: JobKind) -> Self {
        match kind {
            JobKind::Install => Self::Install,
            JobKind::Update => Self::Update,
            JobKind::Repair => Self::Repair,
        }
    }
}

/// Why a job waits (02 §7.10). Every reason except `user` clears by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseReason {
    /// The player paused it.
    User,
    /// The disk filled up; the job continues once enough space is free.
    DiskFull {
        library_path: String,
        required_bytes: u64,
        available_bytes: u64,
    },
    /// The library's drive was disconnected; the job continues when it is back.
    LibraryOffline { library_path: String },
    /// The server can't be reached; the job continues when it is back.
    Offline,
}

/// Why a job stopped for good (until the player retries or removes it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadError {
    /// A chunk failed its hash twice (02 §7.10). `reported`: the launcher sent
    /// an integrity report, so the server's admins know.
    DamagedFile {
        reported: bool,
    },
    /// The manifest's signature doesn't verify (01 §3.4). Nothing was written.
    SignatureInvalid,
    /// The manifest was signed with a revoked or unknown publisher key.
    UntrustedKey,
    /// The server's trust bundle expired: new downloads wait until it is renewed.
    TrustExpired,
    /// The version was withdrawn or deleted on the server.
    VersionUnavailable,
    /// Any other I/O error, with the OS message and the path (the journal is kept).
    Io {
        path: String,
        detail: String,
    },
    Server {
        code: String,
        message: String,
    },
    /// A compatibility check of the launch target refused the package
    /// (GAME-06, through the priority-files hook). Partial files are removed.
    Blocked {
        blocker: CompatBlocker,
    },
}

impl DownloadError {
    /// `code` and message of the `install-finished` event.
    pub fn summary(&self) -> (String, String) {
        let code = match self {
            Self::DamagedFile { .. } => "damaged_file",
            Self::SignatureInvalid => "signature_invalid",
            Self::UntrustedKey => "untrusted_key",
            Self::TrustExpired => "trust_expired",
            Self::VersionUnavailable => "version_unavailable",
            Self::Io { .. } => "io",
            Self::Server { code, .. } => code.as_str(),
            Self::Blocked { .. } => "blocked",
        };
        let message = match self {
            Self::DamagedFile { .. } => "A file on the server is damaged".to_owned(),
            Self::SignatureInvalid => "The package's signature does not verify".to_owned(),
            Self::UntrustedKey => "The package was signed with an untrusted key".to_owned(),
            Self::TrustExpired => "The server's trust bundle expired".to_owned(),
            Self::VersionUnavailable => "This version is no longer available".to_owned(),
            Self::Io { detail, .. } => detail.clone(),
            Self::Server { message, .. } => message.clone(),
            Self::Blocked { .. } => "This game cannot run on this computer".to_owned(),
        };
        (code.to_owned(), message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadState {
    Active,
    Queued,
    Paused { reason: PauseReason },
    Failed { error: DownloadError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct DownloadJob {
    pub package: PackageRef,
    pub title: String,
    /// `vgimg:` URL of the cached cover.
    pub cover_url: Option<String>,
    pub kind: DownloadKind,
    /// The version being installed (for a repair: the installed one).
    pub version_label: String,
    pub library_id: String,
    /// Last known progress; running jobs report live progress through `install-progress`.
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: DownloadState,
    pub queued_at: String,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct DownloadHistoryEntry {
    pub id: String,
    pub package: PackageRef,
    pub title: String,
    pub kind: DownloadKind,
    pub version_label: String,
    pub bytes_total: u64,
    pub finished_at: String,
    pub outcome: InstallOutcome,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct DownloadQueue {
    /// In queue order: running jobs first, then the rest in the order they will run.
    pub jobs: Vec<DownloadJob>,
    /// Newest first, at most 100 entries.
    pub history: Vec<DownloadHistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadActionError {
    #[error("no such download")]
    NotFound,
    #[error("not enough disk space")]
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("the library is offline")]
    LibraryOffline { library_path: String },
    #[error("the server cannot be reached")]
    Offline,
    #[error("{detail}")]
    Io { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DownloadSettings {
    /// Kibibytes per second; null = unlimited (02 §7.12).
    pub bandwidth_limit_kib: Option<u32>,
    /// How many installs run at the same time, 1–3.
    pub concurrent_installs: u32,
}

impl DownloadSettings {
    pub const MAX_CONCURRENT: u32 = 3;

    pub fn limit_bytes_per_second(self) -> Option<u64> {
        self.bandwidth_limit_kib.map(|kib| u64::from(kib) * 1024)
    }
}

/// The queue or the history changed (jobs added, reordered, paused, finished, removed).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct DownloadsChanged {}
