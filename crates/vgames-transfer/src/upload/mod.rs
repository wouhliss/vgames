//! The upload engine and the publish flow (02-package-format §6), as a
//! library for the `vgames` CLI (Agent 5) and the launcher's admin mode
//! (A2-T13):
//!
//! 1. [`plan_folder`]: scan (symlinks and special files rejected), plan, and
//!    with `compression = auto` a first pass that picks each chunk's encoding.
//! 2. [`publish`]: create the version (or resume it from the resume file) →
//!    upload every pack into GCS resumable sessions, 4–16 in parallel, hashing
//!    on the way → build the manifest → **sign it locally** → upload it →
//!    finalize → wait for the server's verification → optionally publish.
//!
//! The upload aborts if a source file's size or mtime changes. An
//! interrupted upload resumes from the resume file: every session is asked
//! how many bytes it holds, and only the rest is sent.

pub mod packs;
pub mod resume;
pub mod session;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::manifest::Platform;
use vgames_core::{Context, Digest, Envelope, SecretKey};
use vgames_pack::manifest::{Execution, VersionIdentity};
use vgames_pack::scan::{self, FsReader, Rejected};
use vgames_pack::{Compression, PACK_SIZE, PackSource, Plan};
use vgames_proto::versions::{
    FinalizeRequest, SignatureEnvelope, UploadTarget, Version, VersionCreate, VersionState,
};

use crate::download::RemoteError;
use crate::http::{ClientOptions, transfer_client};
use packs::PackUploads;
use resume::{PackState, ResumeFile, ResumeState, plan_digest};
use session::SessionError;

/// The admin API calls a publish makes (implemented by the CLI's and the
/// launcher's API clients, and by test doubles).
pub trait PublishApi: Send + Sync + 'static {
    /// `POST /v1/admin/packages/{id}/versions`.
    fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    /// `GET /v1/admin/versions/{id}`.
    fn get_version(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    /// `POST /v1/admin/versions/{id}/packs/{i}/upload-session`.
    fn pack_upload_session(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> impl Future<Output = Result<UploadTarget, RemoteError>> + Send;
    /// `POST /v1/admin/versions/{id}/manifest-upload`.
    fn manifest_upload(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<UploadTarget, RemoteError>> + Send;
    /// `POST /v1/admin/versions/{id}/finalize`.
    fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    /// `POST /v1/admin/versions/{id}/publish`.
    fn publish(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    /// `DELETE /v1/admin/versions/{id}`.
    fn abort(&self, version_id: Uuid) -> impl Future<Output = Result<(), RemoteError>> + Send;
}

/// Signs manifest bytes (`vgames/manifest/v1`). The publisher key is
/// decrypted by the caller and zeroized when dropped (01-security §3.3).
pub trait ManifestSigner: Send + Sync {
    fn sign_manifest(&self, manifest: &[u8]) -> Envelope;
}

impl ManifestSigner for SecretKey {
    fn sign_manifest(&self, manifest: &[u8]) -> Envelope {
        Envelope::sign(self, Context::Manifest, manifest)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{path} changed while uploading; start the upload again")]
    SourceChanged { path: String },
    #[error("cannot read the source folder: {0}")]
    Source(String),
    #[error("{} file(s) cannot be packaged, first: {}", .0.len(), .0.first().map(|r| r.path.as_str()).unwrap_or_default())]
    InvalidTree(Vec<Rejected>),
    #[error("the package cannot be planned: {0}")]
    Plan(String),
    #[error("the server refused: {0}")]
    Remote(RemoteError),
    #[error("the storage server: {0}")]
    Session(SessionError),
    #[error("the signature does not cover this manifest")]
    Signature,
    #[error("the server could not verify the upload: {reason}")]
    VerificationFailed { reason: String },
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

impl UploadError {
    /// The API problem code, when the server refused (`publisher_key_untrusted`, …).
    pub fn remote_code(&self) -> Option<&str> {
        match self {
            Self::Remote(e) => e.code.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Bytes per `PUT` (a multiple of 256 KiB, GCS rule).
    pub piece_size: usize,
    pub initial_parallel: usize,
    pub min_parallel: usize,
    pub max_parallel: usize,
    pub request_timeout: Duration,
    pub piece_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub max_attempts: u32,
    pub aimd_interval: Duration,
    pub progress_interval: Duration,
    /// How often to ask the server about verification.
    pub poll_interval: Duration,
    pub client: ClientOptions,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            piece_size: 16 * 1024 * 1024,
            initial_parallel: 4,
            min_parallel: 4,
            max_parallel: 16,
            request_timeout: Duration::from_secs(30),
            piece_timeout: Duration::from_secs(180),
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            max_attempts: 12,
            aimd_interval: Duration::from_secs(2),
            progress_interval: Duration::from_millis(250),
            poll_interval: Duration::from_secs(2),
            client: ClientOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadPhase {
    Starting,
    Uploading,
    Signing,
    Finalizing,
    Verifying,
    Publishing,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UploadProgress {
    pub phase: UploadPhase,
    /// Stored bytes confirmed by the storage server.
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_second: u64,
    pub eta_seconds: Option<u32>,
    pub parallel_packs: u32,
    /// Server-side verification, 0..1 (phase `verifying`).
    pub verify_progress: Option<f32>,
}

/// Cancel a publish and watch its progress.
#[derive(Clone)]
pub struct UploadControl {
    cancel: CancellationToken,
    progress: Arc<watch::Sender<UploadProgress>>,
}

impl Default for UploadControl {
    fn default() -> Self {
        Self::new()
    }
}

impl UploadControl {
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            progress: Arc::new(watch::Sender::new(UploadProgress {
                phase: UploadPhase::Starting,
                bytes_done: 0,
                bytes_total: 0,
                bytes_per_second: 0,
                eta_seconds: None,
                parallel_packs: 0,
                verify_progress: None,
            })),
        }
    }

    /// Stops the upload; the resume file lets a later run continue.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn progress(&self) -> watch::Receiver<UploadProgress> {
        self.progress.subscribe()
    }

    fn update(&self, f: impl FnOnce(&mut UploadProgress)) {
        self.progress.send_modify(f);
    }
}

/// A planned folder, ready to upload.
#[derive(Clone)]
pub struct PlannedFolder {
    pub root: PathBuf,
    pub source: PackSource,
}

impl PlannedFolder {
    pub fn plan(&self) -> &Plan {
        &self.source.plan
    }

    pub fn total_bytes(&self) -> u64 {
        self.source.plan.total_bytes()
    }
}

/// Scans and plans `root` (blocking: run it off the async runtime). Invalid
/// entries (symlinks, special files, unsafe paths) refuse the whole tree and
/// are listed for the plan preview.
pub fn plan_folder(
    root: &Path,
    compression: Compression,
    threads: usize,
) -> Result<PlannedFolder, UploadError> {
    let scanned = scan::scan(root).map_err(|e| UploadError::Source(e.to_string()))?;
    if !scanned.rejected.is_empty() {
        return Err(UploadError::InvalidTree(scanned.rejected));
    }
    let plan = Arc::new(
        Plan::new(scanned.files, scanned.directories)
            .map_err(|e| UploadError::Plan(e.to_string()))?,
    );
    let reader = Arc::new(FsReader::new(root));
    let source = match compression {
        Compression::None => PackSource::new(
            Arc::clone(&plan),
            Arc::new(
                plan.raw_packing(PACK_SIZE)
                    .map_err(|e| UploadError::Plan(e.to_string()))?,
            ),
            reader,
        ),
        Compression::Auto => {
            vgames_pack::source::analyze(plan, reader, compression, PACK_SIZE, threads.max(1))
                .map_err(|e| match e {
                    vgames_pack::source::SourceError::Changed { path } => {
                        UploadError::SourceChanged { path }
                    }
                    other => UploadError::Source(other.to_string()),
                })?
        }
    };
    Ok(PlannedFolder {
        root: root.to_owned(),
        source,
    })
}

/// What to publish.
pub struct PublishRequest {
    pub package_id: Uuid,
    pub version_label: String,
    pub platform: Platform,
    pub folder: PlannedFolder,
    /// Launch targets, saves, controllers, multiplayer (from the package spec).
    pub execution: Execution,
    /// Where the resume state is kept (`None`: no resume).
    pub resume_file: Option<PathBuf>,
    /// Publish once the server verified the version.
    pub publish: bool,
}

#[derive(Debug, Clone)]
pub struct PublishReport {
    pub version: Version,
    pub manifest_blake3: Digest,
    pub manifest_size: u64,
    /// True when an earlier interrupted upload was continued.
    pub resumed: bool,
}

fn proto_platform(platform: Platform) -> Result<vgames_proto::packages::Platform, UploadError> {
    serde_json::from_value(serde_json::Value::String(platform.as_str().to_owned()))
        .map_err(|_| UploadError::Internal(format!("platform {platform} unknown to the API types")))
}

fn wire_envelope(envelope: &Envelope) -> Result<SignatureEnvelope, UploadError> {
    serde_json::from_slice(&envelope.to_bytes()).map_err(|e| UploadError::Internal(e.to_string()))
}

/// Runs the whole publish flow (02 §6 steps 2–8).
pub async fn publish<A: PublishApi>(
    request: PublishRequest,
    signer: &dyn ManifestSigner,
    api: Arc<A>,
    options: &UploadOptions,
    control: &UploadControl,
) -> Result<PublishReport, UploadError> {
    let source = request.folder.source.clone();
    let digest = plan_digest(&source.plan, &source.packing);
    let resume_file = request.resume_file.as_ref().map(ResumeFile::new);
    let total = source.packing.total_stored();
    control.update(|p| {
        p.phase = UploadPhase::Starting;
        p.bytes_total = total;
    });

    // Resume the same upload, or start a version.
    let mut resumed = false;
    let mut state = None;
    if let Some(saved) = resume_file.as_ref().and_then(ResumeFile::load)
        && saved.package_id == request.package_id
        && saved.plan_digest == digest
        && saved.platform == request.platform.as_str()
        && saved.version_label == request.version_label
    {
        match api.get_version(saved.version_id).await {
            Ok(v) if v.state == VersionState::Uploading => {
                tracing::info!(version = %saved.version_id, "resuming an interrupted upload");
                resumed = true;
                state = Some(saved);
            }
            Ok(v) => tracing::info!(
                state = v.state.as_str(),
                "the saved upload can no longer continue"
            ),
            Err(error) if error.retryable => return Err(UploadError::Remote(error)),
            Err(error) => tracing::info!(%error, "the saved upload is gone"),
        }
    }
    let state = match state {
        Some(s) => s,
        None => {
            let version = api
                .create_version(
                    request.package_id,
                    VersionCreate {
                        platform: proto_platform(request.platform)?,
                        version_label: request.version_label.clone(),
                    },
                )
                .await
                .map_err(UploadError::Remote)?;
            let state = ResumeState {
                format: resume::FORMAT.to_owned(),
                server_id: version.server_id,
                package_id: request.package_id,
                version_id: version.id,
                sequence: u64::try_from(version.sequence)
                    .map_err(|_| UploadError::Internal("negative sequence".into()))?,
                version_label: request.version_label.clone(),
                platform: request.platform.as_str().to_owned(),
                plan_digest: digest.clone(),
                packs: (0..source.packing.pack_count())
                    .map(|i| {
                        (
                            i,
                            PackState {
                                session: None,
                                confirmed: 0,
                                complete: false,
                            },
                        )
                    })
                    .collect(),
            };
            if let Some(file) = &resume_file {
                let (file, snapshot) = (file.path().to_owned(), state.clone());
                tokio::task::spawn_blocking(move || ResumeFile::new(file).save(&snapshot))
                    .await
                    .map_err(|e| UploadError::Internal(e.to_string()))?
                    .map_err(|e| {
                        UploadError::Internal(format!("cannot save the resume file: {e}"))
                    })?;
            }
            state
        }
    };
    let (version_id, server_id, sequence) = (state.version_id, state.server_id, state.sequence);

    // Packs.
    let client = transfer_client(&options.client)
        .map_err(|e| UploadError::Internal(format!("HTTP client: {e}")))?;
    let cancel = control.cancel.child_token();
    let uploads = PackUploads::new(
        Arc::clone(&api),
        source.clone(),
        state,
        resume_file.as_ref().map(|f| ResumeFile::new(f.path())),
        client.clone(),
        options.clone(),
        cancel,
    );
    control.update(|p| p.phase = UploadPhase::Uploading);
    let mut meter = Meter::new(uploads.confirmed.load(std::sync::atomic::Ordering::Relaxed));
    packs::run(Arc::clone(&uploads), |bytes, parallel| {
        let (rate, eta) = meter.sample(bytes, total);
        control.update(|p| {
            p.bytes_done = bytes;
            p.bytes_per_second = rate;
            p.eta_seconds = eta;
            p.parallel_packs = u32::try_from(parallel).unwrap_or(u32::MAX);
        });
    })
    .await?;

    // Every source file must still be what was planned (02 §4).
    let root = request.folder.root.clone();
    let planned = source.plan.files().to_vec();
    let unchanged = tokio::task::spawn_blocking(move || {
        scan::scan(&root).map(|now| {
            let mut now = now.files;
            now.sort_by(|a, b| a.path.cmp(&b.path));
            (now == planned, now)
        })
    })
    .await
    .map_err(|e| UploadError::Internal(e.to_string()))?
    .map_err(|e| UploadError::Source(e.to_string()))?;
    if !unchanged.0 {
        let changed = unchanged
            .1
            .iter()
            .zip(source.plan.files())
            .find(|(a, b)| a != b)
            .map_or_else(
                || "(files were added or removed)".to_owned(),
                |(a, _)| a.path.clone(),
            );
        return Err(UploadError::SourceChanged { path: changed });
    }

    // Manifest, signed locally.
    control.update(|p| p.phase = UploadPhase::Signing);
    let hashes = source
        .hashes
        .finish()
        .map_err(|e| UploadError::Internal(e.to_string()))?;
    let identity = VersionIdentity {
        server_id,
        package_id: request.package_id,
        version_id,
        sequence,
        version_label: request.version_label.clone(),
        platform: request.platform.as_str().to_owned(),
        created_at: time::OffsetDateTime::now_utc().unix_timestamp(),
    };
    let manifest = vgames_pack::manifest::build(
        &identity,
        &request.execution,
        &source.plan,
        &source.packing,
        &hashes,
    )
    .map_err(|e| UploadError::Internal(e.to_string()))?;
    let manifest_blake3 = Digest::of(&manifest);
    let envelope = signer.sign_manifest(&manifest);
    if envelope.payload_blake3 != manifest_blake3 || envelope.context != Context::Manifest {
        return Err(UploadError::Signature);
    }

    control.update(|p| p.phase = UploadPhase::Finalizing);
    let manifest_size = manifest.len() as u64;
    let body = bytes::Bytes::from(manifest);
    let mut attempt = 0;
    loop {
        let target = api
            .manifest_upload(version_id)
            .await
            .map_err(UploadError::Remote)?;
        match session::put_object(
            &client,
            &target.url,
            &target.headers,
            body.clone(),
            options.piece_timeout,
        )
        .await
        {
            Ok(()) => break,
            Err(e)
                if (e.is_transient() || matches!(e, SessionError::Refused { status: 403, .. }))
                    && attempt < 3 =>
            {
                attempt += 1;
                tokio::time::sleep(options.backoff_base * attempt).await;
            }
            Err(e) => return Err(UploadError::Session(e)),
        }
    }
    let finalize = FinalizeRequest {
        manifest_size: i64::try_from(manifest_size)
            .map_err(|_| UploadError::Internal("manifest too large".into()))?,
        manifest_blake3: manifest_blake3.to_hex(),
        signature: wire_envelope(&envelope)?,
    };
    let mut version = match api.finalize(version_id, finalize.clone()).await {
        Ok(v) => v,
        Err(e) if e.retryable => {
            // It may have been accepted before the connection failed.
            tokio::time::sleep(options.backoff_base).await;
            match api.get_version(version_id).await {
                Ok(v) if v.state != VersionState::Uploading => v,
                _ => api
                    .finalize(version_id, finalize)
                    .await
                    .map_err(UploadError::Remote)?,
            }
        }
        Err(e) => return Err(UploadError::Remote(e)),
    };
    if let Some(file) = &resume_file
        && let Err(error) = file.remove()
    {
        tracing::warn!(%error, "cannot delete the upload resume file");
    }

    // Server-side verification (the version.verify job).
    control.update(|p| p.phase = UploadPhase::Verifying);
    loop {
        match version.state {
            VersionState::Ready | VersionState::Published => break,
            VersionState::Failed => {
                return Err(UploadError::VerificationFailed {
                    reason: version.failure_reason.unwrap_or_else(|| "unknown".into()),
                });
            }
            VersionState::Aborted | VersionState::Yanked => {
                return Err(UploadError::VerificationFailed {
                    reason: format!("the version was {}", version.state.as_str()),
                });
            }
            VersionState::Uploading | VersionState::Verifying => {}
        }
        let progress = version.verify_progress;
        control.update(|p| p.verify_progress = progress);
        tokio::select! {
            () = control.cancel.cancelled() => return Err(UploadError::Cancelled),
            () = tokio::time::sleep(options.poll_interval) => {}
        }
        version = match api.get_version(version_id).await {
            Ok(v) => v,
            Err(e) if e.retryable => continue,
            Err(e) => return Err(UploadError::Remote(e)),
        };
    }
    control.update(|p| p.verify_progress = Some(1.0));

    if request.publish && version.state != VersionState::Published {
        control.update(|p| p.phase = UploadPhase::Publishing);
        version = api.publish(version_id).await.map_err(UploadError::Remote)?;
    }
    control.update(|p| p.phase = UploadPhase::Done);
    Ok(PublishReport {
        version,
        manifest_blake3,
        manifest_size,
        resumed,
    })
}

/// EWMA rate (5 s) and ETA.
struct Meter {
    last: u64,
    at: std::time::Instant,
    rate: f64,
}

impl Meter {
    fn new(start: u64) -> Self {
        Self {
            last: start,
            at: std::time::Instant::now(),
            rate: 0.0,
        }
    }

    fn sample(&mut self, bytes: u64, total: u64) -> (u64, Option<u32>) {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.at).as_secs_f64();
        if dt > 0.0 {
            let instant = bytes.saturating_sub(self.last) as f64 / dt;
            self.rate += (1.0 - (-dt / 5.0).exp()) * (instant - self.rate);
            self.last = bytes;
            self.at = now;
        }
        let eta = (self.rate >= 1.0).then(|| {
            (total.saturating_sub(bytes) as f64 / self.rate)
                .ceil()
                .min(f64::from(u32::MAX)) as u32
        });
        (self.rate.max(0.0) as u64, eta)
    }
}
