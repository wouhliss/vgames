//! Launcher self-update (A5-T09, 08-release §2). Owner: Agent 5.
//!
//! - `tauri-plugin-updater` verifies the minisign signature of every artifact
//!   against the public key compiled into the launcher (`tauri.conf.json`);
//!   nothing unsigned is installed.
//! - Only a strictly greater version is offered ([`policy::is_update`]), and the
//!   download URL must be HTTPS ([`policy::require_https`]).
//! - Checks start once the UI is interactive (`app_ready`), then every 6 hours,
//!   and wait while a game runs or a download is active. Installing waits for
//!   downloads to reach a checkpoint and is refused while a game runs.
//! - "What's new" comes from `changelog-user.json` (HTTPS, 1 MiB cap, strictly
//!   validated, plain text), falling back to the `latest.json` notes.
//!
//! The UI talks to this module only through `updater_status`, `updater_check`,
//! `updater_whats_new`, `updater_install` and the `UpdaterStatus` event.

pub mod policy;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use semver::Version;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::UpdaterExt;
use tauri_specta::Event as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{CommandError, CommandResult, ErrorCode};
use crate::events::{AppEvent, InstallPhase, PackageRef};
use crate::state::AppState;

use self::policy::{Activity, Blocked, WhatsNew};

/// Between two automatic checks.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Every network request of the updater.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Cumulative player changelog published with every release (08-release §3.3).
const CHANGELOG_URL: &str =
    "https://github.com/wouhliss/vgames/releases/latest/download/changelog-user.json";
/// Progress events to the UI, at most 4 per second.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Where the updater is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdaterState {
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
        date: Option<String>,
    },
    Downloading {
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    /// Installed; the launcher restarts.
    Installed {
        version: String,
    },
    Failed {
        message: String,
    },
}

/// Current updater status. Also emitted as an event whenever it changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct UpdaterStatus {
    pub current_version: String,
    pub state: UpdaterState,
    /// Why checks and installs are waiting, if they are.
    pub blocked: Option<Blocked>,
}

struct Inner {
    state: UpdaterState,
    activity: Activity,
    pending: Option<tauri_plugin_updater::Update>,
    whats_new: Option<WhatsNew>,
}

/// Managed by Tauri; cheap to clone.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Mutex<Inner>>,
    /// Signalled when games or downloads start or stop.
    activity_changed: Arc<Notify>,
    /// Serializes checks and installs.
    busy: Arc<tokio::sync::Mutex<()>>,
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn key(p: &PackageRef) -> (uuid::Uuid, uuid::Uuid) {
    (p.server_id, p.package_id)
}

impl Updater {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: UpdaterState::Idle,
                activity: Activity::default(),
                pending: None,
                whats_new: None,
            })),
            activity_changed: Arc::new(Notify::new()),
            busy: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        // A poisoned lock only means a panic elsewhere; the data is still usable.
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    pub fn status(&self) -> UpdaterStatus {
        self.with(|i| UpdaterStatus {
            current_version: current_version().to_string(),
            state: i.state.clone(),
            blocked: i.activity.blocked(),
        })
    }

    fn set(&self, app: &AppHandle, state: UpdaterState) {
        self.with(|i| i.state = state);
        if let Err(error) = self.status().emit(app) {
            tracing::debug!(%error, "cannot emit the updater status");
        }
    }

    fn blocked(&self) -> Option<Blocked> {
        self.with(|i| i.activity.blocked())
    }

    /// Waits until no game runs and no download is active. `false` on shutdown.
    async fn wait_until_idle(&self, shutdown: &CancellationToken) -> bool {
        loop {
            let notified = self.activity_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.blocked().is_none() {
                return true;
            }
            tokio::select! {
                () = &mut notified => {}
                () = shutdown.cancelled() => return false,
            }
        }
    }
}

/// Registers the updater state and starts its background tasks. Call from `setup`
/// after the plugin is registered and `AppState` is managed.
pub fn init(app: &AppHandle, state: &AppState) {
    let updater = Updater::new();
    app.manage(updater.clone());
    spawn_activity_tracker(app.clone(), updater.clone(), state);
    spawn_scheduler(app.clone(), updater, state);
}

/// Follows games and installs on the internal bus.
fn spawn_activity_tracker(app: AppHandle, updater: Updater, state: &AppState) {
    let mut events = state.bus.subscribe();
    let shutdown = state.shutdown.child_token();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = tokio::select! {
                e = events.recv() => e,
                () = shutdown.cancelled() => return,
            };
            let changed = match event {
                Ok(AppEvent::GameStarted(e)) => {
                    updater.with(|i| i.activity.game_started(key(&e.package)));
                    true
                }
                Ok(AppEvent::GameStopped(e)) => {
                    updater.with(|i| i.activity.game_stopped(key(&e.package)));
                    true
                }
                Ok(AppEvent::InstallProgress(e)) => {
                    let paused = e.phase == InstallPhase::Paused;
                    updater.with(|i| i.activity.install_progress(key(&e.package), paused));
                    true
                }
                Ok(AppEvent::InstallFinished(e)) => {
                    updater.with(|i| i.activity.install_finished(key(&e.package)));
                    true
                }
                Ok(_) => false,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "updater lagged behind the event bus");
                    false
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if changed {
                updater.activity_changed.notify_waiters();
                if let Err(error) = updater.status().emit(&app) {
                    tracing::debug!(%error, "cannot emit the updater status");
                }
            }
        }
    });
}

/// First check once the UI is ready, then every 6 hours, never while busy.
fn spawn_scheduler(app: AppHandle, updater: Updater, state: &AppState) {
    let ready = state.ui_ready.clone();
    let shutdown = state.shutdown.child_token();
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            () = ready.notified() => {}
            () = shutdown.cancelled() => return,
        }
        loop {
            if !updater.wait_until_idle(&shutdown).await {
                return;
            }
            check(&app, &updater).await;
            tokio::select! {
                () = tokio::time::sleep(CHECK_EVERY) => {}
                () = shutdown.cancelled() => return,
            }
        }
    });
}

/// One update check. Never fails loudly: the result is the status.
async fn check(app: &AppHandle, updater: &Updater) -> UpdaterStatus {
    let _busy = updater.busy.lock().await;
    if matches!(
        updater.with(|i| i.state.clone()),
        UpdaterState::Downloading { .. } | UpdaterState::Installed { .. }
    ) {
        return updater.status();
    }
    updater.set(app, UpdaterState::Checking);
    let result = async {
        let built = app
            .updater_builder()
            // Strictly greater only: never a downgrade, never a re-install.
            .version_comparator(|current, remote| policy::is_update(&current, &remote.version))
            .timeout(HTTP_TIMEOUT)
            .build()?;
        built.check().await
    }
    .await;
    match result {
        Ok(Some(update)) => {
            if let Err(error) = policy::require_https(&update.download_url, cfg!(debug_assertions))
            {
                tracing::warn!(%error, "refusing an update served over plain HTTP");
                updater.set(
                    app,
                    UpdaterState::Failed {
                        message: "The update is not served securely.".into(),
                    },
                );
                return updater.status();
            }
            let state = UpdaterState::Available {
                version: update.version.clone(),
                date: update.date.map(|d| d.date().to_string()),
            };
            tracing::info!(version = %update.version, "launcher update available");
            updater.with(|i| {
                i.pending = Some(update);
                i.whats_new = None;
            });
            updater.set(app, state);
        }
        Ok(None) => updater.set(app, UpdaterState::UpToDate),
        Err(error) => {
            tracing::warn!(error = %crate::error::DisplayChain(&error), "update check failed");
            updater.set(
                app,
                UpdaterState::Failed {
                    message: "Could not check for updates.".into(),
                },
            );
        }
    }
    updater.status()
}

/// Downloads `changelog-user.json` with the size cap. `None` on any problem.
async fn fetch_changelog() -> Option<Vec<policy::ReleaseNotes>> {
    let url = Url::parse(CHANGELOG_URL).ok()?;
    policy::require_https(&url, false).ok()?;
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .https_only(true)
        .build()
        .ok()?;
    let mut response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    if response
        .content_length()
        .is_some_and(|n| n > policy::MAX_CHANGELOG_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > policy::MAX_CHANGELOG_BYTES {
            tracing::warn!("changelog larger than 1 MiB; using the update notes instead");
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    match policy::parse_changelog(&body) {
        Ok(releases) => Some(releases),
        Err(error) => {
            tracing::warn!(%error, "invalid changelog; using the update notes instead");
            None
        }
    }
}

fn error(code: ErrorCode, message: &str) -> CommandError {
    CommandError::new(code, message)
}

/// Current updater status.
#[tauri::command]
#[specta::specta]
pub fn updater_status(updater: State<'_, Updater>) -> UpdaterStatus {
    updater.status()
}

/// Checks for an update now (onboarding's "launcher too old" prompt, Settings).
/// Refused while a game runs.
#[tauri::command]
#[specta::specta]
pub async fn updater_check(
    app: AppHandle,
    updater: State<'_, Updater>,
) -> CommandResult<UpdaterStatus> {
    let updater = updater.inner().clone();
    if updater.blocked() == Some(Blocked::GameRunning) {
        return Err(error(
            ErrorCode::Conflict,
            "Close the game to check for updates.",
        ));
    }
    Ok(check(&app, &updater).await)
}

/// What changed between the installed version and the available update.
#[tauri::command]
#[specta::specta]
pub async fn updater_whats_new(updater: State<'_, Updater>) -> CommandResult<WhatsNew> {
    let updater = updater.inner().clone();
    if let Some(cached) = updater.with(|i| i.whats_new.clone()) {
        return Ok(cached);
    }
    let Some((version, notes)) = updater.with(|i| {
        i.pending
            .as_ref()
            .map(|u| (u.version.clone(), u.body.clone()))
    }) else {
        return Err(error(ErrorCode::NotFound, "No update is available."));
    };
    let new = Version::parse(&version)
        .map_err(|_| error(ErrorCode::Integrity, "The update has an invalid version."))?;
    let changelog = fetch_changelog().await;
    let whats_new = policy::whats_new(
        changelog.as_deref(),
        notes.as_deref(),
        &current_version(),
        &new,
    );
    updater.with(|i| i.whats_new = Some(whats_new.clone()));
    Ok(whats_new)
}

/// Downloads, verifies (minisign) and installs the available update, then
/// restarts. Waits for downloads to reach a checkpoint; refused while a game runs.
#[tauri::command]
#[specta::specta]
pub async fn updater_install(
    app: AppHandle,
    state: State<'_, AppState>,
    updater: State<'_, Updater>,
) -> CommandResult<()> {
    let updater = updater.inner().clone();
    let shutdown = state.shutdown.child_token();
    if updater.blocked() == Some(Blocked::GameRunning) {
        return Err(error(
            ErrorCode::Conflict,
            "Close the game to install the update.",
        ));
    }
    let _busy = updater.busy.lock().await;
    // Downloads pause at their next checkpoint; the UI shows `blocked` meanwhile.
    if !updater.wait_until_idle(&shutdown).await {
        return Err(error(ErrorCode::Conflict, "The launcher is closing."));
    }
    if updater.blocked().is_some() {
        return Err(error(
            ErrorCode::Conflict,
            "Close the game to install the update.",
        ));
    }
    let Some(update) = updater.with(|i| i.pending.take()) else {
        return Err(error(ErrorCode::NotFound, "No update is available."));
    };
    let version = update.version.clone();
    updater.set(
        &app,
        UpdaterState::Downloading {
            version: version.clone(),
            downloaded: 0,
            total: None,
        },
    );

    let progress_app = app.clone();
    let progress_updater = updater.clone();
    let progress_version = version.clone();
    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now();
    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded = downloaded.saturating_add(chunk as u64);
                if last_emit.elapsed() >= PROGRESS_INTERVAL {
                    last_emit = Instant::now();
                    progress_updater.set(
                        &progress_app,
                        UpdaterState::Downloading {
                            version: progress_version.clone(),
                            downloaded,
                            total,
                        },
                    );
                }
            },
            || tracing::info!("update downloaded and verified"),
        )
        .await;
    match result {
        Ok(()) => {
            tracing::info!(%version, "launcher update installed; restarting");
            updater.set(&app, UpdaterState::Installed { version });
            app.restart();
        }
        Err(e) => {
            // A signature mismatch lands here: nothing was installed.
            tracing::error!(error = %crate::error::DisplayChain(&e), "update install failed");
            updater.with(|i| i.pending = Some(update));
            updater.set(
                &app,
                UpdaterState::Failed {
                    message: "The update could not be installed.".into(),
                },
            );
            Err(error(
                ErrorCode::Integrity,
                "The update could not be verified or installed.",
            ))
        }
    }
}
