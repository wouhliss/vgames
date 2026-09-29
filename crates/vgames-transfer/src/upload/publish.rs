//! Resumable publishing orchestration (02-package-format §6).
//!
//! Persist an idempotency key before [`create_version`], then persist the returned
//! version before [`run`]. Restart with that same version: `run` refreshes its
//! status and continues uploading, verification, or publishing without creating
//! another version. An interrupted mutation can have succeeded remotely; inspect
//! the current version before explicitly retrying it. No mutation is retried here.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::{Context, Envelope, SecretKey};
use vgames_pack::PackSource;
use vgames_pack::manifest::{self, Execution, VersionIdentity};
use vgames_proto::versions::{
    FinalizeRequest, SignatureContext, SignatureEnvelope, UploadTarget, Version, VersionCreate,
    VersionState,
};

use super::{UploadApi, UploadControl, UploadError, UploadOptions, protocol};
use crate::download::RemoteError;
use crate::http::transfer_client;

/// Authenticated API boundary, implemented by the CLI or launcher API client.
/// Implementations must not automatically retry mutations or log signed URLs.
pub trait PublishApi: Send + Sync + 'static {
    fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
        idempotency_key: Uuid,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    fn get_version(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> impl Future<Output = Result<UploadTarget, RemoteError>> + Send;
    fn manifest_upload_target(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<UploadTarget, RemoteError>> + Send;
    fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
    fn publish(
        &self,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Version, RemoteError>> + Send;
}

/// Inputs retained by the caller across restarts. Keys remain in memory only.
pub struct PublishRequest {
    pub version: Version,
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub version_create: VersionCreate,
    pub execution: Execution,
    pub source: PackSource,
    pub resume_path: PathBuf,
    /// Required only while the remote version is `uploading`. Moved into the
    /// signing worker and zeroized on drop by `vgames-core`.
    pub signing_key: Option<SecretKey>,
}

#[derive(Debug, Clone)]
pub struct PublishOptions {
    pub upload: UploadOptions,
    pub verification_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            upload: UploadOptions::default(),
            verification_timeout: Duration::from_secs(3600),
            poll_interval: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPhase {
    Preparing,
    Uploading,
    Signing,
    UploadingManifest,
    Finalizing,
    Verifying,
    Publishing,
    Published,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PublishProgress {
    pub phase: PublishPhase,
    pub verification: Option<f32>,
}

#[derive(Clone)]
pub struct PublishControl {
    /// Upload byte progress and cancellation, shared with the publishing flow.
    pub upload: UploadControl,
    progress: watch::Sender<PublishProgress>,
}

impl PublishControl {
    pub fn new(upload: UploadControl) -> Self {
        let (progress, _) = watch::channel(PublishProgress {
            phase: PublishPhase::Preparing,
            verification: None,
        });
        Self { upload, progress }
    }

    pub fn progress(&self) -> watch::Receiver<PublishProgress> {
        self.progress.subscribe()
    }

    fn phase(&self, phase: PublishPhase, verification: Option<f32>) {
        self.progress.send_replace(PublishProgress {
            phase,
            verification: verification.filter(|v| v.is_finite() && (0.0..=1.0).contains(v)),
        });
    }
}

/// Errors contain no remote response body, signed URL, or private key material.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("publishing cancelled")]
    Cancelled,
    #[error("publishing options contain a zero timeout or polling interval")]
    Options,
    #[error("{0} timed out; refresh the version before retrying")]
    Timeout(&'static str),
    #[error("the publishing API request failed during {operation}")]
    Remote {
        operation: &'static str,
        retryable: bool,
    },
    #[error("the version identity does not match the requested package")]
    Identity,
    #[error("cannot continue publishing from version state {0:?}")]
    State(VersionState),
    #[error("the server could not verify the uploaded package")]
    VerificationFailed,
    #[error("a publisher key is required to sign the manifest")]
    MissingKey,
    #[error("cannot build a valid manifest from the uploaded source")]
    Manifest,
    #[error("the manifest signing worker failed")]
    Worker,
    #[error("the manifest could not be uploaded")]
    ManifestUpload,
    #[error(transparent)]
    Upload(#[from] UploadError),
}

fn check_options(options: &PublishOptions) -> Result<(), PublishError> {
    if options.upload.request_timeout.is_zero()
        || options.verification_timeout.is_zero()
        || options.poll_interval.is_zero()
    {
        return Err(PublishError::Options);
    }
    Ok(())
}

async fn call<T>(
    future: impl Future<Output = Result<T, RemoteError>>,
    operation: &'static str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<T, PublishError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(PublishError::Cancelled),
        result = tokio::time::timeout(timeout, future) => result
            .map_err(|_| PublishError::Timeout(operation))?
            .map_err(|error| PublishError::Remote { operation, retryable: error.retryable }),
    }
}

fn check_request(
    version: &Version,
    server_id: Uuid,
    package_id: Uuid,
    request: &VersionCreate,
) -> Result<(), PublishError> {
    if version.id.is_nil()
        || version.server_id != server_id
        || version.package_id != package_id
        || version.platform != request.platform
        || version.version_label != request.version_label
        || version.sequence <= 0
    {
        return Err(PublishError::Identity);
    }
    Ok(())
}

fn check_identity(version: &Version, expected: &Version) -> Result<(), PublishError> {
    if version.id != expected.id
        || version.server_id != expected.server_id
        || version.package_id != expected.package_id
        || version.platform != expected.platform
        || version.sequence != expected.sequence
        || version.version_label != expected.version_label
        || version.created_at != expected.created_at
    {
        return Err(PublishError::Identity);
    }
    Ok(())
}

/// Creates a version once with a caller-persisted idempotency key. A replay may
/// return a version that already advanced; [`run`] refreshes its current state.
pub async fn create_version<A: PublishApi>(
    api: &A,
    server_id: Uuid,
    package_id: Uuid,
    request: VersionCreate,
    idempotency_key: Uuid,
    options: &PublishOptions,
    control: &PublishControl,
) -> Result<Version, PublishError> {
    check_options(options)?;
    let version = call(
        api.create_version(package_id, request.clone(), idempotency_key),
        "version creation",
        options.upload.request_timeout,
        &control.upload.cancel,
    )
    .await?;
    check_request(&version, server_id, package_id, &request)?;
    check_active(version.state)?;
    Ok(version)
}

fn check_active(state: VersionState) -> Result<(), PublishError> {
    match state {
        VersionState::Uploading
        | VersionState::Verifying
        | VersionState::Ready
        | VersionState::Published => Ok(()),
        VersionState::Failed => Err(PublishError::VerificationFailed),
        other => Err(PublishError::State(other)),
    }
}

struct PackApi<A>(Arc<A>);

impl<A: PublishApi> UploadApi for PackApi<A> {
    async fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        self.0
            .pack_upload_target(version_id, pack)
            .await
            .map_err(|error| RemoteError {
                retryable: error.retryable,
                code: None,
                message: "the pack upload target request failed".to_owned(),
            })
    }
}

/// Continues an existing version through pack upload, signing, manifest upload,
/// verification and publication. Create/finalize/publish are never retried.
pub async fn run<A: PublishApi>(
    request: PublishRequest,
    api: Arc<A>,
    options: &PublishOptions,
    control: &PublishControl,
) -> Result<Version, PublishError> {
    let result = run_inner(request, api, options, control).await;
    if let Err(error) = &result {
        control.phase(
            if matches!(error, PublishError::Cancelled) || control.upload.cancel.is_cancelled() {
                PublishPhase::Cancelled
            } else {
                PublishPhase::Failed
            },
            None,
        );
    }
    result
}

async fn run_inner<A: PublishApi>(
    mut request: PublishRequest,
    api: Arc<A>,
    options: &PublishOptions,
    control: &PublishControl,
) -> Result<Version, PublishError> {
    check_options(options)?;
    check_request(
        &request.version,
        request.server_id,
        request.package_id,
        &request.version_create,
    )?;
    let cancel = &control.upload.cancel;
    let timeout = options.upload.request_timeout;
    let expected = &request.version;
    let mut version = call(
        api.get_version(expected.id),
        "version status",
        timeout,
        cancel,
    )
    .await?;
    check_identity(&version, expected)?;
    check_active(version.state)?;
    if version.state == VersionState::Uploading {
        let key = request.signing_key.take().ok_or(PublishError::MissingKey)?;
        control.phase(PublishPhase::Uploading, None);
        super::run(
            request.source.clone(),
            version.id,
            Arc::new(PackApi(api.clone())),
            request.resume_path,
            &options.upload,
            &control.upload,
        )
        .await?;
        control.phase(PublishPhase::Signing, None);
        let identity = VersionIdentity {
            server_id: version.server_id,
            package_id: version.package_id,
            version_id: version.id,
            sequence: u64::try_from(version.sequence).map_err(|_| PublishError::Identity)?,
            version_label: version.version_label.clone(),
            platform: version.platform.as_str().to_owned(),
            created_at: version.created_at.unix_timestamp(),
        };
        let signing_cancel = cancel.clone();
        let work = tokio::task::spawn_blocking(move || {
            if signing_cancel.is_cancelled() {
                return Err(PublishError::Cancelled);
            }
            let source = request.source;
            let hashes = source.hashes.finish().map_err(|_| PublishError::Manifest)?;
            let bytes = manifest::build(
                &identity,
                &request.execution,
                &source.plan,
                &source.packing,
                &hashes,
            )
            .map_err(|_| PublishError::Manifest)?;
            vgames_core::manifest::parse_and_validate(&bytes)
                .map_err(|_| PublishError::Manifest)?;
            if signing_cancel.is_cancelled() {
                return Err(PublishError::Cancelled);
            }
            let signature = Envelope::sign(&key, Context::Manifest, &bytes);
            let finalize = FinalizeRequest {
                manifest_size: i64::try_from(bytes.len()).map_err(|_| PublishError::Manifest)?,
                manifest_blake3: signature.payload_blake3.to_hex(),
                signature: SignatureEnvelope {
                    format: vgames_core::sign::ENVELOPE_FORMAT.to_owned(),
                    alg: vgames_core::sign::ENVELOPE_ALG.to_owned(),
                    context: SignatureContext::Manifest,
                    key_id: signature.key_id.to_hex(),
                    payload_blake3: signature.payload_blake3.to_hex(),
                    signature: signature.signature.to_base64(),
                },
            };
            Ok((bytes, finalize))
        });
        // Join the bounded signing work even after cancellation so its key has
        // been dropped before returning control to the caller.
        let (bytes, finalize) = work.await.map_err(|_| PublishError::Worker)??;
        control.phase(PublishPhase::UploadingManifest, None);
        let target = call(
            api.manifest_upload_target(version.id),
            "manifest target",
            timeout,
            cancel,
        )
        .await?;
        let client = transfer_client(&options.upload.client_options)
            .map_err(|_| PublishError::ManifestUpload)?;
        protocol::put_manifest(&client, &target, bytes.into(), timeout, cancel)
            .await
            .map_err(|_| {
                if cancel.is_cancelled() {
                    PublishError::Cancelled
                } else {
                    PublishError::ManifestUpload
                }
            })?;
        control.phase(PublishPhase::Finalizing, None);
        version = call(
            api.finalize(version.id, finalize),
            "finalize",
            timeout,
            cancel,
        )
        .await?;
        check_identity(&version, expected)?;
        if !matches!(version.state, VersionState::Verifying | VersionState::Ready) {
            check_active(version.state)?;
            return Err(PublishError::State(version.state));
        }
    }
    // Resuming verification does not need an unlocked private key.
    drop(request.signing_key);
    if version.state == VersionState::Verifying {
        let poll = async {
            while version.state == VersionState::Verifying {
                control.phase(PublishPhase::Verifying, version.verify_progress);
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Err(PublishError::Cancelled),
                    () = tokio::time::sleep(options.poll_interval.max(Duration::from_millis(250))) => {},
                }
                version = call(
                    api.get_version(expected.id),
                    "version status",
                    timeout,
                    cancel,
                )
                .await?;
                check_identity(&version, expected)?;
                if !matches!(version.state, VersionState::Verifying | VersionState::Ready) {
                    check_active(version.state)?;
                    return Err(PublishError::State(version.state));
                }
            }
            Ok(())
        };
        tokio::time::timeout(options.verification_timeout, poll)
            .await
            .map_err(|_| PublishError::Timeout("verification"))??;
    }
    if version.state == VersionState::Ready {
        control.phase(PublishPhase::Publishing, None);
        version = call(api.publish(version.id), "publication", timeout, cancel).await?;
        check_identity(&version, expected)?;
        if version.state != VersionState::Published {
            return Err(PublishError::State(version.state));
        }
    }
    control.phase(PublishPhase::Published, Some(1.0));
    Ok(version)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;
    use time::OffsetDateTime;
    use vgames_pack::scan::{FsReader, scan};
    use vgames_pack::{PACK_SIZE, Plan};
    use vgames_proto::auth::UserPublic;
    use vgames_proto::packages::Platform;
    use vgames_proto::versions::UploadMethod;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn version(state: VersionState) -> Version {
        Version {
            id: Uuid::from_u128(3),
            package_id: Uuid::from_u128(2),
            server_id: Uuid::from_u128(1),
            platform: Platform::LinuxX86_64,
            sequence: 1,
            version_label: "1.0".to_owned(),
            state,
            is_current_release: None,
            failure_reason: None,
            total_size: None,
            file_count: None,
            chunk_count: None,
            pack_count: None,
            publisher_key_id: None,
            signature: None,
            verify_progress: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            created_by: UserPublic {
                id: Uuid::from_u128(4),
                username: "fixture".to_owned(),
                display_name: None,
                avatar_url: None,
            },
            finalized_at: None,
            verified_at: None,
            published_at: None,
            yanked_at: None,
        }
    }

    fn create() -> VersionCreate {
        VersionCreate {
            platform: Platform::LinuxX86_64,
            version_label: "1.0".to_owned(),
        }
    }

    struct MockApi {
        origin: String,
        replies: Mutex<VecDeque<Version>>,
        calls: Mutex<Vec<&'static str>>,
        finalize: Mutex<Option<FinalizeRequest>>,
        idempotency: Mutex<Option<Uuid>>,
    }

    impl MockApi {
        fn new(origin: String, states: &[VersionState]) -> Arc<Self> {
            Arc::new(Self {
                origin,
                replies: Mutex::new(states.iter().map(|state| version(*state)).collect()),
                calls: Mutex::new(Vec::new()),
                finalize: Mutex::new(None),
                idempotency: Mutex::new(None),
            })
        }

        fn reply(&self, operation: &'static str) -> Result<Version, RemoteError> {
            self.calls.lock().unwrap().push(operation);
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected API call"))
        }

        fn target(&self, suffix: &str, method: UploadMethod) -> UploadTarget {
            UploadTarget {
                url: format!("{}{suffix}", self.origin),
                method,
                headers: BTreeMap::new(),
                expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(15),
            }
        }
    }

    impl PublishApi for MockApi {
        async fn create_version(
            &self,
            package: Uuid,
            request: VersionCreate,
            key: Uuid,
        ) -> Result<Version, RemoteError> {
            assert_eq!(package, Uuid::from_u128(2));
            assert_eq!(request, create());
            *self.idempotency.lock().unwrap() = Some(key);
            self.reply("create")
        }

        async fn get_version(&self, id: Uuid) -> Result<Version, RemoteError> {
            assert_eq!(id, Uuid::from_u128(3));
            self.reply("get")
        }

        async fn pack_upload_target(
            &self,
            id: Uuid,
            pack: u32,
        ) -> Result<UploadTarget, RemoteError> {
            assert_eq!(id, Uuid::from_u128(3));
            assert_eq!(pack, 0);
            self.calls.lock().unwrap().push("pack");
            let mut target = self.target("/pack", UploadMethod::Post);
            target
                .headers
                .insert("x-goog-resumable".to_owned(), "start".to_owned());
            Ok(target)
        }

        async fn manifest_upload_target(&self, id: Uuid) -> Result<UploadTarget, RemoteError> {
            assert_eq!(id, Uuid::from_u128(3));
            self.calls.lock().unwrap().push("manifest");
            Ok(self.target("/manifest", UploadMethod::Put))
        }

        async fn finalize(
            &self,
            id: Uuid,
            request: FinalizeRequest,
        ) -> Result<Version, RemoteError> {
            assert_eq!(id, Uuid::from_u128(3));
            *self.finalize.lock().unwrap() = Some(request);
            self.reply("finalize")
        }

        async fn publish(&self, id: Uuid) -> Result<Version, RemoteError> {
            assert_eq!(id, Uuid::from_u128(3));
            self.reply("publish")
        }
    }

    fn request(root: &std::path::Path, state: VersionState) -> PublishRequest {
        let source_root = root.join("source");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::write(source_root.join("game"), b"pack bytes").unwrap();
        let scan = scan(&source_root).unwrap();
        let plan = Arc::new(Plan::new(scan.files, scan.directories).unwrap());
        let packing = Arc::new(plan.raw_packing(PACK_SIZE).unwrap());
        PublishRequest {
            version: version(state),
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
            version_create: create(),
            execution: Execution::default(),
            source: PackSource::new(plan, packing, Arc::new(FsReader::new(source_root))),
            resume_path: root.join("resume.json"),
            signing_key: None,
        }
    }

    fn options() -> PublishOptions {
        let mut options = PublishOptions::default();
        options.upload.client_options.use_system_proxy = false;
        options.poll_interval = Duration::from_millis(250);
        options
    }

    #[tokio::test]
    async fn creates_with_explicit_idempotency_and_checks_identity() {
        let api = MockApi::new(String::new(), &[VersionState::Uploading]);
        let control = PublishControl::new(UploadControl::default());
        let key = Uuid::now_v7();
        let created = create_version(
            api.as_ref(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            create(),
            key,
            &options(),
            &control,
        )
        .await
        .unwrap();
        assert_eq!(created.id, Uuid::from_u128(3));
        assert_eq!(*api.idempotency.lock().unwrap(), Some(key));
        assert_eq!(*api.calls.lock().unwrap(), ["create"]);
    }

    #[tokio::test]
    async fn full_flow_signs_exact_uploaded_manifest_and_publishes_once() {
        let storage = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/pack"))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("location", format!("{}/session", storage.uri())),
            )
            .expect(1)
            .mount(&storage)
            .await;
        Mock::given(method("PUT"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&storage)
            .await;
        Mock::given(method("PUT"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&storage)
            .await;
        let api = MockApi::new(
            storage.uri(),
            &[
                VersionState::Uploading,
                VersionState::Verifying,
                VersionState::Ready,
                VersionState::Published,
            ],
        );
        let root = tempfile::tempdir().unwrap();
        let mut input = request(root.path(), VersionState::Uploading);
        let key = SecretKey::generate().unwrap();
        let public = key.public_key();
        input.signing_key = Some(key);
        let control = PublishControl::new(UploadControl::default());
        let published = run(input, api.clone(), &options(), &control).await.unwrap();
        assert_eq!(published.state, VersionState::Published);
        assert_eq!(control.progress().borrow().phase, PublishPhase::Published);
        assert_eq!(
            *api.calls.lock().unwrap(),
            ["get", "pack", "manifest", "finalize", "get", "publish"]
        );
        let received = storage.received_requests().await.unwrap();
        assert_eq!(
            received
                .iter()
                .find(|r| r.url.path() == "/session")
                .unwrap()
                .body,
            b"pack bytes"
        );
        let bytes = &received
            .iter()
            .find(|r| r.url.path() == "/manifest")
            .unwrap()
            .body;
        let finalize = api.finalize.lock().unwrap().clone().unwrap();
        assert_eq!(finalize.manifest_size, bytes.len() as i64);
        assert_eq!(
            finalize.manifest_blake3,
            blake3::hash(bytes).to_hex().as_str()
        );
        let envelope = Envelope::parse(&serde_json::to_vec(&finalize.signature).unwrap()).unwrap();
        envelope.verify(&public, Context::Manifest, bytes).unwrap();
        let manifest = vgames_core::manifest::parse_and_validate(bytes).unwrap();
        assert_eq!(manifest.version_id, published.id);
    }

    #[tokio::test]
    async fn restart_verifying_needs_no_key_and_skips_upload_and_finalize() {
        let root = tempfile::tempdir().unwrap();
        let api = MockApi::new(
            String::new(),
            &[
                VersionState::Verifying,
                VersionState::Ready,
                VersionState::Published,
            ],
        );
        let control = PublishControl::new(UploadControl::default());
        run(
            request(root.path(), VersionState::Uploading),
            api.clone(),
            &options(),
            &control,
        )
        .await
        .unwrap();
        assert_eq!(*api.calls.lock().unwrap(), ["get", "get", "publish"]);
    }

    #[tokio::test]
    async fn published_restart_does_not_publish_again() {
        let root = tempfile::tempdir().unwrap();
        let api = MockApi::new(String::new(), &[VersionState::Published]);
        let control = PublishControl::new(UploadControl::default());
        run(
            request(root.path(), VersionState::Uploading),
            api.clone(),
            &options(),
            &control,
        )
        .await
        .unwrap();
        assert_eq!(*api.calls.lock().unwrap(), ["get"]);
    }

    #[tokio::test]
    async fn verification_failure_and_deadline_never_publish() {
        for deadline in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let api = MockApi::new(
                String::new(),
                &[VersionState::Verifying, VersionState::Failed],
            );
            let control = PublishControl::new(UploadControl::default());
            let mut options = options();
            if deadline {
                options.verification_timeout = Duration::from_millis(10);
            }
            let result = run(
                request(root.path(), VersionState::Verifying),
                api.clone(),
                &options,
                &control,
            )
            .await;
            assert!(if deadline {
                matches!(result, Err(PublishError::Timeout("verification")))
            } else {
                matches!(result, Err(PublishError::VerificationFailed))
            });
            assert!(!api.calls.lock().unwrap().contains(&"publish"));
        }
    }

    #[tokio::test]
    async fn status_identity_changes_stop_before_mutations() {
        let root = tempfile::tempdir().unwrap();
        let api = MockApi::new(String::new(), &[VersionState::Ready]);
        api.replies.lock().unwrap().front_mut().unwrap().sequence += 1;
        let control = PublishControl::new(UploadControl::default());
        assert!(matches!(
            run(
                request(root.path(), VersionState::Uploading),
                api.clone(),
                &options(),
                &control
            )
            .await,
            Err(PublishError::Identity)
        ));
        assert_eq!(*api.calls.lock().unwrap(), ["get"]);
    }

    #[tokio::test]
    async fn cancellation_deadlines_and_remote_errors_are_sanitized() {
        let cancel = CancellationToken::new();
        let timeout = Duration::from_millis(10);
        let result = call(
            std::future::pending::<Result<(), RemoteError>>(),
            "status",
            timeout,
            &cancel,
        )
        .await;
        assert!(matches!(result, Err(PublishError::Timeout("status"))));
        let result = call(
            std::future::ready(Err::<(), _>(RemoteError::fatal(
                "remote-sensitive-response",
            ))),
            "finalize",
            timeout,
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(!format!("{result:?} {result}").contains("remote-sensitive-response"));
        cancel.cancel();
        let result = call(
            std::future::ready(Ok::<_, RemoteError>(())),
            "status",
            timeout,
            &cancel,
        )
        .await;
        assert!(matches!(result, Err(PublishError::Cancelled)));
    }

    #[test]
    fn every_release_identity_field_is_bound() {
        let expected = version(VersionState::Uploading);
        let mut changes = vec![expected.clone(); 8];
        changes[0].id = Uuid::from_u128(99);
        changes[1].server_id = Uuid::from_u128(99);
        changes[2].package_id = Uuid::from_u128(99);
        changes[3].platform = Platform::WindowsX86_64;
        changes[4].sequence = 0;
        changes[5].version_label.push('2');
        changes[6].created_at += time::Duration::seconds(1);
        changes[7].sequence = 2;
        for changed in changes {
            assert!(matches!(
                check_identity(&changed, &expected),
                Err(PublishError::Identity)
            ));
        }
    }
}
