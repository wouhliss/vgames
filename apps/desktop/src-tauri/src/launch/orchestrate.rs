//! One launch, end to end (A2-T09): rate limit, install state, trust, the
//! pre-launch checks (02 §11), the launch plan and the game session. Used by
//! the `game_launch` command and the `vgames://launch/<id>` deep link.
//!
//! Still to hook in: the cloud-save pull (A2-T11), the controller session
//! (A2-T12) and compat plans for Proton/Wine (A2-T16/T17).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use specta::Type;
use uuid::Uuid;
use vgames_core::manifest::Platform;
use vgames_core::trust::TrustState;
use vgames_core::verify::ExpectedRelease;

use super::prelaunch::{Prelaunch, PrelaunchError};
use super::{GameSessions, LaunchPlan, SessionError, TargetChoice, TargetError};
use crate::db::{self, Db, DbError};
use crate::events::PackageRef;
use crate::servers::Servers;

/// 01-security §7: one launch per 3 s, whatever asked for it.
const LAUNCH_INTERVAL: Duration = Duration::from_secs(3);

/// Where the verified trust state of a server comes from.
pub trait TrustSource: Send + Sync + 'static {
    fn trust(
        &self,
        server_id: Uuid,
    ) -> impl Future<Output = Result<Option<Arc<TrustState>>, DbError>> + Send;
}

impl TrustSource for Servers {
    async fn trust(&self, server_id: Uuid) -> Result<Option<Arc<TrustState>>, DbError> {
        self.trust_state(server_id).await
    }
}

/// Variables a [`LaunchHooks`] adds, once ready.
pub type HookEnv<'a> = Pin<Box<dyn Future<Output = Vec<(String, String)>> + Send + 'a>>;

/// Launcher-side extras for a game process (Agent 4: the in-game overlay).
pub trait LaunchHooks: Send + Sync + 'static {
    /// Variables to add just before `package` starts. Launcher-generated values
    /// only, never package or network data; a refused key is skipped.
    fn prepare(&self, package: PackageRef) -> HookEnv<'_>;
    /// The game did not start after [`Self::prepare`].
    fn aborted(&self, package: PackageRef);
}

/// An install that is busy with something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BusyState {
    Installing,
    Updating,
    Repairing,
    Moving,
    Uninstalling,
}

/// Why a launch did not start (the UI's `LaunchError`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchError {
    #[error("The package is not installed")]
    NotInstalled,
    #[error("The install is incomplete")]
    Incomplete,
    #[error("The package is busy")]
    Busy { state: BusyState },
    #[error("The library drive is not connected")]
    LibraryOffline { library_path: String },
    #[error("The game is already running")]
    AlreadyRunning,
    #[error("The launch option does not exist")]
    TargetNotFound,
    /// A file no longer matches the signed manifest; "Verify" repairs it.
    #[error("A game file was modified")]
    Integrity { path: String },
    /// The publisher key was revoked; "Verify" fetches a re-signed manifest.
    #[error("The package's signing key was revoked")]
    KeyRevoked,
    #[error("{detail}")]
    CompatUnavailable { detail: String },
    #[error("Wait a moment before launching again")]
    RateLimited,
    /// A cloud save conflict must be resolved first (A2-T11 sends the conflict).
    #[error("Resolve the cloud save conflict first")]
    SaveConflict { conflict_id: String },
    #[error("{detail}")]
    Io { detail: String },
}

impl LaunchError {
    fn io(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(error = %crate::error::DisplayChain(error), "{context}");
        Self::Io {
            detail: context.to_owned(),
        }
    }
}

/// Launches games; one per launcher.
pub struct Launcher<T> {
    db: Db,
    trust: Arc<T>,
    sessions: GameSessions,
    prelaunch: Arc<Prelaunch>,
    last_launch: Mutex<Option<Instant>>,
    host: Option<Platform>,
    hooks: OnceLock<Arc<dyn LaunchHooks>>,
}

impl<T: TrustSource> Launcher<T> {
    pub fn new(db: Db, trust: Arc<T>, sessions: GameSessions) -> Self {
        Self {
            db,
            trust,
            sessions,
            prelaunch: Arc::new(Prelaunch::default()),
            last_launch: Mutex::new(None),
            host: host_platform(),
            hooks: OnceLock::new(),
        }
    }

    /// Installs the launch hooks (once, at startup; later calls are ignored).
    pub fn set_hooks(&self, hooks: Arc<dyn LaunchHooks>) {
        if self.hooks.set(hooks).is_err() {
            tracing::warn!("launch hooks already installed");
        }
    }

    pub fn prelaunch(&self) -> &Arc<Prelaunch> {
        &self.prelaunch
    }

    /// Checks and starts `package`; returns the game's pid.
    pub async fn launch(
        &self,
        package: PackageRef,
        choice: TargetChoice,
    ) -> Result<u32, LaunchError> {
        self.rate_limit()?;
        if self.sessions.is_running(package) {
            return Err(LaunchError::AlreadyRunning);
        }
        let row = db::installs::row(&self.db, package)
            .await
            .map_err(|e| LaunchError::io("Cannot read the install", &e))?
            .ok_or(LaunchError::NotInstalled)?;
        match row.state.as_str() {
            "installed" => {}
            "installing" => return Err(busy(BusyState::Installing)),
            "updating" => return Err(busy(BusyState::Updating)),
            "repairing" => return Err(busy(BusyState::Repairing)),
            "moving" => return Err(busy(BusyState::Moving)),
            "uninstalling" => return Err(busy(BusyState::Uninstalling)),
            _ => return Err(LaunchError::Incomplete),
        }
        let expected = expected_release(package, &row).ok_or(LaunchError::Incomplete)?;
        let native = self
            .host
            .is_some_and(|host| runs_natively(expected.platform, host));
        if !native {
            return Err(LaunchError::CompatUnavailable {
                detail: format!(
                    "This {} build needs a compatibility layer, which this launcher version cannot run yet.",
                    expected.platform.as_str()
                ),
            });
        }
        let trust = self
            .trust
            .trust(package.server_id)
            .await
            .map_err(|e| LaunchError::io("Cannot read the server's trust state", &e))?
            .ok_or_else(|| LaunchError::Io {
                detail: "Connect to this game's server once before launching it.".into(),
            })?;

        let prelaunch = Arc::clone(&self.prelaunch);
        let root = row.root.clone();
        let library = row.library_path.clone();
        let checked = tokio::task::spawn_blocking(move || {
            if std::fs::metadata(&library).is_err() {
                return Err(LaunchError::LibraryOffline {
                    library_path: library.to_string_lossy().into_owned(),
                });
            }
            prelaunch
                .check(&root, &trust, &expected, &choice)
                .map_err(from_prelaunch)
        })
        .await
        .map_err(|e| LaunchError::io("The pre-launch check stopped", &e))??;

        let mut prepared = LaunchPlan::Native
            .prepare(checked.target, std::env::vars_os())
            .map_err(|e| from_prelaunch(PrelaunchError::Launch(e)))?;
        let hooks = self.hooks.get();
        if let Some(hooks) = hooks {
            for (key, value) in hooks.prepare(package).await {
                if let Err(error) = prepared.inject_env(&key, value) {
                    tracing::warn!(%error, "launch hook variable refused");
                }
            }
        }
        let started = self
            .sessions
            .start(package, row.root, prepared)
            .await
            .map_err(|error| match error {
                SessionError::AlreadyRunning => LaunchError::AlreadyRunning,
                other => LaunchError::io("Cannot start the game", &other),
            });
        if started.is_err()
            && let Some(hooks) = hooks
        {
            hooks.aborted(package);
        }
        started
    }

    fn rate_limit(&self) -> Result<(), LaunchError> {
        let mut last = self
            .last_launch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < LAUNCH_INTERVAL) {
            return Err(LaunchError::RateLimited);
        }
        *last = Some(now);
        Ok(())
    }
}

fn busy(state: BusyState) -> LaunchError {
    LaunchError::Busy { state }
}

fn expected_release(
    package: PackageRef,
    row: &db::installs::InstallRow,
) -> Option<ExpectedRelease> {
    Some(ExpectedRelease {
        server_id: package.server_id,
        package_id: package.package_id,
        version_id: Uuid::parse_str(&row.version_id).ok()?,
        platform: Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == row.platform)?,
        sequence: u64::try_from(row.sequence).ok()?,
    })
}

fn from_prelaunch(error: PrelaunchError) -> LaunchError {
    match error {
        PrelaunchError::NotInstalled => LaunchError::Incomplete,
        PrelaunchError::Reverify => LaunchError::KeyRevoked,
        PrelaunchError::Integrity(detail) => {
            tracing::warn!(%detail, "pre-launch integrity check failed");
            LaunchError::Integrity {
                path: ".vgames/manifest.json".into(),
            }
        }
        PrelaunchError::ExecutableModified { path } => LaunchError::Integrity { path },
        PrelaunchError::Launch(TargetError::NoTargets | TargetError::UnknownTarget(_)) => {
            LaunchError::TargetNotFound
        }
        PrelaunchError::Launch(TargetError::OutsideInstall { path, .. }) => {
            LaunchError::Integrity {
                path: path.to_string_lossy().into_owned(),
            }
        }
        PrelaunchError::Launch(error @ TargetError::EnvKey(_)) => LaunchError::io(
            "The package asks for a forbidden environment variable",
            &error,
        ),
        PrelaunchError::Io { path, source } => {
            tracing::warn!(path = %path.display(), %source, "pre-launch read failed");
            LaunchError::Io {
                detail: format!("Cannot read {}", display_name(&path)),
            }
        }
    }
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

/// The platform this launcher runs on.
pub const fn host_platform() -> Option<Platform> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(Platform::WindowsX86_64)
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some(Platform::WindowsAarch64)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(Platform::LinuxX86_64)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(Platform::LinuxAarch64)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(Platform::MacosAarch64)
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(Platform::MacosX86_64)
    } else {
        None
    }
}

/// Same OS, and the same architecture or one the OS translates itself
/// (x86-64 on Windows on Arm and on Apple silicon through Rosetta 2).
pub fn runs_natively(build: Platform, host: Platform) -> bool {
    use Platform::*;
    build == host
        || matches!(
            (build, host),
            (WindowsX86_64, WindowsAarch64) | (MacosX86_64, MacosAarch64)
        )
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
