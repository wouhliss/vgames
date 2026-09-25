//! Parallel pack uploads: each pack streams from its source files
//! (`vgames_pack::PackSource`, never written to disk) into its own resumable
//! session in 16 MiB pieces. 4–16 packs in flight, adapted like downloads
//! (AIMD). Failures query the session and resume from the confirmed offset.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_pack::PackSource;
use vgames_pack::source::{PackReader, SourceError};

use super::resume::{PackState, ResumeFile, ResumeState};
use super::session::{self, SessionError, SessionStatus};
use super::{PublishApi, UploadError, UploadOptions};
use crate::download::RemoteError;
use crate::download::aimd::Aimd;

/// Shared state of one pack-upload run.
pub struct PackUploads<A> {
    pub api: Arc<A>,
    pub source: PackSource,
    pub version_id: Uuid,
    pub client: reqwest::Client,
    pub options: UploadOptions,
    pub state: Mutex<ResumeState>,
    pub resume: Option<ResumeFile>,
    pub cancel: CancellationToken,
    /// Bytes the storage server confirmed, all packs.
    pub confirmed: AtomicU64,
    pub congestion: AtomicU64,
    pub active: AtomicUsize,
    pub target: AtomicUsize,
    queue: Mutex<VecDeque<u32>>,
    failure: Mutex<Option<UploadError>>,
    last_save: Mutex<Instant>,
}

impl<A: PublishApi> PackUploads<A> {
    pub fn new(
        api: Arc<A>,
        source: PackSource,
        state: ResumeState,
        resume: Option<ResumeFile>,
        client: reqwest::Client,
        options: UploadOptions,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        let queue = (0..source.packing.pack_count()).collect();
        let confirmed = state.packs.values().map(|p| p.confirmed).sum();
        Arc::new(Self {
            api,
            version_id: state.version_id,
            source,
            client,
            target: AtomicUsize::new(options.initial_parallel),
            options,
            state: Mutex::new(state),
            resume,
            cancel,
            confirmed: AtomicU64::new(confirmed),
            congestion: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            queue: Mutex::new(queue),
            failure: Mutex::new(None),
            last_save: Mutex::new(Instant::now()),
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.source.packing.total_stored()
    }

    fn fail(&self, error: UploadError) {
        if let Ok(mut failure) = self.failure.lock()
            && failure.is_none()
        {
            *failure = Some(error);
        }
        self.cancel.cancel();
    }

    pub fn take_failure(&self) -> Option<UploadError> {
        self.failure.lock().ok().and_then(|mut f| f.take())
    }

    fn pack_state(&self, pack: u32) -> PackState {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.packs.get(&pack).cloned())
            .unwrap_or(PackState {
                session: None,
                confirmed: 0,
                complete: false,
            })
    }

    /// Records a pack's progress; persists the resume file now when `force`
    /// (new session, completion) or at most once a second otherwise.
    async fn record(&self, pack: u32, update: PackState, force: bool) {
        let snapshot = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let previous = state.packs.get(&pack).map_or(0, |p| p.confirmed);
            if update.confirmed > previous {
                self.confirmed
                    .fetch_add(update.confirmed - previous, Ordering::Relaxed);
            } else if update.confirmed < previous {
                self.confirmed
                    .fetch_sub(previous - update.confirmed, Ordering::Relaxed);
            }
            state.packs.insert(pack, update);
            let due = self
                .last_save
                .lock()
                .map(|mut last| {
                    let due = force || last.elapsed() >= Duration::from_secs(1);
                    if due {
                        *last = Instant::now();
                    }
                    due
                })
                .unwrap_or(true);
            due.then(|| state.clone())
        };
        if let (Some(snapshot), Some(resume)) = (snapshot, &self.resume) {
            let path = resume.path().to_owned();
            let result =
                tokio::task::spawn_blocking(move || ResumeFile::new(path).save(&snapshot)).await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("cannot save the upload resume file");
            }
        }
    }

    /// Saves the resume file now (end of run).
    pub async fn persist(&self) {
        let snapshot = self.state.lock().ok().map(|s| s.clone());
        if let (Some(snapshot), Some(resume)) = (snapshot, &self.resume) {
            let path = resume.path().to_owned();
            let _ =
                tokio::task::spawn_blocking(move || ResumeFile::new(path).save(&snapshot)).await;
        }
    }
}

/// Uploads every pack; returns when all are stored, or on the first fatal error.
pub async fn run<A: PublishApi>(
    uploads: Arc<PackUploads<A>>,
    mut on_tick: impl FnMut(u64, usize),
) -> Result<(), UploadError> {
    let mut workers = JoinSet::new();
    spawn_workers(&uploads, &mut workers);
    let options = &uploads.options;
    let mut aimd = Aimd::new(
        options.initial_parallel,
        options.min_parallel,
        options.max_parallel,
    );
    let mut aimd_tick = tokio::time::interval_at(
        (Instant::now() + options.aimd_interval).into(),
        options.aimd_interval,
    );
    let mut progress_tick = tokio::time::interval(options.progress_interval);
    loop {
        // Checked on every turn, not only when a worker exits: a cancel before
        // any worker started, or a package without packs, must end the run.
        if workers.is_empty() {
            let pending = uploads.queue.lock().map(|q| !q.is_empty()).unwrap_or(false);
            if uploads.cancel.is_cancelled() || !pending {
                break;
            }
            spawn_workers(&uploads, &mut workers);
            if workers.is_empty() {
                break;
            }
        }
        tokio::select! {
            joined = workers.join_next(), if !workers.is_empty() => {
                if let Some(Err(error)) = joined && error.is_panic() {
                    uploads.fail(UploadError::Internal("an upload worker panicked".into()));
                }
            }
            _ = aimd_tick.tick() => {
                let congestion = uploads.congestion.swap(0, Ordering::Relaxed);
                let bytes = uploads.confirmed.load(Ordering::Relaxed);
                let target = aimd.update(bytes, congestion, false, Instant::now());
                uploads.target.store(target, Ordering::SeqCst);
                if !uploads.cancel.is_cancelled() {
                    spawn_workers(&uploads, &mut workers);
                }
            }
            _ = progress_tick.tick() => {
                on_tick(uploads.confirmed.load(Ordering::Relaxed), uploads.active.load(Ordering::Relaxed));
            }
        }
    }
    on_tick(uploads.confirmed.load(Ordering::Relaxed), 0);
    uploads.persist().await;
    if let Some(error) = uploads.take_failure() {
        return Err(error);
    }
    if uploads.cancel.is_cancelled() {
        return Err(UploadError::Cancelled);
    }
    Ok(())
}

fn spawn_workers<A: PublishApi>(uploads: &Arc<PackUploads<A>>, workers: &mut JoinSet<()>) {
    let target = uploads.target.load(Ordering::SeqCst);
    let pending = uploads.queue.lock().map(|q| q.len()).unwrap_or(0);
    let mut spawned = 0;
    while uploads.active.load(Ordering::SeqCst) < target
        && spawned < pending
        && !uploads.cancel.is_cancelled()
    {
        uploads.active.fetch_add(1, Ordering::SeqCst);
        workers.spawn(worker(Arc::clone(uploads)));
        spawned += 1;
    }
}

async fn worker<A: PublishApi>(uploads: Arc<PackUploads<A>>) {
    loop {
        let leave = uploads
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                (active > uploads.target.load(Ordering::SeqCst)).then(|| active - 1)
            })
            .is_ok();
        if leave {
            return;
        }
        let next = uploads.queue.lock().ok().and_then(|mut q| q.pop_front());
        let Some(pack) = next.filter(|_| !uploads.cancel.is_cancelled()) else {
            uploads.active.fetch_sub(1, Ordering::SeqCst);
            return;
        };
        if let Err(error) = upload_pack(&uploads, pack).await {
            uploads.fail(error);
        }
    }
}

/// Reads up to `max` bytes (a whole piece unless the pack ends).
fn read_piece(mut reader: PackReader, max: usize) -> (PackReader, std::io::Result<Vec<u8>>) {
    let mut buf = vec![0u8; max];
    let mut filled = 0;
    while filled < max {
        match reader.read(buf.get_mut(filled..).unwrap_or_default()) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return (reader, Err(e)),
        }
    }
    buf.truncate(filled);
    (reader, Ok(buf))
}

fn source_error(error: std::io::Error) -> UploadError {
    match error
        .get_ref()
        .and_then(|e| e.downcast_ref::<SourceError>())
    {
        Some(SourceError::Changed { path }) => UploadError::SourceChanged { path: path.clone() },
        Some(SourceError::ChunkChanged { chunk }) => UploadError::SourceChanged {
            path: format!("(bytes of chunk {chunk})"),
        },
        _ => UploadError::Source(error.to_string()),
    }
}

async fn open_at<A>(
    uploads: &PackUploads<A>,
    pack: u32,
    offset: u64,
) -> Result<PackReader, UploadError> {
    let source = uploads.source.clone();
    tokio::task::spawn_blocking(move || source.open_pack(pack, offset))
        .await
        .map_err(|e| UploadError::Internal(e.to_string()))?
        .map_err(|e| UploadError::Source(e.to_string()))
}

/// Streams the rest of the pack through the hashers without uploading it
/// (a pack stored in an earlier run still needs its hashes for the manifest).
async fn hash_only<A>(uploads: &PackUploads<A>, pack: u32, from: u64) -> Result<(), UploadError> {
    let mut reader = open_at(uploads, pack, from).await?;
    loop {
        let (back, piece) =
            tokio::task::spawn_blocking(move || read_piece(reader, 16 * 1024 * 1024))
                .await
                .map_err(|e| UploadError::Internal(e.to_string()))?;
        reader = back;
        if piece.map_err(source_error)?.is_empty() {
            return Ok(());
        }
    }
}

/// Retries a remote call on retryable errors with backoff.
async fn remote<T, F, Fut>(
    uploads: &PackUploads<impl PublishApi>,
    mut call: F,
) -> Result<T, UploadError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteError>>,
{
    let mut attempt = 0u32;
    loop {
        match call().await {
            Ok(v) => return Ok(v),
            Err(e) if e.retryable && attempt < uploads.options.max_attempts => {
                attempt += 1;
                sleep_or_cancel(uploads, backoff(&uploads.options, attempt)).await?;
            }
            Err(e) => return Err(UploadError::Remote(e)),
        }
    }
}

fn backoff(options: &UploadOptions, attempt: u32) -> Duration {
    let cap = options
        .backoff_base
        .saturating_mul(1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX))
        .min(options.backoff_max);
    let fraction = getrandom::u64().map_or(1.0, |r| (r >> 11) as f64 / (1u64 << 53) as f64);
    cap.mul_f64(fraction)
}

async fn sleep_or_cancel<A>(uploads: &PackUploads<A>, delay: Duration) -> Result<(), UploadError> {
    tokio::select! {
        () = uploads.cancel.cancelled() => Err(UploadError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Opens a new resumable session for `pack`.
async fn new_session<A: PublishApi>(
    uploads: &PackUploads<A>,
    pack: u32,
) -> Result<String, UploadError> {
    let timeout = uploads.options.request_timeout;
    let mut refused = 0;
    loop {
        let target = remote(uploads, || {
            uploads.api.pack_upload_session(uploads.version_id, pack)
        })
        .await?;
        match session::start(&uploads.client, &target.url, &target.headers, timeout).await {
            Ok(uri) => return Ok(uri),
            Err(SessionError::Transient(_)) if refused < uploads.options.max_attempts => {
                refused += 1;
                uploads.congestion.fetch_add(1, Ordering::Relaxed);
                sleep_or_cancel(uploads, backoff(&uploads.options, refused)).await?;
            }
            // An expired start URL: ask for a fresh one (twice at most).
            Err(SessionError::Refused { .. } | SessionError::Gone(_)) if refused < 2 => {
                refused += 1
            }
            Err(e) => return Err(UploadError::Session(e)),
        }
    }
}

async fn upload_pack<A: PublishApi>(
    uploads: &PackUploads<A>,
    pack: u32,
) -> Result<(), UploadError> {
    let Some(size) = uploads.source.pack_size(pack) else {
        return Err(UploadError::Internal(format!("no pack {pack}")));
    };
    let piece_size = uploads.options.piece_size;
    let timeout = uploads.options.piece_timeout;
    let mut state = uploads.pack_state(pack);
    if state.complete {
        return hash_only(uploads, pack, size).await;
    }
    let mut failures = 0u32;
    // `known`: the server's confirmed offset is known in this run (no query needed).
    let mut known = false;
    loop {
        if uploads.cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }
        let session_uri = match &state.session {
            Some(uri) => uri.clone(),
            None => {
                let uri = new_session(uploads, pack).await?;
                state = PackState {
                    session: Some(uri.clone()),
                    confirmed: 0,
                    complete: false,
                };
                uploads.record(pack, state.clone(), true).await;
                known = true;
                uri
            }
        };
        if !known {
            match session::query(
                &uploads.client,
                &session_uri,
                size,
                uploads.options.request_timeout,
            )
            .await
            {
                Ok(SessionStatus::Complete) => {
                    state.confirmed = size;
                    state.complete = true;
                    uploads.record(pack, state.clone(), true).await;
                    return hash_only(uploads, pack, size).await;
                }
                Ok(SessionStatus::Incomplete(n)) => {
                    state.confirmed = n.min(size);
                    known = true;
                }
                Err(SessionError::Gone(_)) => {
                    tracing::info!(pack, "upload session expired; starting a new one");
                    state.session = None;
                    state.confirmed = 0;
                    continue;
                }
                Err(SessionError::Transient(reason)) => {
                    failures += 1;
                    if failures > uploads.options.max_attempts {
                        return Err(UploadError::Session(SessionError::Transient(reason)));
                    }
                    uploads.congestion.fetch_add(1, Ordering::Relaxed);
                    sleep_or_cancel(uploads, backoff(&uploads.options, failures)).await?;
                    continue;
                }
                Err(e) => return Err(UploadError::Session(e)),
            }
        }

        let mut reader = open_at(uploads, pack, state.confirmed).await?;
        let mut offset = state.confirmed;
        let mut resync = false;
        while offset < size {
            let want =
                usize::try_from((size - offset).min(piece_size as u64)).unwrap_or(piece_size);
            let (back, piece) = tokio::task::spawn_blocking(move || read_piece(reader, want))
                .await
                .map_err(|e| UploadError::Internal(e.to_string()))?;
            reader = back;
            let piece = piece.map_err(source_error)?;
            if piece.len() != want {
                return Err(UploadError::SourceChanged {
                    path: format!("(pack {pack} is shorter than planned)"),
                });
            }
            let len = piece.len() as u64;
            let sent = tokio::select! {
                () = uploads.cancel.cancelled() => return Err(UploadError::Cancelled),
                r = session::put_piece(&uploads.client, &session_uri, offset, bytes::Bytes::from(piece), size, timeout) => r,
            };
            match sent {
                Ok(SessionStatus::Complete) => {
                    offset = size;
                }
                Ok(SessionStatus::Incomplete(n)) if n == offset + len => {
                    offset = n;
                }
                Ok(SessionStatus::Incomplete(n)) => {
                    // The server kept fewer (or more) bytes than sent: resend from there.
                    tracing::debug!(
                        pack,
                        confirmed = n,
                        "partial piece; resuming from the confirmed offset"
                    );
                    state.confirmed = n.min(size);
                    known = true;
                    resync = true;
                    break;
                }
                Err(SessionError::Gone(_)) => {
                    state.session = None;
                    state.confirmed = 0;
                    resync = true;
                    break;
                }
                Err(SessionError::Transient(reason)) => {
                    failures += 1;
                    if failures > uploads.options.max_attempts {
                        return Err(UploadError::Session(SessionError::Transient(reason)));
                    }
                    uploads.congestion.fetch_add(1, Ordering::Relaxed);
                    sleep_or_cancel(uploads, backoff(&uploads.options, failures)).await?;
                    // Where did it stop? Ask the server (02 §6: `bytes */*`).
                    known = false;
                    resync = true;
                    break;
                }
                Err(e @ (SessionError::Refused { .. } | SessionError::Protocol(_))) => {
                    failures += 1;
                    if failures > 2 {
                        return Err(UploadError::Session(e));
                    }
                    known = false;
                    resync = true;
                    break;
                }
            }
            state.confirmed = offset;
            failures = 0;
            uploads.record(pack, state.clone(), false).await;
        }
        if resync {
            continue;
        }
        // Drive the reader to its end so the pack hash is recorded (the
        // server may report completion before the last piece was read).
        loop {
            let (back, rest) = tokio::task::spawn_blocking(move || read_piece(reader, piece_size))
                .await
                .map_err(|e| UploadError::Internal(e.to_string()))?;
            reader = back;
            if rest.map_err(source_error)?.is_empty() {
                break;
            }
        }
        state.confirmed = size;
        state.complete = true;
        uploads.record(pack, state, true).await;
        return Ok(());
    }
}
