//! The install queue worker (INS-03): claims jobs from `db::download_jobs`
//! (1–3 at once, from the download settings) and runs each through the
//! signature-first transfer path (G1): `install::fetch_release` (manifest size
//! and BLAKE3, then `verify_manifest`) → `install::check_space` →
//! `install::install`. Nothing is written before the manifest verifies, and no
//! chunk reaches its file before its BLAKE3 matches.
//!
//! Nothing here polls. The dispatcher sleeps on a `Notify` woken by queue
//! changes and finished jobs; jobs paused for a reason that clears by itself
//! (offline, library offline, disk full) are re-checked on connectivity
//! changes, window focus, library listing and startup ([`Downloads::recheck`]).

pub mod backend;
pub mod hook;
mod job;
mod maintain;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_transfer::download::{DownloadControl, DownloadOptions};

use self::backend::Backend;
use self::hook::{NoPriority, PriorityHook};
pub use self::model::*;
use crate::db::download_jobs::{self as jobs, Job, JobState, JobStoreError, JobTransition};
use crate::db::settings::{self, ConcurrentInstalls, DownloadLimit};
use crate::db::{self, Db};
use crate::events::{AppEvent, EventBus, InstallFinished, InstallOutcome, PackageRef};

/// How a job ended this time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum End {
    Installed,
    Paused(PauseReason),
    Failed(DownloadError),
    Cancelled {
        keep_partial: bool,
    },
    /// Paused for the updater (`pause_all_at_checkpoint`): back in the queue.
    Checkpoint,
}

/// What a command asked of a running job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Pause,
    Checkpoint,
    Cancel { keep_partial: bool },
}

struct Running {
    control: DownloadControl,
    stop: Option<Stop>,
    /// The run returned; its end is being recorded. Requests wait for that.
    finishing: bool,
    /// Becomes `true` once the end is recorded and the job left `running`.
    done: watch::Sender<bool>,
    /// Stored progress when the run started (live progress is the control's).
    stored: (u64, u64),
}

impl Running {
    fn bytes(&self) -> (u64, u64) {
        let live = self.control.current_progress();
        if live.bytes_total > 0 {
            (live.bytes_done, live.bytes_total)
        } else {
            self.stored
        }
    }
}

struct State {
    running: HashMap<PackageRef, Running>,
    /// Set by `pause_all_at_checkpoint`: claim nothing new.
    held: bool,
    settings: DownloadSettings,
}

/// Called after an install's files or signature changed (update, repair):
/// the launcher forgets its pre-launch stamps and cached launch targets.
pub type FilesChanged = Arc<dyn Fn(PackageRef, &std::path::Path) + Send + Sync>;

/// Free bytes on the drive holding a path.
pub type FreeSpace = Arc<dyn Fn(&std::path::Path) -> u64 + Send + Sync>;

pub struct Downloads<B: Backend> {
    db: Db,
    free_space: FreeSpace,
    /// Chunks verified across every run (each chunk once unless retried).
    verified: std::sync::atomic::AtomicU64,
    bus: EventBus,
    backend: Arc<B>,
    options: DownloadOptions,
    hook: RwLock<Arc<dyn PriorityHook>>,
    files_changed: RwLock<Option<FilesChanged>>,
    state: Mutex<State>,
    /// Serializes claims with entering an updater checkpoint.
    claim_gate: tokio::sync::Mutex<()>,
    wake: Notify,
    active: watch::Sender<usize>,
    stop: CancellationToken,
}

fn store_error(context: &str, error: &dyn std::error::Error) -> DownloadActionError {
    tracing::error!(error = %crate::error::DisplayChain(error), "{context}");
    DownloadActionError::Io {
        detail: format!("Cannot {context}"),
    }
}

impl<B: Backend> Downloads<B> {
    /// `options` are the engine settings (tests shorten its timings).
    pub fn new(
        db: Db,
        bus: EventBus,
        backend: Arc<B>,
        options: DownloadOptions,
        stop: CancellationToken,
    ) -> Arc<Self> {
        Self::with_free_space(
            db,
            bus,
            backend,
            options,
            stop,
            Arc::new(|path| vgames_transfer::sys::available_space(path).unwrap_or(0)),
        )
    }

    /// Like [`Self::new`] with another free-space probe (tests).
    pub fn with_free_space(
        db: Db,
        bus: EventBus,
        backend: Arc<B>,
        options: DownloadOptions,
        stop: CancellationToken,
        free_space: FreeSpace,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            free_space,
            verified: std::sync::atomic::AtomicU64::new(0),
            bus,
            backend,
            options,
            hook: RwLock::new(Arc::new(NoPriority)),
            files_changed: RwLock::new(None),
            state: Mutex::new(State {
                running: HashMap::new(),
                held: false,
                settings: DownloadSettings {
                    bandwidth_limit_kib: None,
                    concurrent_installs: 1,
                },
            }),
            claim_gate: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            active: watch::Sender::new(0),
            stop,
        })
    }

    /// GAME-06 registers its launch-target check here.
    pub fn set_priority_hook(&self, hook: Arc<dyn PriorityHook>) {
        if let Ok(mut slot) = self.hook.write() {
            *slot = hook;
        }
    }

    pub fn set_files_changed(&self, hook: FilesChanged) {
        if let Ok(mut slot) = self.files_changed.write() {
            *slot = Some(hook);
        }
    }

    pub(crate) fn files_changed(&self, package: PackageRef, root: &std::path::Path) {
        let hook = self.files_changed.read().ok().and_then(|h| h.clone());
        if let Some(hook) = hook {
            hook(package, root);
        }
    }

    pub(crate) fn hook(&self) -> Arc<dyn PriorityHook> {
        match self.hook.read() {
            Ok(slot) => Arc::clone(&slot),
            Err(_) => Arc::new(NoPriority),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wakes the dispatcher (queue changed).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// A job was queued (`installs::start`): announce it and run it.
    pub fn queued(&self) {
        self.changed();
        self.wake();
    }

    fn changed(&self) {
        self.bus
            .publish(AppEvent::DownloadsChanged(DownloadsChanged {}));
    }

    /// Startup: puts interrupted jobs back in the queue (each resumes from its
    /// journal), re-checks paused ones, and runs the dispatcher until `stop`.
    pub fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            if let Err(error) = jobs::recover_active(&this.db).await {
                tracing::error!(%error, "cannot requeue interrupted downloads");
            }
            match this.load_settings().await {
                Ok(settings) => this.lock().settings = settings,
                Err(error) => tracing::error!(%error, "cannot read the download settings"),
            }
            this.recheck().await;
            this.dispatch().await;
        });
        self.spawn_listener();
    }

    async fn load_settings(&self) -> Result<DownloadSettings, db::DbError> {
        self.db
            .call(|conn| {
                let concurrent = settings::get::<ConcurrentInstalls>(conn)?;
                let limit = settings::get::<DownloadLimit>(conn)?;
                Ok(DownloadSettings {
                    bandwidth_limit_kib: limit
                        .map(|bytes| u32::try_from(bytes / 1024).unwrap_or(u32::MAX).max(1)),
                    concurrent_installs: concurrent.clamp(1, DownloadSettings::MAX_CONCURRENT),
                })
            })
            .await
    }

    /// Re-checks paused jobs on connectivity changes (server back online).
    fn spawn_listener(self: &Arc<Self>) {
        let mut events = self.bus.subscribe();
        let this = Arc::downgrade(self);
        let stop = self.stop.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let event = tokio::select! {
                    () = stop.cancelled() => return,
                    event = events.recv() => event,
                };
                let recheck = match event {
                    Ok(AppEvent::ConnectivityChanged(c)) => c.online,
                    Ok(_) => false,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                match this.upgrade() {
                    Some(this) if recheck => this.recheck().await,
                    Some(_) => {}
                    None => return,
                }
            }
        });
    }

    async fn dispatch(self: &Arc<Self>) {
        loop {
            if self.stop.is_cancelled() {
                return;
            }
            self.fill().await;
            tokio::select! {
                () = self.wake.notified() => {}
                () = self.stop.cancelled() => return,
            }
        }
    }

    /// Starts jobs until the concurrency setting is reached.
    async fn fill(self: &Arc<Self>) {
        loop {
            let _claim = self.claim_gate.lock().await;
            let (held, max) = {
                let state = self.lock();
                (state.held, state.settings.concurrent_installs as usize)
            };
            if held {
                return;
            }
            match jobs::claim_next(&self.db, max).await {
                Ok(Some(job)) => self.launch(job),
                Ok(None) => return,
                Err(error) => {
                    tracing::error!(%error, "cannot claim the next download");
                    return;
                }
            }
        }
    }

    fn launch(self: &Arc<Self>, job: Job) {
        let control = {
            let mut state = self.lock();
            let control = DownloadControl::new(state.settings.limit_bytes_per_second());
            // Claimed while `pause_all_at_checkpoint` was starting: it stops at
            // the first checkpoint like the others.
            let stop = state.held.then(|| {
                control.pause();
                Stop::Checkpoint
            });
            state.running.insert(
                job.package,
                Running {
                    control: control.clone(),
                    stop,
                    finishing: false,
                    done: watch::Sender::new(false),
                    stored: (job.bytes_done, job.bytes_total),
                },
            );
            self.active.send_replace(state.running.len());
            control
        };
        self.changed();
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let package = job.package;
            let end = this.run(&job, &control).await;
            let stop = this.lock().running.get_mut(&package).and_then(|r| {
                r.finishing = true;
                r.stop
            });
            let end = match (end, stop) {
                // A pause requested for the updater is a checkpoint, not a
                // player's pause.
                (End::Paused(PauseReason::User), Some(Stop::Checkpoint)) => End::Checkpoint,
                (end, _) => end,
            };
            this.complete(&job, end).await;
            {
                let mut state = this.lock();
                if let Some(running) = state.running.remove(&package) {
                    running.done.send_replace(true);
                }
                this.active.send_replace(state.running.len());
            }
            this.changed();
            this.wake();
        });
    }

    /// Records how a run ended, in the queue, the history and the events.
    async fn complete(&self, job: &Job, end: End) {
        let package = job.package;
        let bytes = self.lock().running.get(&package).map(Running::bytes);
        if let Some((done, total)) = bytes
            && let Err(error) = jobs::set_progress(&self.db, package, done, total).await
        {
            tracing::warn!(%error, "cannot record download progress");
        }
        let result = match &end {
            End::Installed => self.finish(package, InstallOutcome::Installed).await,
            End::Cancelled { keep_partial } => {
                self.finish(
                    package,
                    InstallOutcome::Cancelled {
                        kept_partial: *keep_partial,
                    },
                )
                .await
            }
            End::Paused(reason) => {
                tracing::info!(%package.package_id, ?reason, "download paused");
                jobs::transition(
                    &self.db,
                    package,
                    JobTransition::PauseActive {
                        reason: encode(reason),
                    },
                )
                .await
            }
            End::Failed(error) => {
                tracing::warn!(%package.package_id, ?error, "download failed");
                let (code, message) = error.summary();
                let result = jobs::transition(
                    &self.db,
                    package,
                    JobTransition::FailActive {
                        error: encode(error),
                    },
                )
                .await;
                self.bus.publish(AppEvent::InstallFinished(InstallFinished {
                    package,
                    outcome: InstallOutcome::Failed { code, message },
                }));
                // A blocked install is removed with its partial files.
                if matches!(error, DownloadError::Blocked { .. }) {
                    self.drop_job(package).await;
                }
                result
            }
            End::Checkpoint => {
                jobs::transition(&self.db, package, JobTransition::RequeueActive).await
            }
        };
        // A run that stopped before the engine started (fetching the
        // manifest) reports no pause of its own; listeners such as the
        // updater must see that it is not downloading.
        if matches!(end, End::Paused(_) | End::Checkpoint) {
            let (bytes_done, bytes_total) = bytes.unwrap_or((job.bytes_done, job.bytes_total));
            self.bus
                .publish(AppEvent::InstallProgress(crate::events::InstallProgress {
                    package,
                    phase: crate::events::InstallPhase::Paused,
                    bytes_done,
                    bytes_total,
                    bytes_per_second: 0,
                    eta_seconds: None,
                    connections: 0,
                }));
        }
        if let Err(error) = result {
            tracing::error!(%error, "cannot record the end of a download");
        }
    }

    /// Removes a job and records it in the history, then announces it.
    async fn finish(
        &self,
        package: PackageRef,
        outcome: InstallOutcome,
    ) -> Result<(), JobStoreError> {
        let info = db::installs::catalog_info(&self.db, package)
            .await?
            .unwrap_or_default();
        let outcome_json = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".into());
        jobs::finish(
            &self.db,
            package,
            info.title,
            info.version_label,
            outcome_json,
        )
        .await?;
        self.bus.publish(AppEvent::InstallFinished(InstallFinished {
            package,
            outcome,
        }));
        Ok(())
    }

    async fn drop_job(&self, package: PackageRef) {
        if let Err(error) = self.finish_failed(package).await {
            tracing::error!(%error, "cannot remove a blocked download");
        }
    }

    async fn finish_failed(&self, package: PackageRef) -> Result<(), JobStoreError> {
        let job = self.job(package).await?;
        let (code, message) = match job.as_ref().and_then(decode_state) {
            Some(DownloadState::Failed { error }) => error.summary(),
            _ => ("failed".into(), "The download failed".into()),
        };
        let info = db::installs::catalog_info(&self.db, package)
            .await?
            .unwrap_or_default();
        let outcome = InstallOutcome::Failed { code, message };
        jobs::finish(
            &self.db,
            package,
            info.title,
            info.version_label,
            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".into()),
        )
        .await?;
        Ok(())
    }

    async fn job(&self, package: PackageRef) -> Result<Option<Job>, JobStoreError> {
        Ok(jobs::list(&self.db)
            .await?
            .into_iter()
            .find(|j| j.package == package))
    }

    // ------------------------------------------------------------------ queue actions

    /// Every job and the history, for the Downloads screen.
    pub async fn list(
        &self,
        cover: impl Fn(PackageRef, Uuid) -> Option<String>,
    ) -> Result<DownloadQueue, JobStoreError> {
        let mut jobs_out = Vec::new();
        for job in jobs::list(&self.db).await? {
            let info = db::installs::catalog_info(&self.db, job.package)
                .await?
                .unwrap_or_default();
            let live = self.lock().running.get(&job.package).map(Running::bytes);
            let (bytes_done, bytes_total) = live.unwrap_or((job.bytes_done, job.bytes_total));
            let state = decode_state(&job).unwrap_or(DownloadState::Queued);
            jobs_out.push(DownloadJob {
                package: job.package,
                title: info.title,
                cover_url: info
                    .cover_asset_id
                    .and_then(|asset| cover(job.package, asset)),
                kind: job.kind.into(),
                version_label: info.version_label,
                library_id: job.library_id.to_string(),
                bytes_done,
                bytes_total,
                state,
                queued_at: unix_rfc3339(job.created_at),
            });
        }
        let history = jobs::history(&self.db)
            .await?
            .into_iter()
            .filter_map(|entry| {
                Some(DownloadHistoryEntry {
                    id: entry.id.to_string(),
                    package: entry.package,
                    title: entry.title,
                    kind: entry.kind.into(),
                    version_label: entry.version_label,
                    bytes_total: entry.bytes_total,
                    finished_at: unix_rfc3339(entry.finished_at),
                    outcome: serde_json::from_str(&entry.outcome).ok()?,
                })
            })
            .collect();
        Ok(DownloadQueue {
            jobs: jobs_out,
            history,
        })
    }

    /// Asks a running job to stop. `false` when it is not running (or not
    /// any more: a run that is recording its end is waited for first).
    async fn request_stop(&self, package: PackageRef, stop: Stop) -> bool {
        loop {
            let mut done = {
                let mut state = self.lock();
                let Some(running) = state.running.get_mut(&package) else {
                    return false;
                };
                if !running.finishing {
                    running.stop = Some(stop);
                    match stop {
                        Stop::Pause | Stop::Checkpoint => running.control.pause(),
                        Stop::Cancel { .. } => running.control.cancel(),
                    }
                    return true;
                }
                running.done.subscribe()
            };
            let _ = done.wait_for(|done| *done).await;
        }
    }

    pub async fn pause(&self, package: PackageRef) -> Result<(), DownloadActionError> {
        if self.request_stop(package, Stop::Pause).await {
            return Ok(());
        }
        jobs::transition(&self.db, package, JobTransition::PauseQueued)
            .await
            .map_err(action_error)?;
        self.changed();
        Ok(())
    }

    pub async fn resume(&self, package: PackageRef) -> Result<(), DownloadActionError> {
        jobs::transition(&self.db, package, JobTransition::Resume)
            .await
            .map_err(action_error)?;
        self.changed();
        self.wake();
        Ok(())
    }

    pub async fn retry(&self, package: PackageRef) -> Result<(), DownloadActionError> {
        jobs::transition(&self.db, package, JobTransition::Retry)
            .await
            .map_err(action_error)?;
        self.changed();
        self.wake();
        Ok(())
    }

    /// Stops a job. Without `keep_partial` an install's files are deleted
    /// (only the library marker stays); an update or repair keeps the
    /// installed version.
    pub async fn cancel(
        &self,
        package: PackageRef,
        keep_partial: bool,
    ) -> Result<(), DownloadActionError> {
        if self
            .request_stop(package, Stop::Cancel { keep_partial })
            .await
        {
            return Ok(());
        }
        let job = self
            .job(package)
            .await
            .map_err(action_error)?
            .ok_or(DownloadActionError::NotFound)?;
        if !keep_partial && job.kind == jobs::JobKind::Install {
            self.discard_install(package).await?;
        }
        self.finish(
            package,
            InstallOutcome::Cancelled {
                kept_partial: keep_partial,
            },
        )
        .await
        .map_err(action_error)?;
        if !keep_partial && job.kind == jobs::JobKind::Install {
            db::installs::delete(&self.db, package)
                .await
                .map_err(|e| store_error("forget the install", &e))?;
        }
        self.changed();
        Ok(())
    }

    /// Deletes what an unfinished install wrote (no links followed).
    pub(crate) async fn discard_install(
        &self,
        package: PackageRef,
    ) -> Result<(), DownloadActionError> {
        let Some(row) = db::installs::row(&self.db, package)
            .await
            .map_err(|e| store_error("read the install", &e))?
        else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || job::discard_partial(&row.root))
            .await
            .map_err(|e| store_error("delete the partial install", &e))?
            .map_err(|e| store_error("delete the partial install", &e))
    }

    /// Removes a failed job; its partial files stay (as a cancel with keep).
    pub async fn remove(&self, package: PackageRef) -> Result<(), DownloadActionError> {
        let job = self
            .job(package)
            .await
            .map_err(action_error)?
            .ok_or(DownloadActionError::NotFound)?;
        if job.state == JobState::Active {
            return Err(DownloadActionError::Io {
                detail: "Pause or cancel the download first".into(),
            });
        }
        self.finish_failed(package).await.map_err(action_error)?;
        self.changed();
        Ok(())
    }

    pub async fn reorder(&self, packages: Vec<PackageRef>) -> Result<(), DownloadActionError> {
        jobs::reorder(&self.db, packages)
            .await
            .map_err(action_error)?;
        self.changed();
        self.wake();
        Ok(())
    }

    pub async fn clear_history(&self) -> Result<(), JobStoreError> {
        jobs::clear_history(&self.db).await?;
        self.changed();
        Ok(())
    }

    pub fn settings(&self) -> DownloadSettings {
        self.lock().settings
    }

    /// Saves the settings and applies them to running jobs (bandwidth through
    /// `DownloadControl::set_limit`).
    pub async fn set_settings(
        &self,
        settings: DownloadSettings,
    ) -> Result<DownloadSettings, db::DbError> {
        let settings = DownloadSettings {
            bandwidth_limit_kib: settings.bandwidth_limit_kib.filter(|kib| *kib > 0),
            concurrent_installs: settings
                .concurrent_installs
                .clamp(1, DownloadSettings::MAX_CONCURRENT),
        };
        self.db
            .call(move |conn| {
                settings::set::<ConcurrentInstalls>(conn, &settings.concurrent_installs)?;
                settings::set::<DownloadLimit>(conn, &settings.limit_bytes_per_second())
            })
            .await?;
        {
            let mut state = self.lock();
            state.settings = settings;
            for running in state.running.values() {
                running.control.set_limit(settings.limit_bytes_per_second());
            }
        }
        self.wake();
        Ok(settings)
    }

    /// Before the updater installs (INT's `updater_install`): every running
    /// install pauses at its next checkpoint (written chunks journaled) and
    /// nothing new starts. Resolves once none is running. Paused jobs go back
    /// to the queue and resume from their journal after the restart, or after
    /// [`Self::release_checkpoint`] when the update did not happen.
    pub async fn pause_all_at_checkpoint(&self) {
        let claim = self.claim_gate.lock().await;
        let packages: Vec<PackageRef> = {
            let mut state = self.lock();
            state.held = true;
            state.running.keys().copied().collect()
        };
        drop(claim);
        for package in packages {
            self.request_stop(package, Stop::Checkpoint).await;
        }
        let mut active = self.active.subscribe();
        // The sender lives as long as `self`.
        let _ = active.wait_for(|count| *count == 0).await;
    }

    pub fn release_checkpoint(&self) {
        self.lock().held = false;
        self.wake();
    }

    /// Resumes paused jobs whose reason cleared: the server is reachable
    /// again (they simply try), the library is back, or enough space is free.
    pub async fn recheck(&self) {
        let jobs_list = match jobs::list(&self.db).await {
            Ok(jobs_list) => jobs_list,
            Err(error) => {
                tracing::error!(%error, "cannot read the download queue");
                return;
            }
        };
        let mut resumed = false;
        for job in jobs_list {
            let Some(DownloadState::Paused { reason }) = decode_state(&job) else {
                continue;
            };
            let clear = match reason {
                PauseReason::User => false,
                PauseReason::Offline => true,
                PauseReason::LibraryOffline { .. } | PauseReason::DiskFull { .. } => {
                    self.condition_cleared(&job, &reason).await
                }
            };
            if clear
                && jobs::transition(&self.db, job.package, JobTransition::Resume)
                    .await
                    .is_ok()
            {
                resumed = true;
            }
        }
        if resumed {
            self.changed();
            self.wake();
        }
    }

    /// Engine options for one run.
    pub(crate) fn options(&self) -> DownloadOptions {
        self.options.clone()
    }

    pub fn free_space(&self, path: &std::path::Path) -> u64 {
        (self.free_space)(path)
    }

    pub(crate) fn count_verified(&self, chunks: u64) {
        self.verified
            .fetch_add(chunks, std::sync::atomic::Ordering::Relaxed);
    }

    /// Chunks verified by every run so far.
    pub fn chunks_verified(&self) -> u64 {
        self.verified.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    pub(crate) fn stop_requested(&self, package: PackageRef) -> Option<Stop> {
        self.lock().running.get(&package).and_then(|r| r.stop)
    }
}

fn action_error(error: JobStoreError) -> DownloadActionError {
    match error {
        JobStoreError::NotFound => DownloadActionError::NotFound,
        JobStoreError::InvalidState => DownloadActionError::Io {
            detail: "The download is not in a state that allows this".into(),
        },
        JobStoreError::InvalidOrder => DownloadActionError::Io {
            detail: "The new order must list every waiting download once".into(),
        },
        other => store_error("update the download queue", &other),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
}

/// The UI state of a stored job.
fn decode_state(job: &Job) -> Option<DownloadState> {
    Some(match job.state {
        JobState::Active => DownloadState::Active,
        JobState::Queued => DownloadState::Queued,
        JobState::Paused => DownloadState::Paused {
            reason: match job.error.as_deref() {
                // `PauseQueued` stores the bare word.
                None | Some("user") => PauseReason::User,
                Some(text) => serde_json::from_str(text).unwrap_or(PauseReason::User),
            },
        },
        JobState::Failed => DownloadState::Failed {
            error: job
                .error
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(DownloadError::Server {
                    code: "failed".into(),
                    message: job.error.clone().unwrap_or_default(),
                }),
        },
    })
}

fn unix_rfc3339(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .map(crate::catalog::rfc3339)
        .unwrap_or_default()
}

/// Starts the worker and update detection (INS-04). When the main window
/// gains focus, paused jobs are re-checked (a drive plugged back in, space
/// freed meanwhile) and updates looked for.
pub fn init(app: &tauri::AppHandle, state: &crate::state::AppState) {
    use tauri::Manager as _;
    state.downloads.start();
    let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) else {
        return;
    };
    crate::installs::updates::spawn(
        Arc::clone(&state.catalog),
        state.db.clone(),
        Arc::clone(&state.installs),
        state.bus.clone(),
        Arc::clone(&state.update_triggers),
        state.shutdown.child_token(),
    );
    let downloads = Arc::clone(&state.downloads);
    let triggers = Arc::clone(&state.update_triggers);
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(true) = event {
            let downloads = Arc::clone(&downloads);
            tauri::async_runtime::spawn(async move { downloads.recheck().await });
            triggers.focus.notify_one();
        }
    });
}

#[cfg(test)]
mod tests;
