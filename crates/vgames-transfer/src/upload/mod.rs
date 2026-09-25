//! Resumable pack uploads (02-package-format §6).
//!
//! Packs stream directly from [`PackSource`]; no pack files are staged on disk.
//! A private resume record binds session URIs to a version and the source scan.
//! After any ambiguous PUT failure, the server's confirmed offset wins. Keep
//! the resume record outside the source tree, in the user's app-data directory.

mod protocol;
pub mod publish;
mod resume;

use std::future::Future;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_pack::PackSource;
use vgames_pack::source::PackReader;
use vgames_proto::versions::UploadTarget;

use crate::download::{RemoteError, aimd::Aimd};
use crate::http::{ClientOptions, transfer_client};
use resume::{PackResume, ResumeStore};

pub use protocol::ProtocolError;

pub const PIECE_SIZE: usize = 16 * 1024 * 1024;

/// Implemented by the authenticated launcher/CLI API client. Credentials never
/// reach the storage client; upload targets authorize just one pack.
pub trait UploadApi: Send + Sync + 'static {
    fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> impl Future<Output = Result<UploadTarget, RemoteError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("upload cancelled; it can be resumed")]
    Cancelled,
    #[error("upload protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("the server refused the upload")]
    Remote { retryable: bool },
    #[error("cannot read the upload source: {0}")]
    Source(#[from] vgames_pack::source::SourceError),
    #[error("cannot read or persist upload data: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Resume(&'static str),
    #[error("upload stopped after repeated network failures; resume to try again")]
    RetryLimit,
    #[error("invalid upload options")]
    Options,
    #[error("internal upload task failed")]
    Internal,
}

#[derive(Debug, Clone)]
pub struct UploadOptions {
    pub client_options: ClientOptions,
    pub request_timeout: Duration,
    pub initial_connections: usize,
    pub max_connections: usize,
    pub max_retries: u32,
    pub retry_base: Duration,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            client_options: ClientOptions::default(),
            request_timeout: Duration::from_secs(120),
            initial_connections: 6,
            max_connections: 16,
            max_retries: 8,
            retry_base: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadPhase {
    Uploading,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone)]
pub struct UploadProgress {
    pub phase: UploadPhase,
    pub bytes_confirmed: u64,
    pub bytes_total: u64,
    pub bytes_per_second: f64,
    pub active_packs: usize,
}

#[derive(Clone)]
pub struct UploadControl {
    pub cancel: CancellationToken,
    progress: watch::Sender<UploadProgress>,
}

impl Default for UploadControl {
    fn default() -> Self {
        Self::new()
    }
}

impl UploadControl {
    pub fn new() -> Self {
        let (progress, _) = watch::channel(UploadProgress {
            phase: UploadPhase::Uploading,
            bytes_confirmed: 0,
            bytes_total: 0,
            bytes_per_second: 0.0,
            active_packs: 0,
        });
        Self {
            cancel: CancellationToken::new(),
            progress,
        }
    }

    pub fn progress(&self) -> watch::Receiver<UploadProgress> {
        self.progress.subscribe()
    }
}

struct Shared {
    source: PackSource,
    store: Arc<ResumeStore>,
    client: reqwest::Client,
    cancel: CancellationToken,
    options: UploadOptions,
    offsets: Vec<AtomicU64>,
    congestion: AtomicU64,
}

impl Shared {
    fn confirmed(&self) -> u64 {
        self.offsets.iter().map(|v| v.load(Ordering::Relaxed)).sum()
    }

    async fn checkpoint(&self, pack: u32, session: &str, offset: u64) -> Result<(), UploadError> {
        self.store
            .save(
                pack,
                PackResume {
                    session: session.into(),
                    offset,
                },
            )
            .await?;
        self.offsets
            .get(pack as usize)
            .ok_or(UploadError::Internal)?
            .store(offset, Ordering::Relaxed);
        Ok(())
    }
}

/// Upload all packs with AIMD concurrency (4–16), returning only after hashes
/// are complete. Call again with a freshly scanned identical source to resume
/// after cancellation/crash. Persist the Version before calling this function.
pub async fn run<A: UploadApi>(
    source: PackSource,
    version_id: Uuid,
    api: Arc<A>,
    resume_path: PathBuf,
    options: &UploadOptions,
    control: &UploadControl,
) -> Result<(), UploadError> {
    if !(4..=16).contains(&options.max_connections)
        || !(4..=options.max_connections).contains(&options.initial_connections)
        || options.request_timeout.is_zero()
        || options.max_retries == 0
    {
        return Err(UploadError::Options);
    }
    if control.cancel.is_cancelled() {
        return Err(UploadError::Cancelled);
    }
    let source_copy = source.clone();
    let store = tokio::task::spawn_blocking(move || {
        ResumeStore::open(resume_path, version_id, &source_copy)
    })
    .await
    .map_err(|_| UploadError::Internal)??;
    let count = source.packing.pack_count();
    let total = source.packing.total_stored();
    let client = transfer_client(&options.client_options).map_err(|_| UploadError::Internal)?;
    let shared = Arc::new(Shared {
        source,
        store: Arc::new(store),
        client,
        cancel: control.cancel.child_token(),
        options: options.clone(),
        offsets: (0..count).map(|_| AtomicU64::new(0)).collect(),
        congestion: AtomicU64::new(0),
    });
    let mut workers = JoinSet::new();
    let mut next = 0u32;
    let mut aimd = Aimd::new(options.initial_connections, 4, options.max_connections);
    let mut update = tokio::time::interval(Duration::from_millis(250));
    update.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_control = Instant::now();
    let mut previous_bytes = 0u64;
    let mut previous_at = Instant::now();
    let mut rate = 0.0;
    let mut result = Ok(());
    loop {
        while next < count && workers.len() < aimd.target() {
            let (shared, api, pack) = (Arc::clone(&shared), Arc::clone(&api), next);
            workers.spawn(async move { upload_pack(shared, api, version_id, pack).await });
            next += 1;
        }
        if workers.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            () = control.cancel.cancelled() => { result = Err(UploadError::Cancelled); break; }
            joined = workers.join_next() => match joined {
                Some(Ok(Ok(()))) => {},
                Some(Ok(Err(error))) => { result = Err(error); break; },
                _ => { result = Err(UploadError::Internal); break; }
            },
            _ = update.tick() => {
                let now = Instant::now();
                let bytes = shared.confirmed();
                let elapsed = now.duration_since(previous_at).as_secs_f64();
                let measured = bytes.saturating_sub(previous_bytes) as f64 / elapsed.max(0.001);
                rate += (1.0 - (-elapsed / 5.0).exp()) * (measured - rate);
                previous_at = now;
                previous_bytes = bytes;
                if now.duration_since(last_control) >= Duration::from_secs(2) {
                    aimd.update(bytes, shared.congestion.swap(0, Ordering::Relaxed), false, now);
                    last_control = now;
                }
                control.progress.send_replace(UploadProgress {
                    phase: UploadPhase::Uploading, bytes_confirmed: bytes, bytes_total: total,
                    bytes_per_second: rate, active_packs: workers.len(),
                });
            }
        }
    }
    shared.cancel.cancel();
    // Drain workers, including in-flight blocking reads/checkpoints, before
    // releasing the resume lock. A later run cannot race detached writes.
    while workers.join_next().await.is_some() {}
    if result.is_ok() {
        let source = shared.source.clone();
        result = tokio::task::spawn_blocking(move || {
            validate_sources(&source)?;
            source.hashes.finish()?;
            Ok(())
        })
        .await
        .map_err(|_| UploadError::Internal)?;
    }
    control.progress.send_replace(UploadProgress {
        phase: match &result {
            Ok(()) => UploadPhase::Complete,
            Err(UploadError::Cancelled | UploadError::Protocol(ProtocolError::Cancelled)) => {
                UploadPhase::Cancelled
            }
            Err(_) => UploadPhase::Failed,
        },
        bytes_confirmed: shared.confirmed(),
        bytes_total: total,
        bytes_per_second: rate,
        active_packs: 0,
    });
    result
}

/// Metadata recheck also covers empty files and files uploaded by an earlier
/// worker. SourceReader must check its original scan even for a zero-byte read.
pub(super) fn validate_sources(source: &PackSource) -> Result<(), UploadError> {
    for (index, file) in source.plan.files().iter().enumerate() {
        source
            .reader
            .read_exact_at(index as u32, file, 0, &mut [])?;
    }
    Ok(())
}

async fn retry(shared: &Shared, failures: &mut u32) -> Result<(), UploadError> {
    *failures += 1;
    if *failures > shared.options.max_retries {
        return Err(UploadError::RetryLimit);
    }
    shared.congestion.fetch_add(1, Ordering::Relaxed);
    let ceiling = shared
        .options
        .retry_base
        .saturating_mul(1u32 << (*failures - 1).min(16))
        .min(Duration::from_secs(30));
    let mut entropy = [0u8; 8];
    getrandom::fill(&mut entropy).map_err(|_| UploadError::Internal)?;
    let fraction = u64::from_le_bytes(entropy) as f64 / u64::MAX as f64;
    tokio::select! {
        () = shared.cancel.cancelled() => Err(UploadError::Cancelled),
        () = tokio::time::sleep(ceiling.mul_f64(fraction)) => Ok(()),
    }
}

async fn new_session<A: UploadApi>(
    shared: &Shared,
    api: &A,
    version_id: Uuid,
    pack: u32,
    failures: &mut u32,
) -> Result<String, UploadError> {
    loop {
        let target = tokio::select! {
            () = shared.cancel.cancelled() => return Err(UploadError::Cancelled),
            result = tokio::time::timeout(shared.options.request_timeout, api.pack_upload_target(version_id, pack)) => result,
        };
        let target = match target {
            Ok(Ok(target)) => target,
            Ok(Err(error)) if !error.retryable => {
                return Err(UploadError::Remote { retryable: false });
            }
            _ => {
                retry(shared, failures).await?;
                continue;
            }
        };
        match protocol::start(
            &shared.client,
            &target,
            shared.options.request_timeout,
            &shared.cancel,
        )
        .await
        {
            Ok(session) => {
                shared.checkpoint(pack, &session, 0).await?;
                return Ok(session);
            }
            Err(error)
                if error.retryable()
                    || matches!(error, ProtocolError::Rejected { status: 403 }) =>
            {
                retry(shared, failures).await?
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn probe(
    shared: &Shared,
    session: &str,
    total: u64,
    failures: &mut u32,
) -> Result<protocol::Confirmed, UploadError> {
    loop {
        match protocol::status(
            &shared.client,
            session,
            total,
            shared.options.request_timeout,
            &shared.cancel,
        )
        .await
        {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable() => retry(shared, failures).await?,
            Err(error) => return Err(error.into()),
        }
    }
}

async fn read_piece(
    reader: PackReader,
    length: usize,
    cancel: &CancellationToken,
) -> Result<(PackReader, Bytes), UploadError> {
    let stop = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut bytes = vec![0u8; length];
        // Bound cancellation latency while regenerating a large source pack.
        for slice in bytes.chunks_mut(256 * 1024) {
            if stop.is_cancelled() {
                return Err(UploadError::Cancelled);
            }
            reader.read_exact(slice)?;
        }
        Ok((reader, Bytes::from(bytes)))
    })
    .await
    .map_err(|_| UploadError::Internal)?
}

async fn upload_pack<A: UploadApi>(
    shared: Arc<Shared>,
    api: Arc<A>,
    version_id: Uuid,
    pack: u32,
) -> Result<(), UploadError> {
    let total = shared.source.pack_size(pack).ok_or(UploadError::Internal)?;
    let saved = shared.store.get(pack)?;
    let mut failures = 0;
    let mut session = match &saved {
        Some(saved) => saved.session.clone(),
        None => new_session(&shared, api.as_ref(), version_id, pack, &mut failures).await?,
    };
    let mut confirmed = 0;
    let mut needs_probe = saved.is_some();
    let mut complete = false;
    let mut reader = None;
    let mut pending = Bytes::new();
    let mut pending_at = 0;
    let mut sent_end = total; // A prior process may have sent beyond its checkpoint.
    loop {
        if shared.cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }
        if needs_probe {
            match probe(&shared, &session, total, &mut failures).await {
                Ok(state) => {
                    if state.offset < confirmed || state.offset > sent_end {
                        return Err(ProtocolError::Invalid(
                            "server offset is outside the sent data",
                        )
                        .into());
                    }
                    confirmed = state.offset;
                    complete = state.complete;
                    shared.checkpoint(pack, &session, confirmed).await?;
                }
                Err(UploadError::Protocol(ProtocolError::Expired)) => {
                    retry(&shared, &mut failures).await?;
                    session =
                        new_session(&shared, api.as_ref(), version_id, pack, &mut failures).await?;
                    confirmed = 0;
                    reader = None;
                    pending = Bytes::new();
                }
                Err(error) => return Err(error),
            }
            needs_probe = false;
        }
        if complete {
            break;
        }
        if confirmed == total {
            retry(&shared, &mut failures).await?;
            needs_probe = true;
            continue;
        }
        if pending.is_empty() || confirmed == pending_at + pending.len() as u64 {
            let pack_reader = match reader.take() {
                Some(reader) => reader,
                None => shared.source.open_pack(pack, confirmed)?,
            };
            let count = (total - confirmed).min(PIECE_SIZE as u64) as usize;
            let (pack_reader, bytes) = read_piece(pack_reader, count, &shared.cancel).await?;
            reader = Some(pack_reader);
            pending = bytes;
            pending_at = confirmed;
        }
        let start = usize::try_from(confirmed - pending_at).map_err(|_| UploadError::Internal)?;
        if start >= pending.len() {
            return Err(UploadError::Internal);
        }
        sent_end = pending_at + pending.len() as u64;
        let previous = confirmed;
        match protocol::put(
            &shared.client,
            &session,
            confirmed,
            total,
            pending.slice(start..),
            shared.options.request_timeout,
            &shared.cancel,
        )
        .await
        {
            Ok(state) => {
                confirmed = state.offset;
                complete = state.complete;
                shared.checkpoint(pack, &session, confirmed).await?;
                if confirmed > previous {
                    failures = 0;
                } else {
                    retry(&shared, &mut failures).await?;
                    needs_probe = true;
                }
            }
            Err(ProtocolError::Expired) => {
                retry(&shared, &mut failures).await?;
                session =
                    new_session(&shared, api.as_ref(), version_id, pack, &mut failures).await?;
                confirmed = 0;
                reader = None;
                pending = Bytes::new();
            }
            Err(error) if error.retryable() => {
                retry(&shared, &mut failures).await?;
                needs_probe = true;
            }
            Err(error) => return Err(error.into()),
        }
    }
    // Opening at total regenerates skipped bytes after a process restart;
    // reading EOF finalizes the pack hash even after the last exact-size read.
    let source = shared.source.clone();
    let cancel = shared.cancel.clone();
    tokio::task::spawn_blocking(move || {
        let mut reader = match reader {
            Some(reader) => reader,
            None => source.open_pack(pack, total)?,
        };
        let mut buf = [0u8; 64 * 1024];
        loop {
            if cancel.is_cancelled() {
                return Err(UploadError::Cancelled);
            }
            if reader.read(&mut buf)? == 0 {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| UploadError::Internal)?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use time::OffsetDateTime;
    use vgames_pack::scan::{FsReader, scan};
    use vgames_pack::{PACK_SIZE, Plan};
    use vgames_proto::versions::UploadMethod;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[derive(Clone)]
    struct LocalApi {
        base: String,
        starts: Arc<AtomicUsize>,
    }

    impl UploadApi for LocalApi {
        async fn pack_upload_target(
            &self,
            _version_id: Uuid,
            _pack: u32,
        ) -> Result<UploadTarget, RemoteError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(UploadTarget {
                url: format!("{}/start", self.base),
                method: UploadMethod::Post,
                headers: [("x-goog-resumable".to_owned(), "start".to_owned())].into(),
                expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(15),
            })
        }
    }

    fn source(root: &Path) -> PackSource {
        let scan = scan(root).unwrap();
        let plan = Arc::new(Plan::new(scan.files, scan.directories).unwrap());
        let packing = Arc::new(plan.raw_packing(PACK_SIZE).unwrap());
        PackSource::new(plan, packing, Arc::new(FsReader::new(root)))
    }

    #[tokio::test]
    async fn resume_uses_server_confirmed_offset_and_rejects_changed_source() {
        let root = tempfile::tempdir().unwrap();
        let files = root.path().join("source");
        std::fs::create_dir(&files).unwrap();
        let content = vec![b'x'; PIECE_SIZE + 100];
        std::fs::write(files.join("game"), &content).unwrap();
        let server = MockServer::start().await;
        let session = format!("{}/session", server.uri());
        Mock::given(method("POST"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(201).insert_header("location", session.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        let allow_finish = Arc::new(AtomicBool::new(false));
        let finish = allow_finish.clone();
        Mock::given(method("PUT"))
            .and(path("/session"))
            .respond_with(move |request: &Request| {
                let header = request.headers.get("content-range").unwrap();
                let value = header.to_str().unwrap();
                if value == "bytes */*" {
                    return ResponseTemplate::new(308)
                        .insert_header("range", format!("bytes=0-{}", PIECE_SIZE - 1));
                }
                if value.starts_with("bytes 0-") {
                    assert_eq!(request.body.len(), PIECE_SIZE);
                    ResponseTemplate::new(308)
                        .insert_header("range", format!("bytes=0-{}", PIECE_SIZE - 1))
                } else {
                    assert_eq!(
                        value,
                        format!(
                            "bytes {}-{}/{}",
                            PIECE_SIZE,
                            PIECE_SIZE + 99,
                            PIECE_SIZE + 100
                        )
                    );
                    assert_eq!(request.body, vec![b'x'; 100]);
                    if finish.load(Ordering::SeqCst) {
                        ResponseTemplate::new(200)
                    } else {
                        ResponseTemplate::new(503).set_delay(Duration::from_secs(2))
                    }
                }
            })
            .mount(&server)
            .await;
        let api = Arc::new(LocalApi {
            base: server.uri(),
            starts: Arc::new(AtomicUsize::new(0)),
        });
        let record = root.path().join("resume.json");
        let version = Uuid::now_v7();
        let control = UploadControl::new();
        let mut progress = control.progress();
        let mut options = UploadOptions::default();
        options.client_options.use_system_proxy = false;
        let first = tokio::spawn({
            let api = api.clone();
            let record = record.clone();
            let control = control.clone();
            let source = source(&files);
            let options = options.clone();
            async move { run(source, version, api, record, &options, &control).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while progress.borrow_and_update().bytes_confirmed < PIECE_SIZE as u64 {
                progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        control.cancel.cancel();
        assert!(matches!(first.await.unwrap(), Err(UploadError::Cancelled)));
        assert_eq!(api.starts.load(Ordering::SeqCst), 1);
        allow_finish.store(true, Ordering::SeqCst);
        run(
            source(&files),
            version,
            api.clone(),
            record.clone(),
            &options,
            &UploadControl::new(),
        )
        .await
        .unwrap();
        assert_eq!(api.starts.load(Ordering::SeqCst), 1);
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().any(|request| {
            request.url.path() == "/session"
                && request
                    .headers
                    .get("content-range")
                    .is_some_and(|value| value == "bytes */*")
        }));
        // A changed source cannot take over the old upload session.
        std::fs::write(files.join("game"), vec![b'y'; PIECE_SIZE + 101]).unwrap();
        assert!(matches!(
            run(
                source(&files),
                version,
                api.clone(),
                record.clone(),
                &options,
                &UploadControl::new()
            )
            .await,
            Err(UploadError::Resume(_))
        ));
    }

    #[tokio::test]
    async fn failed_final_response_queries_status_before_resending() {
        let root = tempfile::tempdir().unwrap();
        let files = root.path().join("source");
        std::fs::create_dir(&files).unwrap();
        std::fs::write(files.join("game"), b"a completed upload").unwrap();
        let server = MockServer::start().await;
        let session = format!("{}/session", server.uri());
        Mock::given(method("POST"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(201).insert_header("location", session.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        let pieces = Arc::new(AtomicUsize::new(0));
        let seen = pieces.clone();
        Mock::given(method("PUT"))
            .and(path("/session"))
            .respond_with(move |request: &Request| {
                if request
                    .headers
                    .get("content-range")
                    .is_some_and(|value| value == "bytes */*")
                {
                    return ResponseTemplate::new(200);
                }
                seen.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(503)
            })
            .mount(&server)
            .await;
        let api = Arc::new(LocalApi {
            base: server.uri(),
            starts: Arc::new(AtomicUsize::new(0)),
        });
        let mut options = UploadOptions::default();
        options.client_options.use_system_proxy = false;
        run(
            source(&files),
            Uuid::now_v7(),
            api,
            root.path().join("resume.json"),
            &options,
            &UploadControl::new(),
        )
        .await
        .unwrap();
        assert_eq!(pieces.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_file_changed_during_streaming_aborts_with_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let files = root.path().join("source");
        std::fs::create_dir(&files).unwrap();
        let file = files.join("game");
        std::fs::write(&file, vec![b'x'; PIECE_SIZE + 100]).unwrap();
        let server = MockServer::start().await;
        let session = format!("{}/session", server.uri());
        Mock::given(method("POST"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(201).insert_header("location", session.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        let changed = Arc::new(AtomicBool::new(false));
        let changed_here = changed.clone();
        Mock::given(method("PUT"))
            .and(path("/session"))
            .respond_with(move |request: &Request| {
                if request
                    .headers
                    .get("content-range")
                    .is_some_and(|value| value == "bytes */*")
                {
                    return ResponseTemplate::new(308)
                        .insert_header("range", format!("bytes=0-{}", PIECE_SIZE - 1));
                }
                if !changed_here.swap(true, Ordering::SeqCst) {
                    use std::io::Write as _;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&file)
                        .unwrap()
                        .write_all(b"!")
                        .unwrap();
                    return ResponseTemplate::new(308)
                        .insert_header("range", format!("bytes=0-{}", PIECE_SIZE - 1));
                }
                ResponseTemplate::new(200)
            })
            .mount(&server)
            .await;
        let api = Arc::new(LocalApi {
            base: server.uri(),
            starts: Arc::new(AtomicUsize::new(0)),
        });
        let mut options = UploadOptions::default();
        options.client_options.use_system_proxy = false;
        let error = run(
            source(&files),
            Uuid::now_v7(),
            api,
            root.path().join("resume.json"),
            &options,
            &UploadControl::new(),
        )
        .await
        .unwrap_err();
        assert!(changed.load(Ordering::SeqCst));
        assert!(
            format!("{error}").contains("changed while packing"),
            "{error}"
        );
    }
}
