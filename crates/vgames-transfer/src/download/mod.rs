//! The download engine (02-package-format §7).
//!
//! [`run`] fetches the chunks a [`DownloadSpec`] still needs and writes them
//! into their final files:
//!
//! ```text
//! scheduler ─ranges─▶ N workers (HTTP/1.1, AIMD 2..32) ─stored chunk─▶ bounded queue
//!    ▲  retries/backoff        │ pooled 4 MiB buffers (48)             │
//!    │                         ▼                                       ▼
//!    └──── mismatch ◀── M writer threads: decode (bounded) → BLAKE3 → positional writes
//!                                                                      │
//!                     every 2 s: fsync touched files → journal (atomic) ◀┘
//! ```
//!
//! Invariants: no chunk byte reaches a file before its BLAKE3 matches the
//! (verified) manifest; memory is bounded by the buffer pool; nothing polls
//! while idle (the timers here live only while a download runs); every task
//! stops on pause, cancel or failure. Fresh installs are driven by
//! [`crate::install`], updates and repairs by `crate::update`.

pub mod aimd;
pub mod fetch;
pub mod journal;
pub mod manifest;
pub mod pool;
pub mod scheduler;
pub mod table;
pub mod throttle;
pub mod writer;

use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use vgames_proto::versions::{IntegrityReport, PackUrl};

use crate::fsutil;
use crate::http::{ClientOptions, transfer_client};
use aimd::Aimd;
use fetch::{Timings, UrlCache, WorkerCtx};
use journal::{Bitset, Journal};
use pool::BufferPool;
use scheduler::{Scheduler, Stop};
use table::{ChunkTable, plan_ranges};
use throttle::Throttle;
use writer::{Stats, Targets, WriterShared, Writers};

pub use vgames_core::layout::{CHUNK_SIZE, MAX_STORED_OVERHEAD};

/// The API calls the engine needs (implemented by the launcher's API client
/// and by test doubles).
pub trait PackUrlSource: Send + Sync + 'static {
    /// `POST /v1/versions/{vid}/download-urls` for these packs.
    fn pack_urls(
        &self,
        packs: &[u32],
    ) -> impl Future<Output = Result<Vec<PackUrl>, RemoteError>> + Send;

    /// `POST /v1/versions/{vid}/integrity-reports`.
    fn report_integrity(
        &self,
        report: IntegrityReport,
    ) -> impl Future<Output = Result<(), RemoteError>> + Send;
}

/// An API failure as the engine sees it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RemoteError {
    /// Worth retrying with backoff (network, 5xx, 429).
    pub retryable: bool,
    /// Problem `code` from the API, when there is one (`version_yanked`, …).
    pub code: Option<String>,
    pub message: String,
}

impl RemoteError {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            code: None,
            message: message.into(),
        }
    }

    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            code: None,
            message: message.into(),
        }
    }
}

/// Why a download failed. The journal is kept, so it can be resumed.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// A chunk failed verification twice (reported to the server).
    #[error("the server has a damaged file (pack {pack}, chunk {chunk}): {detail}")]
    Integrity {
        pack: u32,
        chunk: u32,
        detail: String,
    },
    #[error("the download server cannot be reached: {0}")]
    Network(String),
    #[error("the download links for pack {pack} keep being refused (HTTP {status})")]
    LinksRefused { pack: u32, status: u16 },
    #[error("the server refused: {0}")]
    Remote(RemoteError),
    #[error("cannot {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

/// Why a download paused itself or was paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    User,
    /// The disk filled up; resume once space is free (02 §7.10).
    DiskFull,
}

/// Engine settings. Defaults are the values of 02 §7.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub initial_connections: usize,
    pub min_connections: usize,
    pub max_connections: usize,
    /// `false` keeps `initial_connections` fixed (benchmarks, tests).
    pub adaptive: bool,
    pub max_range_bytes: u64,
    /// Chunk buffers (each holds one stored chunk).
    pub buffers: usize,
    pub writer_threads: usize,
    /// Open file handles across all writer threads.
    pub open_handles: usize,
    pub timings: Timings,
    pub journal_interval: Duration,
    pub aimd_interval: Duration,
    pub progress_interval: Duration,
    pub client: ClientOptions,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            initial_connections: 6,
            min_connections: 2,
            max_connections: crate::http::MAX_CONNECTIONS,
            adaptive: true,
            max_range_bytes: 32 * 1024 * 1024,
            buffers: 48,
            writer_threads: 4,
            open_handles: 128,
            timings: Timings {
                stall_timeout: Duration::from_secs(20),
                backoff_base: Duration::from_millis(500),
                backoff_max: Duration::from_secs(30),
                max_attempts: 20,
            },
            journal_interval: Duration::from_secs(2),
            aimd_interval: Duration::from_secs(2),
            progress_interval: Duration::from_millis(250),
            client: ClientOptions::default(),
        }
    }
}

/// What the UI shows (sent at most every `progress_interval`, ≤ 4 Hz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    VerifyingManifest,
    Allocating,
    Downloading,
    Finalizing,
    Paused,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    /// Decoded bytes written (including earlier runs of this download).
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// EWMA (5 s) of the write rate.
    pub bytes_per_second: u64,
    pub eta_seconds: Option<u32>,
    pub connections: u32,
}

impl Progress {
    fn idle(phase: Phase) -> Self {
        Self {
            phase,
            bytes_done: 0,
            bytes_total: 0,
            bytes_per_second: 0,
            eta_seconds: None,
            connections: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Run,
    Pause,
    Cancel,
}

/// Pause, resume, cancel and throttle a download, and watch its progress.
/// Cheap to clone; one per install job.
#[derive(Clone)]
pub struct DownloadControl {
    request: Arc<watch::Sender<Request>>,
    progress: Arc<watch::Sender<Progress>>,
    throttle: Arc<Throttle>,
}

impl DownloadControl {
    /// `limit`: bytes per second, `None` for unlimited.
    pub fn new(limit: Option<u64>) -> Self {
        Self {
            request: Arc::new(watch::Sender::new(Request::Run)),
            progress: Arc::new(watch::Sender::new(Progress::idle(Phase::Queued))),
            throttle: Arc::new(Throttle::new(limit)),
        }
    }

    /// Stops fetching; chunks already received are written and journaled.
    pub fn pause(&self) {
        self.request.send_replace(Request::Pause);
    }

    /// Clears a pause so the next [`run`] continues.
    pub fn resume(&self) {
        self.request.send_replace(Request::Run);
    }

    pub fn cancel(&self) {
        self.request.send_replace(Request::Cancel);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.request.borrow() == Request::Cancel
    }

    /// Waits until a pause or cancel is requested; `true` means cancel.
    pub async fn interrupted(&self) -> bool {
        let mut rx = self.request.subscribe();
        loop {
            match *rx.borrow_and_update() {
                Request::Run => {}
                Request::Pause => return false,
                Request::Cancel => return true,
            }
            if rx.changed().await.is_err() {
                return true;
            }
        }
    }

    pub fn set_limit(&self, bytes_per_second: Option<u64>) {
        self.throttle.set_rate(bytes_per_second);
    }

    pub fn progress(&self) -> watch::Receiver<Progress> {
        self.progress.subscribe()
    }

    pub fn current_progress(&self) -> Progress {
        self.progress.borrow().clone()
    }

    /// Publishes a phase change outside the engine (allocation, finalizing…).
    pub fn set_phase(&self, phase: Phase) {
        self.progress.send_modify(|p| p.phase = phase);
    }

    fn publish(&self, progress: Progress) {
        self.progress.send_if_modified(|current| {
            let changed = *current != progress;
            *current = progress;
            changed
        });
    }
}

/// One download: which chunks, into which files.
pub struct DownloadSpec {
    pub table: Arc<ChunkTable>,
    pub targets: Arc<Targets>,
    /// Chunks already written (loaded from `.vgames/journal.bin`).
    pub journal: Journal,
    /// Chunks this download needs (`None`: all of them).
    pub wanted: Option<Bitset>,
}

/// How [`run`] ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Paused(PauseReason),
    Cancelled,
}

/// Counters of one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunStats {
    pub chunks_verified: u64,
    pub verify_failures: u64,
    pub chunks_written: u64,
    pub bytes_written: u64,
    pub network_bytes: u64,
    pub buffers_allocated: usize,
    pub peak_connections: usize,
}

pub struct RunReport {
    pub outcome: RunOutcome,
    pub stats: RunStats,
    /// The journal as persisted at the end of the run.
    pub journal: Journal,
}

/// Downloads every missing chunk of `spec`. Returns when all are written, or
/// when the download is paused, cancelled or fails (the journal is persisted
/// in every case, so a later run resumes).
pub async fn run<A: PackUrlSource>(
    spec: DownloadSpec,
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
) -> Result<RunReport, DownloadError> {
    let DownloadSpec {
        table,
        targets,
        journal,
        wanted,
    } = spec;
    let done = journal.done().clone();
    let is_wanted = |c: u32| wanted.as_ref().is_none_or(|w| w.get(c));
    let ranges = plan_ranges(
        &table,
        |c| !done.get(c) && is_wanted(c),
        options.max_range_bytes,
    );
    let (mut bytes_total, mut bytes_done) = (0u64, 0u64);
    for (i, chunk) in table.chunks().iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if is_wanted(i) {
            bytes_total += chunk.size;
            if done.get(i) {
                bytes_done += chunk.size;
            }
        }
    }
    let mut meter = Meter::new(bytes_total, bytes_done);
    let packs: Vec<u32> = ranges
        .iter()
        .filter_map(|r| table.chunk(r.first).map(|c| c.pack))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let scheduler = Arc::new(Scheduler::new(&table, ranges));
    let stats = Arc::new(Stats::default());
    let journal = Arc::new(Mutex::new(journal));
    let mut control_rx = control.request.subscribe();

    let finish = |outcome,
                  stats: RunStats,
                  journal: Arc<Mutex<Journal>>|
     -> Result<RunReport, DownloadError> {
        let journal = Arc::try_unwrap(journal)
            .map_err(|_| DownloadError::Internal("journal still shared".into()))?
            .into_inner()
            .map_err(|_| DownloadError::Internal("journal lock poisoned".into()))?;
        Ok(RunReport {
            outcome,
            stats,
            journal,
        })
    };

    match *control_rx.borrow_and_update() {
        Request::Cancel => return finish(RunOutcome::Cancelled, RunStats::default(), journal),
        Request::Pause => {
            control.set_phase(Phase::Paused);
            return finish(
                RunOutcome::Paused(PauseReason::User),
                RunStats::default(),
                journal,
            );
        }
        Request::Run => {}
    }
    if scheduler.remaining() == 0 {
        return finish(RunOutcome::Completed, RunStats::default(), journal);
    }
    control.publish(meter.progress(Phase::Downloading, bytes_done, 0));

    let urls = UrlCache::new();
    tokio::select! {
        result = urls.prefetch(&*api, &packs) => match result {
            Ok(()) => {}
            // Workers fetch missing links per pack, with backoff.
            Err(error) if error.retryable => {
                tracing::warn!(%error, "cannot prefetch download links; fetching them per pack");
            }
            Err(error) => return Err(DownloadError::Remote(error)),
        },
        _ = wait_for_stop_request(&mut control_rx) => {
            let outcome = match *control_rx.borrow() {
                Request::Cancel => RunOutcome::Cancelled,
                _ => RunOutcome::Paused(PauseReason::User),
            };
            return finish(outcome, RunStats::default(), journal);
        }
    }

    let client = transfer_client(&options.client)
        .map_err(|e| DownloadError::Internal(format!("HTTP client: {e}")))?;
    let fresh_client = transfer_client(&ClientOptions {
        reuse_connections: false,
        ..options.client.clone()
    })
    .map_err(|e| DownloadError::Internal(format!("HTTP client: {e}")))?;

    let buffer_capacity = usize::try_from(CHUNK_SIZE + MAX_STORED_OVERHEAD).unwrap_or(usize::MAX);
    let pool = BufferPool::new(options.buffers.max(1), buffer_capacity);
    let writer_threads = options.writer_threads.max(1);
    let shared = Arc::new(WriterShared {
        table: Arc::clone(&table),
        targets: Arc::clone(&targets),
        scheduler: Arc::clone(&scheduler),
        durable: Mutex::new(writer::Durable::default()),
        stats: Arc::clone(&stats),
        handles_per_thread: (options.open_handles / writer_threads).max(1),
        scratch_capacity: buffer_capacity,
    });
    let writers = Writers::start(writer_threads, options.buffers.max(1), Arc::clone(&shared))
        .map_err(|e| DownloadError::Internal(format!("cannot start writer threads: {e}")))?;
    let Some(sender) = writers.sender() else {
        return Err(DownloadError::Internal("writer queue closed".into()));
    };
    let initial = options.initial_connections.clamp(
        options.min_connections.max(1),
        options.max_connections.max(1),
    );
    let ctx = Arc::new(WorkerCtx {
        table: Arc::clone(&table),
        scheduler: Arc::clone(&scheduler),
        pool: Arc::clone(&pool),
        sender,
        client,
        fresh_client,
        urls,
        api: Arc::clone(&api),
        throttle: Arc::clone(&control.throttle),
        stats: Arc::clone(&stats),
        timings: options.timings,
        active: AtomicUsize::new(0),
        target: AtomicUsize::new(initial),
    });

    let journal_stop = CancellationToken::new();
    let journal_task = tokio::spawn(journal_loop(
        Arc::clone(&journal),
        Arc::clone(&shared),
        options.journal_interval,
        journal_stop.clone(),
    ));

    let mut workers = JoinSet::new();
    let mut peak = spawn_workers(&ctx, &mut workers);
    let mut aimd = Aimd::new(
        initial,
        options.min_connections.max(1),
        options.max_connections.max(1),
    );
    let mut aimd_tick = tokio::time::interval_at(
        (Instant::now() + options.aimd_interval).into(),
        options.aimd_interval,
    );
    let mut progress_tick = tokio::time::interval(options.progress_interval);
    progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    aimd_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut control_open = true;

    loop {
        tokio::select! {
            joined = workers.join_next(), if !workers.is_empty() => {
                if let Some(Err(error)) = joined && error.is_panic() {
                    scheduler.stop(Stop::Failed(DownloadError::Internal("a download worker panicked".into())));
                }
                if workers.is_empty() {
                    if !scheduler.is_running() {
                        break;
                    }
                    peak = peak.max(spawn_workers(&ctx, &mut workers));
                }
            }
            _ = aimd_tick.tick() => {
                let congestion = scheduler.take_congestion();
                let bytes = stats.network_bytes.load(Ordering::Relaxed);
                if options.adaptive {
                    let throttled = control.throttle.rate().is_some();
                    let target = aimd.update(bytes, congestion, throttled, Instant::now());
                    ctx.target.store(target, Ordering::SeqCst);
                    if scheduler.is_running() {
                        peak = peak.max(spawn_workers(&ctx, &mut workers));
                    }
                }
            }
            _ = progress_tick.tick() => {
                let done_now = bytes_done + stats.bytes_written.load(Ordering::Relaxed);
                let connections = ctx.active.load(Ordering::Relaxed);
                control.publish(meter.progress(Phase::Downloading, done_now, connections));
            }
            changed = control_rx.changed(), if control_open => {
                if changed.is_err() {
                    control_open = false;
                    continue;
                }
                match *control_rx.borrow_and_update() {
                    Request::Pause => scheduler.stop(Stop::Paused(PauseReason::User)),
                    Request::Cancel => scheduler.stop(Stop::Cancelled),
                    Request::Run => {}
                }
            }
        }
    }

    // Workers are gone; closing the queue lets the writers drain and exit.
    drop(ctx);
    writers.finish().await;
    journal_stop.cancel();
    let _ = journal_task.await;
    let flushed = flush_journal(&journal, &shared).await;
    drop(shared);

    let run_stats = RunStats {
        chunks_verified: stats.chunks_verified.load(Ordering::Relaxed),
        verify_failures: stats.verify_failures.load(Ordering::Relaxed),
        chunks_written: stats.chunks_written.load(Ordering::Relaxed),
        bytes_written: stats.bytes_written.load(Ordering::Relaxed),
        network_bytes: stats.network_bytes.load(Ordering::Relaxed),
        buffers_allocated: pool.allocated(),
        peak_connections: peak,
    };
    let done_now = bytes_done + run_stats.bytes_written;
    let outcome = match scheduler.take_stop() {
        None => RunOutcome::Completed,
        Some(Stop::Paused(reason)) => RunOutcome::Paused(reason),
        Some(Stop::Cancelled) => RunOutcome::Cancelled,
        Some(Stop::Failed(error)) => {
            if let Some(failure) = scheduler.integrity_failure() {
                report_integrity(&*api, failure).await;
            }
            control.publish(meter.progress(Phase::Paused, done_now, 0));
            flushed?;
            return Err(error);
        }
    };
    flushed?;
    let phase = match outcome {
        RunOutcome::Completed => Phase::Finalizing,
        RunOutcome::Paused(_) | RunOutcome::Cancelled => Phase::Paused,
    };
    control.publish(meter.progress(phase, done_now, 0));
    finish(outcome, run_stats, journal)
}

async fn wait_for_stop_request(rx: &mut watch::Receiver<Request>) {
    loop {
        if *rx.borrow_and_update() != Request::Run {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Spawns workers up to the target; returns how many are alive.
fn spawn_workers<A: PackUrlSource>(ctx: &Arc<WorkerCtx<A>>, workers: &mut JoinSet<()>) -> usize {
    let target = ctx.target.load(Ordering::SeqCst);
    while ctx.active.load(Ordering::SeqCst) < target {
        ctx.active.fetch_add(1, Ordering::SeqCst);
        workers.spawn(fetch::worker(Arc::clone(ctx)));
    }
    ctx.active.load(Ordering::SeqCst)
}

async fn report_integrity<A: PackUrlSource>(api: &A, failure: scheduler::IntegrityFailure) {
    let report = IntegrityReport {
        pack_index: failure.pack,
        chunk_index: Some(failure.chunk),
        detail: Some(failure.detail.chars().take(1000).collect()),
    };
    match tokio::time::timeout(Duration::from_secs(15), api.report_integrity(report)).await {
        Ok(Ok(())) => tracing::warn!(
            pack = failure.pack,
            chunk = failure.chunk,
            "reported a damaged chunk to the server"
        ),
        Ok(Err(error)) => tracing::warn!(%error, "cannot send the integrity report"),
        Err(_) => tracing::warn!("the integrity report timed out"),
    }
}

async fn journal_loop(
    journal: Arc<Mutex<Journal>>,
    shared: Arc<WriterShared>,
    interval: Duration,
    stop: CancellationToken,
) {
    let mut tick = tokio::time::interval_at((Instant::now() + interval).into(), interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = tick.tick() => {
                if let Err(error) = flush_journal(&journal, &shared).await {
                    shared.scheduler.stop(Stop::Failed(error));
                    return;
                }
            }
        }
    }
}

/// fsyncs the files written since the last flush, then records their chunks
/// in the journal and persists it atomically (02 §7.9).
async fn flush_journal(
    journal: &Arc<Mutex<Journal>>,
    shared: &Arc<WriterShared>,
) -> Result<(), DownloadError> {
    let pending = match shared.durable.lock() {
        Ok(mut durable) => std::mem::take(&mut *durable),
        Err(_) => return Err(DownloadError::Internal("journal state poisoned".into())),
    };
    if pending.chunks.is_empty() {
        return Ok(());
    }
    let journal = Arc::clone(journal);
    let targets = Arc::clone(&shared.targets);
    tokio::task::spawn_blocking(move || {
        for file in &pending.files {
            let Some(Some(path)) = targets.get(*file as usize) else {
                continue;
            };
            fsutil::open_for_write(path)
                .and_then(|f| f.sync_data())
                .map_err(|source| DownloadError::Io {
                    op: "flush",
                    path: path.clone(),
                    source,
                })?;
        }
        let mut journal = journal
            .lock()
            .map_err(|_| DownloadError::Internal("journal lock poisoned".into()))?;
        for chunk in &pending.chunks {
            journal.mark(*chunk);
        }
        journal.persist().map_err(|source| DownloadError::Io {
            op: "write",
            path: journal.path().to_owned(),
            source,
        })
    })
    .await
    .map_err(|e| DownloadError::Internal(format!("journal flush: {e}")))?
}

/// Progress arithmetic: EWMA rate with a 5 s time constant, and ETA.
struct Meter {
    total: u64,
    last_done: u64,
    last_at: Instant,
    rate: f64,
}

impl Meter {
    fn new(total: u64, done: u64) -> Self {
        Self {
            total,
            last_done: done,
            last_at: Instant::now(),
            rate: 0.0,
        }
    }

    fn progress(&mut self, phase: Phase, done: u64, connections: usize) -> Progress {
        let now = Instant::now();
        let dt = now.duration_since(self.last_at).as_secs_f64();
        if dt > 0.0 {
            let instant = done.saturating_sub(self.last_done) as f64 / dt;
            let alpha = 1.0 - (-dt / 5.0).exp();
            self.rate += alpha * (instant - self.rate);
            self.last_done = done;
            self.last_at = now;
        }
        let remaining = self.total.saturating_sub(done);
        let eta_seconds = (self.rate >= 1.0 && phase == Phase::Downloading).then(|| {
            (remaining as f64 / self.rate)
                .ceil()
                .min(f64::from(u32::MAX)) as u32
        });
        Progress {
            phase,
            bytes_done: done.min(self.total),
            bytes_total: self.total,
            bytes_per_second: self.rate.max(0.0) as u64,
            eta_seconds,
            connections: u32::try_from(connections).unwrap_or(u32::MAX),
        }
    }
}
