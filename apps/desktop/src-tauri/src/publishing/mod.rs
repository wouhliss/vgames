//! Launcher admin publishing (INS-06; 02-package-format §6). Admins and owners pick a folder, the
//! launcher packs and uploads it, signs the manifest with their publisher key and waits for the
//! server to verify it; the version then stays `ready` until they release it.
//!
//! - The key file is read and decrypted here, checked against the server's trust bundle (trusted,
//!   held by this account, valid now) before anything is uploaded, and dropped (zeroized by
//!   `vgames-core`) as soon as the manifest is signed. The passphrase is zeroized after use.
//! - A job is kept under `<data_dir>/publishing/<job id>/`: `job.json` (what to resume, never a
//!   key) and the upload resume record. After a restart a running job shows as cancelled and
//!   resumes where the server says it is; it asks for the key again only while unsigned.
//! - Built on `vgames_transfer::upload::publish` with `release: false`.

mod api;
pub mod model;
pub mod plan;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;
use vgames_core::keyfile::{KeyFile, KeyFileError, KeyKind, MAX_KEYFILE_BYTES};
use vgames_core::trust::{KeyStatus, TrustState};
use vgames_core::{SecretKey, Timestamp};
use vgames_pack::PackSource;
use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_proto::packages::AdminPackageCreate;
use vgames_proto::versions::{Version, VersionCreate, VersionState as WireState};
use vgames_transfer::upload::publish::{
    self, PublishControl, PublishError, PublishOptions, PublishPhase as WirePhase, PublishRequest,
};
use vgames_transfer::upload::{UploadControl, UploadError};
use zeroize::Zeroizing;

use self::api::AdminApi;
pub use self::model::*;
use crate::api::ApiClient;
use crate::events::{AppEvent, EventBus};
use crate::servers::{Account, Role, Servers};

/// Progress events per job per second, at most (phase changes always go out).
const EMIT_EVERY: Duration = Duration::from_millis(250);
/// How long `publish_cancel` waits for the job to stop before answering.
const CANCEL_WAIT: Duration = Duration::from_secs(15);
const RECORD_FORMAT: u32 = 1;

/// What publishing needs from the server list (a stub in tests).
pub trait Connections: Send + Sync + 'static {
    fn client(
        &self,
        server_id: Uuid,
    ) -> impl Future<Output = Result<ApiClient, PublishCommandError>> + Send;
    /// The signed-in account (`unauthenticated` when signed out).
    fn account(
        &self,
        server_id: Uuid,
    ) -> impl Future<Output = Result<Account, PublishCommandError>> + Send;
    /// The server's current trust state: refreshed when possible, else the stored one.
    fn trust(
        &self,
        server_id: Uuid,
    ) -> impl Future<Output = Result<Option<Arc<TrustState>>, PublishCommandError>> + Send;
}

impl Connections for Servers {
    async fn client(&self, server_id: Uuid) -> Result<ApiClient, PublishCommandError> {
        Ok(self.api(server_id).await?)
    }

    async fn account(&self, server_id: Uuid) -> Result<Account, PublishCommandError> {
        self.profile(server_id)
            .await
            .map_err(|e| PublishCommandError::internal("load the server", &e))?
            .ok_or(PublishCommandError::NotFound)?
            .account
            .ok_or(PublishCommandError::Unauthenticated)
    }

    async fn trust(&self, server_id: Uuid) -> Result<Option<Arc<TrustState>>, PublishCommandError> {
        match self.refresh_trust(server_id).await {
            Ok(state) => Ok(state),
            Err(error) => {
                tracing::info!(%server_id, %error, "trust refresh failed; using the stored bundle");
                self.trust_state(server_id)
                    .await
                    .map_err(|e| PublishCommandError::internal("load the trust bundle", &e))
            }
        }
    }
}

/// `job.json`: everything needed to show and resume a job. No key, no passphrase.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    format: u32,
    job: PublishJob,
    folder: PathBuf,
    launch: Option<PublishLaunch>,
    idempotency_key: Uuid,
    /// The server's version, once created.
    version: Option<Version>,
}

struct Slot {
    record: Record,
    /// While a run is in flight.
    control: Option<PublishControl>,
    /// Becomes true when the run ended and its outcome is recorded.
    done: watch::Receiver<bool>,
    last_emit: Option<Instant>,
}

pub struct Publisher<C: Connections> {
    conn: Arc<C>,
    dir: PathBuf,
    bus: EventBus,
    options: PublishOptions,
    jobs: Mutex<HashMap<Uuid, Slot>>,
}

fn now() -> Result<Timestamp, PublishCommandError> {
    Timestamp::from_unix(time::OffsetDateTime::now_utc().unix_timestamp())
        .map_err(|e| PublishCommandError::internal("read the clock", &e))
}

/// Reads and decrypts a publisher key file (blocking: Argon2id).
fn unlock(path: &Path, passphrase: &[u8]) -> Result<SecretKey, KeyError> {
    let meta = std::fs::metadata(path).map_err(|_| KeyError::InvalidKeyFile)?;
    if !meta.is_file() || meta.len() > MAX_KEYFILE_BYTES as u64 {
        return Err(KeyError::InvalidKeyFile);
    }
    let bytes = std::fs::read(path).map_err(|_| KeyError::InvalidKeyFile)?;
    let file = KeyFile::parse(&bytes).map_err(|_| KeyError::InvalidKeyFile)?;
    if file.kind != KeyKind::Publisher {
        return Err(KeyError::InvalidKeyFile);
    }
    file.decrypt(passphrase).map_err(|e| match e {
        KeyFileError::WrongPassphrase => KeyError::WrongPassphrase,
        _ => KeyError::InvalidKeyFile,
    })
}

/// The key may sign for this account on this server now (the CLI's check, 02 §6).
fn check_trust(
    trust: Option<&TrustState>,
    key: &SecretKey,
    holder: Uuid,
    now: Timestamp,
) -> Result<(), KeyError> {
    let untrusted = |reason| KeyError::UntrustedKey { reason };
    let trust = trust.ok_or(untrusted(UntrustedReason::NoBundle))?;
    match trust.key_status(&key.public_key().key_id()) {
        KeyStatus::Revoked => Err(untrusted(UntrustedReason::Revoked)),
        KeyStatus::Unknown => Err(untrusted(UntrustedReason::Unknown)),
        KeyStatus::Trusted(p) if p.holder_user_id != holder => {
            Err(untrusted(UntrustedReason::OtherHolder))
        }
        KeyStatus::Trusted(p) if now < p.not_before || now >= p.not_after => {
            Err(untrusted(UntrustedReason::NotValidNow))
        }
        KeyStatus::Trusted(_) => Ok(()),
    }
}

fn execution(launch: Option<&PublishLaunch>) -> Execution {
    Execution {
        launch: launch.map(|l| Launch {
            default: "main".to_owned(),
            targets: vec![LaunchTarget {
                id: "main".to_owned(),
                label: "Play".to_owned(),
                executable: l.executable.clone(),
                args: l.args.clone(),
                working_dir: l.working_dir.clone(),
                env: Default::default(),
            }],
        }),
        ..Execution::default()
    }
}

fn packs_of(source: &PackSource) -> Vec<PackProgress> {
    (0..source.packing.pack_count())
        .map(|index| PackProgress {
            index,
            bytes_confirmed: 0,
            bytes_total: source.pack_size(index).unwrap_or(0),
            state: PackState::Waiting,
        })
        .collect()
}

/// Every pack is on the server in these phases.
fn uploaded(phase: PublishPhase) -> bool {
    matches!(
        phase,
        PublishPhase::Signing
            | PublishPhase::UploadingManifest
            | PublishPhase::Finalizing
            | PublishPhase::Verifying
            | PublishPhase::Ready
            | PublishPhase::Publishing
            | PublishPhase::Published
    )
}

impl PublishJob {
    fn all_uploaded(&mut self) {
        for pack in &mut self.packs {
            pack.bytes_confirmed = pack.bytes_total;
            pack.state = PackState::Done;
        }
        self.bytes_confirmed = self.bytes_total;
    }
}

fn phase_of(phase: WirePhase) -> PublishPhase {
    match phase {
        WirePhase::Preparing => PublishPhase::Preparing,
        WirePhase::Uploading => PublishPhase::Uploading,
        WirePhase::Signing => PublishPhase::Signing,
        WirePhase::UploadingManifest => PublishPhase::UploadingManifest,
        WirePhase::Finalizing => PublishPhase::Finalizing,
        WirePhase::Verifying => PublishPhase::Verifying,
        WirePhase::Ready => PublishPhase::Ready,
        WirePhase::Publishing => PublishPhase::Publishing,
        WirePhase::Published => PublishPhase::Published,
        WirePhase::Failed => PublishPhase::Failed,
        WirePhase::Cancelled => PublishPhase::Cancelled,
    }
}

/// A run's end, as the screen shows it.
fn job_error(error: &PublishError) -> PublishJobError {
    let internal = |detail: &str| PublishJobError::Internal {
        detail: detail.to_owned(),
    };
    match error {
        PublishError::VerificationFailed => PublishJobError::VerificationFailed { reason: None },
        PublishError::State(state) => PublishJobError::VersionGone {
            state: (*state).into(),
        },
        PublishError::Remote { retryable, .. } => PublishJobError::Remote {
            retryable: *retryable,
        },
        PublishError::Timeout(_) | PublishError::ManifestUpload => {
            PublishJobError::Remote { retryable: true }
        }
        PublishError::Upload(upload) => match upload {
            UploadError::Source(_) | UploadError::Resume(_) => PublishJobError::SourceChanged,
            UploadError::Remote { retryable } => PublishJobError::Remote {
                retryable: *retryable,
            },
            UploadError::Protocol(p) => PublishJobError::Remote {
                retryable: p.retryable(),
            },
            UploadError::RetryLimit => PublishJobError::Remote { retryable: true },
            UploadError::Io(_) => PublishJobError::Io {
                detail: "Cannot read or save upload data.".to_owned(),
            },
            UploadError::Cancelled | UploadError::Options | UploadError::Internal => {
                internal("The upload stopped unexpectedly. See the log for details.")
            }
        },
        PublishError::Manifest => internal("Cannot build the manifest from the folder."),
        PublishError::Cancelled
        | PublishError::Options
        | PublishError::Identity
        | PublishError::MissingKey
        | PublishError::Worker => {
            internal("Publishing stopped unexpectedly. See the log for details.")
        }
    }
}

impl<C: Connections> Publisher<C> {
    /// Loads the jobs kept in `dir`. Jobs that were running when the launcher stopped are cancelled.
    pub fn open(conn: Arc<C>, dir: PathBuf, bus: EventBus, options: PublishOptions) -> Self {
        let mut jobs = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path().join("job.json");
                let record = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                    .filter(|r| r.format == RECORD_FORMAT);
                let Some(mut record) = record else {
                    tracing::warn!(path = %path.display(), "ignoring an unreadable publishing job");
                    continue;
                };
                if record.job.phase.is_running() {
                    record.job.phase = PublishPhase::Cancelled;
                    record.job.bytes_per_second = 0.0;
                }
                let (_, done) = watch::channel(true);
                jobs.insert(
                    record.job.id,
                    Slot {
                        record,
                        control: None,
                        done,
                        last_emit: None,
                    },
                );
            }
        }
        Self {
            conn,
            dir,
            bus,
            options: PublishOptions {
                release: false,
                ..options
            },
            jobs: Mutex::new(jobs),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Slot>> {
        self.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn job_dir(&self, id: Uuid) -> PathBuf {
        self.dir.join(id.to_string())
    }

    fn save(&self, record: &Record) {
        let dir = self.job_dir(record.job.id);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let tmp = dir.join("job.json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_vec(record).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(&tmp, dir.join("job.json"))
        };
        if let Err(error) = write() {
            tracing::warn!(job = %record.job.id, %error, "cannot save the publishing job");
        }
    }

    async fn api(&self, server_id: Uuid) -> Result<AdminApi, PublishCommandError> {
        Ok(AdminApi {
            client: self.conn.client(server_id).await?,
        })
    }

    async fn admin(&self, server_id: Uuid) -> Result<Account, PublishCommandError> {
        let account = self.conn.account(server_id).await?;
        match account.role {
            Role::Admin | Role::Owner => Ok(account),
            Role::User => Err(PublishCommandError::Forbidden),
        }
    }

    // ---- packages and versions -------------------------------------------------------------

    pub async fn packages(
        &self,
        server_id: Uuid,
        query: Option<String>,
        cursor: Option<String>,
    ) -> Result<PublishPackagePage, PublishCommandError> {
        self.admin(server_id).await?;
        let page = self
            .api(server_id)
            .await?
            .packages(query.as_deref(), cursor.as_deref())
            .await?;
        Ok(PublishPackagePage {
            items: page.items.into_iter().map(PublishPackage::from).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub async fn package_create(
        &self,
        server_id: Uuid,
        create: PackageCreate,
    ) -> Result<PublishPackage, PublishCommandError> {
        self.admin(server_id).await?;
        let request = AdminPackageCreate {
            title: create.title.trim().to_owned(),
            slug: create
                .slug
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            steam_app_id: None,
            igdb_id: None,
            fetch_metadata: None,
        };
        let created = self
            .api(server_id)
            .await?
            .package_create(&request, Uuid::now_v7())
            .await
            .map_err(|error| match (error.status(), error.problem()) {
                (Some(409), _) => PublishCommandError::SlugTaken,
                (Some(400 | 422), Some(problem)) => {
                    let field = problem.errors.first();
                    PublishCommandError::InvalidField {
                        field: match field.map(|f| f.field.as_str()) {
                            Some(f) if f.contains("slug") => "slug".to_owned(),
                            _ => "title".to_owned(),
                        },
                        message: field
                            .and_then(|f| f.message.clone())
                            .or_else(|| problem.detail.clone())
                            .unwrap_or_else(|| "The server refused this value.".to_owned()),
                    }
                }
                _ => error.into(),
            })?;
        Ok(created.into())
    }

    pub async fn versions(
        &self,
        server_id: Uuid,
        package_id: Uuid,
    ) -> Result<Vec<PublishVersion>, PublishCommandError> {
        self.admin(server_id).await?;
        let page = self.api(server_id).await?.versions(package_id).await?;
        Ok(page.items.into_iter().map(PublishVersion::from).collect())
    }

    async fn version_action(
        &self,
        server_id: Uuid,
        version_id: Uuid,
        allowed: WireState,
        yank: Option<&str>,
    ) -> Result<PublishVersion, PublishCommandError> {
        self.admin(server_id).await?;
        let api = self.api(server_id).await?;
        let current = api.version(version_id).await?;
        if current.state != allowed {
            return Err(PublishCommandError::VersionConflict {
                state: current.state.into(),
            });
        }
        let result = match yank {
            Some(reason) => api.yank(version_id, reason).await,
            None => api.release(version_id).await,
        };
        let version = result.map_err(|error| match error.status() {
            Some(409) => PublishCommandError::VersionConflict {
                state: current.state.into(),
            },
            _ => error.into(),
        })?;
        if yank.is_none() {
            self.mark_published(version_id);
        }
        Ok(version.into())
    }

    pub async fn release(
        &self,
        server_id: Uuid,
        version_id: Uuid,
    ) -> Result<PublishVersion, PublishCommandError> {
        self.version_action(server_id, version_id, WireState::Ready, None)
            .await
    }

    pub async fn yank(
        &self,
        server_id: Uuid,
        version_id: Uuid,
        reason: String,
    ) -> Result<PublishVersion, PublishCommandError> {
        let reason = reason.trim();
        if !(3..=500).contains(&reason.chars().count()) {
            return Err(PublishCommandError::InvalidField {
                field: "reason".to_owned(),
                message: "Give a reason of 3 to 500 characters.".to_owned(),
            });
        }
        self.version_action(server_id, version_id, WireState::Published, Some(reason))
            .await
    }

    fn mark_published(&self, version_id: Uuid) {
        let record = {
            let mut jobs = self.lock();
            let Some(slot) = jobs
                .values_mut()
                .find(|s| s.record.job.version_id == Some(version_id))
            else {
                return;
            };
            slot.record.job.phase = PublishPhase::Published;
            slot.record.clone()
        };
        self.save(&record);
        self.emit(record.job);
    }

    // ---- planning and jobs -----------------------------------------------------------------

    pub async fn plan(&self, folder: PathBuf) -> Result<PublishPlan, PublishCommandError> {
        Ok(Self::scan(folder).await?.plan)
    }

    async fn scan(folder: PathBuf) -> Result<plan::Scanned, PublishCommandError> {
        tokio::task::spawn_blocking(move || plan::scan_folder(&folder))
            .await
            .map_err(|e| PublishCommandError::internal("scan the folder", &e))?
    }

    async fn key(
        &self,
        server_id: Uuid,
        holder: Uuid,
        key_path: String,
        passphrase: String,
    ) -> Result<SecretKey, PublishCommandError> {
        let passphrase = Zeroizing::new(passphrase);
        let key = tokio::task::spawn_blocking(move || {
            unlock(Path::new(&key_path), passphrase.as_bytes())
        })
        .await
        .map_err(|e| PublishCommandError::internal("read the key file", &e))??;
        let trust = self.conn.trust(server_id).await?;
        check_trust(trust.as_deref(), &key, holder, now()?)?;
        Ok(key)
    }

    pub async fn start(
        self: &Arc<Self>,
        server_id: Uuid,
        start: PublishStart,
    ) -> Result<PublishJob, PublishCommandError> {
        let account = self.admin(server_id).await?;
        let label = start.version_label.trim().to_owned();
        if label.is_empty() || label.chars().count() > 64 {
            return Err(PublishCommandError::InvalidLabel);
        }
        let folder = PathBuf::from(&start.folder);
        let scanned = Self::scan(folder.clone()).await?;
        let Some(source) = scanned.source else {
            return Err(PublishCommandError::InvalidPaths {
                count: scanned.plan.invalid_count,
            });
        };
        if let Some(launch) = &start.launch
            && !source
                .plan
                .files()
                .iter()
                .any(|f| f.path == launch.executable)
        {
            return Err(PublishCommandError::InvalidLaunch);
        }
        let key = self
            .key(server_id, account.user_id, start.key_path, start.passphrase)
            .await?;
        let package = self.api(server_id).await?.package(start.package_id).await?;
        let job = PublishJob {
            id: Uuid::now_v7(),
            server_id,
            package_id: start.package_id,
            package_title: package.detail.summary.title,
            platform: start.platform,
            version_label: label,
            version_id: None,
            phase: PublishPhase::Preparing,
            bytes_confirmed: 0,
            bytes_total: 0,
            bytes_per_second: 0.0,
            packs: Vec::new(),
            verification: None,
            error: None,
            resume_needs_key: true,
        };
        let record = Record {
            format: RECORD_FORMAT,
            job,
            folder,
            launch: start.launch,
            idempotency_key: Uuid::now_v7(),
            version: None,
        };
        Ok(self.launch(record, Some(key), source))
    }

    pub fn jobs(&self) -> Vec<PublishJob> {
        let mut jobs: Vec<_> = self.lock().values().map(|s| s.record.job.clone()).collect();
        jobs.sort_by_key(|j| j.id);
        jobs
    }

    pub async fn cancel(&self, id: Uuid) -> Result<PublishJob, PublishCommandError> {
        let mut done = {
            let jobs = self.lock();
            let slot = jobs.get(&id).ok_or(PublishCommandError::NotFound)?;
            let Some(control) = &slot.control else {
                return Err(PublishCommandError::JobConflict {
                    phase: slot.record.job.phase,
                });
            };
            control.upload.cancel.cancel();
            slot.done.clone()
        };
        // The run records its own end (cancelled, or what it finished first).
        let _ = tokio::time::timeout(CANCEL_WAIT, done.wait_for(|d| *d)).await;
        self.lock()
            .get(&id)
            .map(|s| s.record.job.clone())
            .ok_or(PublishCommandError::NotFound)
    }

    pub async fn resume(
        self: &Arc<Self>,
        id: Uuid,
        key: Option<PublishResume>,
    ) -> Result<PublishJob, PublishCommandError> {
        let record = {
            let jobs = self.lock();
            let slot = jobs.get(&id).ok_or(PublishCommandError::NotFound)?;
            let phase = slot.record.job.phase;
            let resumable = phase == PublishPhase::Cancelled
                || (phase == PublishPhase::Failed
                    && matches!(
                        slot.record.job.error,
                        Some(
                            PublishJobError::Remote { .. }
                                | PublishJobError::Io { .. }
                                | PublishJobError::Internal { .. }
                        )
                    ));
            if slot.control.is_some() || !resumable {
                return Err(PublishCommandError::JobConflict { phase });
            }
            slot.record.clone()
        };
        let server_id = record.job.server_id;
        let account = self.admin(server_id).await?;
        let key = if record.job.resume_needs_key {
            let key = key.ok_or(PublishCommandError::KeyRequired)?;
            Some(
                self.key(server_id, account.user_id, key.key_path, key.passphrase)
                    .await?,
            )
        } else {
            None
        };
        let scanned = Self::scan(record.folder.clone()).await?;
        let Some(source) = scanned.source else {
            return Err(PublishCommandError::InvalidPaths {
                count: scanned.plan.invalid_count,
            });
        };
        let mut record = record;
        record.job.phase = PublishPhase::Preparing;
        record.job.error = None;
        Ok(self.launch(record, key, source))
    }

    pub async fn dismiss(&self, id: Uuid) -> Result<(), PublishCommandError> {
        let record = {
            let jobs = self.lock();
            let slot = jobs.get(&id).ok_or(PublishCommandError::NotFound)?;
            if slot.control.is_some() {
                return Err(PublishCommandError::JobConflict {
                    phase: slot.record.job.phase,
                });
            }
            slot.record.clone()
        };
        // An unpublished upload would otherwise stay on the server until its owner aborts it.
        if let Some(version) = &record.version
            && matches!(
                record.job.phase,
                PublishPhase::Cancelled | PublishPhase::Failed | PublishPhase::Ready
            )
            && !matches!(
                record.job.error,
                Some(
                    PublishJobError::VersionGone { .. }
                        | PublishJobError::VerificationFailed { .. }
                )
            )
        {
            let api = self.api(record.job.server_id).await?;
            match api.abort(version.id).await {
                Ok(()) => {}
                Err(error) if matches!(error.status(), Some(404 | 409)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.lock().remove(&id);
        if let Err(error) = std::fs::remove_dir_all(self.job_dir(id))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(job = %id, %error, "cannot delete the publishing job's files");
        }
        Ok(())
    }

    // ---- running -------------------------------------------------------------------------

    fn emit(&self, job: PublishJob) {
        self.bus
            .publish(AppEvent::PublishProgress(PublishProgress(job)));
    }

    /// Updates a job's snapshot; emits on a phase change or after [`EMIT_EVERY`].
    fn update(&self, id: Uuid, change: impl FnOnce(&mut PublishJob), force: bool) {
        let job = {
            let mut jobs = self.lock();
            let Some(slot) = jobs.get_mut(&id) else {
                return;
            };
            let before = slot.record.job.phase;
            change(&mut slot.record.job);
            let due = slot.last_emit.is_none_or(|t| t.elapsed() >= EMIT_EVERY);
            if !(force || due || before != slot.record.job.phase) {
                return;
            }
            slot.last_emit = Some(Instant::now());
            slot.record.job.clone()
        };
        self.emit(job);
    }

    /// Starts a run; returns the job as it starts.
    fn launch(
        self: &Arc<Self>,
        mut record: Record,
        key: Option<SecretKey>,
        source: PackSource,
    ) -> PublishJob {
        let control = PublishControl::new(UploadControl::new());
        let (done_tx, done) = watch::channel(false);
        let id = record.job.id;
        record.job.packs = packs_of(&source);
        record.job.bytes_total = record.job.packs.iter().map(|p| p.bytes_total).sum();
        record.job.bytes_confirmed = 0;
        {
            let mut jobs = self.lock();
            jobs.insert(
                id,
                Slot {
                    record: record.clone(),
                    control: Some(control.clone()),
                    done,
                    last_emit: None,
                },
            );
        }
        self.save(&record);
        let started = record.job.clone();
        self.emit(started.clone());
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let end = this.run(record, key, source, control).await;
            let saved = {
                let mut jobs = this.lock();
                jobs.get_mut(&id).map(|slot| {
                    slot.control = None;
                    end(&mut slot.record);
                    slot.record.clone()
                })
            };
            if let Some(record) = saved {
                this.save(&record);
                this.emit(record.job);
            }
            let _ = done_tx.send(true);
        });
        started
    }

    /// One run; returns how to record its end.
    async fn run(
        self: &Arc<Self>,
        record: Record,
        key: Option<SecretKey>,
        source: PackSource,
        control: PublishControl,
    ) -> Box<dyn FnOnce(&mut Record) + Send> {
        let id = record.job.id;
        let server_id = record.job.server_id;
        let fail = |error: PublishJobError| -> Box<dyn FnOnce(&mut Record) + Send> {
            Box::new(move |r: &mut Record| {
                r.job.phase = PublishPhase::Failed;
                r.job.error = Some(error);
                r.job.bytes_per_second = 0.0;
            })
        };
        let api = match self.api(server_id).await {
            Ok(api) => Arc::new(api),
            Err(error) => {
                tracing::warn!(job = %id, %error, "cannot reach the server to publish");
                return fail(PublishJobError::Remote { retryable: true });
            }
        };
        let create = VersionCreate {
            platform: record.job.platform.into(),
            version_label: record.job.version_label.clone(),
        };

        let reporter = tokio::spawn(Self::report(Arc::clone(self), id, control.clone()));
        let version = match record.version.clone() {
            Some(version) => version,
            None => {
                let created = publish::create_version(
                    api.as_ref(),
                    server_id,
                    record.job.package_id,
                    create.clone(),
                    record.idempotency_key,
                    &self.options,
                    &control,
                )
                .await;
                match created {
                    Ok(version) => {
                        let saved = {
                            let mut jobs = self.lock();
                            jobs.get_mut(&id).map(|slot| {
                                slot.record.version = Some(version.clone());
                                slot.record.job.version_id = Some(version.id);
                                slot.record.clone()
                            })
                        };
                        // Recorded before uploading: a restart resumes this version.
                        if let Some(saved) = saved {
                            self.save(&saved);
                        }
                        version
                    }
                    Err(error) => {
                        reporter.abort();
                        return self.ended(Err(error), None, &api).await;
                    }
                }
            }
        };
        let request = PublishRequest {
            version: version.clone(),
            server_id,
            package_id: record.job.package_id,
            version_create: create,
            execution: execution(record.launch.as_ref()),
            source,
            resume_path: self.job_dir(id).join("packs.json"),
            signing_key: key,
        };
        let result = publish::run(request, Arc::clone(&api), &self.options, &control).await;
        reporter.abort();
        self.ended(result, Some(version.id), &api).await
    }

    async fn ended(
        &self,
        result: Result<Version, PublishError>,
        version_id: Option<Uuid>,
        api: &AdminApi,
    ) -> Box<dyn FnOnce(&mut Record) + Send> {
        // What the server says now decides whether resuming needs the key again.
        let current = match (&result, version_id) {
            (Ok(version), _) => Some(version.clone()),
            (Err(_), Some(id)) => api.version(id).await.ok(),
            (Err(_), None) => None,
        };
        let outcome = match result {
            Ok(version) => match version.state {
                WireState::Published => Ok(PublishPhase::Published),
                _ => Ok(PublishPhase::Ready),
            },
            Err(PublishError::Cancelled | PublishError::Upload(UploadError::Cancelled)) => {
                Ok(PublishPhase::Cancelled)
            }
            Err(PublishError::VerificationFailed) => Err(PublishJobError::VerificationFailed {
                reason: current.as_ref().and_then(|v| v.failure_reason.clone()),
            }),
            Err(error) => {
                tracing::warn!(%error, "publishing stopped");
                Err(job_error(&error))
            }
        };
        Box::new(move |r: &mut Record| {
            if let Some(version) = current {
                r.job.version_id = Some(version.id);
                r.version = Some(version);
            }
            r.job.resume_needs_key = r
                .version
                .as_ref()
                .is_none_or(|v| v.state == WireState::Uploading);
            r.job.bytes_per_second = 0.0;
            r.job.verification = None;
            match outcome {
                Ok(phase) => {
                    r.job.phase = phase;
                    r.job.error = None;
                    if uploaded(phase) {
                        r.job.all_uploaded();
                    }
                }
                Err(error) => {
                    r.job.phase = PublishPhase::Failed;
                    r.job.error = Some(error);
                }
            }
        })
    }

    /// Follows the run's progress into the snapshot until aborted.
    async fn report(this: Arc<Self>, id: Uuid, control: PublishControl) {
        let mut phase = control.progress();
        let mut upload = control.upload.progress();
        loop {
            let now = *phase.borrow_and_update();
            let bytes = upload.borrow_and_update().clone();
            this.update(
                id,
                |job| {
                    job.phase = phase_of(now.phase);
                    job.verification = now.verification.map(f64::from);
                    job.bytes_per_second = if now.phase == WirePhase::Uploading {
                        bytes.bytes_per_second.max(0.0)
                    } else {
                        0.0
                    };
                    if uploaded(job.phase) {
                        // Past uploading (also when a resume found it done on the server).
                        job.all_uploaded();
                    } else if !bytes.packs.is_empty() {
                        for (pack, confirmed) in job.packs.iter_mut().zip(&bytes.packs) {
                            pack.bytes_confirmed = (*confirmed).min(pack.bytes_total);
                            pack.state = if pack.bytes_confirmed >= pack.bytes_total {
                                PackState::Done
                            } else if pack.bytes_confirmed > 0 {
                                PackState::Uploading
                            } else {
                                PackState::Waiting
                            };
                        }
                        job.bytes_confirmed = job.packs.iter().map(|p| p.bytes_confirmed).sum();
                    }
                },
                false,
            );
            tokio::select! {
                changed = phase.changed() => if changed.is_err() { return },
                changed = upload.changed() => if changed.is_err() { return },
            }
        }
    }
}
