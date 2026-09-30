//! The real [`JobRunner`] (A2-T08): turns a queued install job into a signed,
//! verified download through `vgames-transfer`.
//!
//! Order (02 §7): release descriptor → trust state → manifest fetch and
//! signature check → (only then) the install row and folder → transfer. The
//! server, trust and pack-URL access sit behind [`Remote`] so the whole path is
//! tested against the loopback storage rig.

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;
use vgames_core::manifest::Platform;
use vgames_core::trust::TrustState;
use vgames_core::verify::{ExpectedRelease, VerifyMode};
use vgames_core::{Envelope, Timestamp};
use vgames_proto::versions::ReleaseDescriptor;
use vgames_transfer::download::{
    DownloadControl, DownloadError, DownloadOptions, PackUrlSource, PauseReason, RemoteError,
};
use vgames_transfer::install::{self, InstallError, InstallOutcome};

use crate::db::download_jobs::{Job, JobKind};
use crate::db::{Db, installs, libraries};
use crate::events::PackageRef;
use crate::queue::{JobRunner, RunResult};

/// Everything the runner needs from the server side.
pub trait Remote: Send + Sync + 'static {
    type Urls: PackUrlSource;

    /// `GET /v1/packages/{id}/releases/{platform}`.
    fn descriptor(
        &self,
        package: PackageRef,
        platform: Platform,
    ) -> impl Future<Output = Result<ReleaseDescriptor, RemoteError>> + Send;

    /// The stored trust state; `None` until a bundle was verified or while blocked.
    fn trust(&self, server_id: Uuid) -> impl Future<Output = Option<Arc<TrustState>>> + Send;

    /// Signed pack URLs for one version.
    fn pack_urls(
        &self,
        server_id: Uuid,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Arc<Self::Urls>, RemoteError>> + Send;
}

pub struct TransferRunner<R> {
    db: Db,
    remote: Arc<R>,
    http: reqwest::Client,
    options: DownloadOptions,
    platform: Option<Platform>,
}

struct Failure {
    code: &'static str,
    message: String,
}

fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

impl<R: Remote> TransferRunner<R> {
    pub fn new(
        db: Db,
        remote: Arc<R>,
        http: reqwest::Client,
        options: DownloadOptions,
        platform: Option<Platform>,
    ) -> Self {
        Self {
            db,
            remote,
            http,
            options,
            platform,
        }
    }

    async fn library_path(&self, job: &Job) -> Result<PathBuf, Failure> {
        let libraries = libraries::list(&self.db)
            .await
            .map_err(|e| fail("database", e.to_string()))?;
        let library = libraries
            .into_iter()
            .find(|l| l.root.id == job.library_id)
            .ok_or_else(|| fail("library_missing", "the install library was removed"))?;
        if !library.root.path.is_dir() {
            return Err(fail("library_offline", "the install library is offline"));
        }
        Ok(library.root.path)
    }

    async fn try_run(&self, job: &Job, control: &DownloadControl) -> Result<RunResult, Failure> {
        if job.kind != JobKind::Install {
            return Err(fail("unsupported", "updates and repairs are not wired yet"));
        }
        let platform = self
            .platform
            .ok_or_else(|| fail("unsupported_platform", "this system has no supported build"))?;
        let library = self.library_path(job).await?;

        let descriptor = self
            .remote
            .descriptor(job.package, platform)
            .await
            .map_err(|e| fail("server", e.message))?;
        if descriptor.version_id != job.version_id {
            return Err(fail(
                "release_changed",
                "a newer version was published; queue it again",
            ));
        }
        let trust = self
            .remote
            .trust(job.package.server_id)
            .await
            .ok_or_else(|| fail("untrusted", "the server's trust state is not verified"))?;
        let sequence = u64::try_from(descriptor.sequence)
            .map_err(|_| fail("server", "invalid release sequence"))?;
        let expected = ExpectedRelease {
            server_id: job.package.server_id,
            package_id: job.package.package_id,
            version_id: descriptor.version_id,
            platform,
            sequence,
        };
        let envelope = serde_json::to_vec(&descriptor.signature)
            .ok()
            .and_then(|bytes| Envelope::parse(&bytes).ok())
            .ok_or_else(|| fail("server", "the release signature is malformed"))?;
        control.set_phase(vgames_transfer::download::Phase::VerifyingManifest);
        let link =
            vgames_transfer::download::manifest::ManifestLink::try_from(&descriptor.manifest)
                .map_err(|e| fail("server", e.to_string()))?;
        let release = install::fetch_release(
            &self.http,
            &link,
            envelope,
            &trust,
            &expected,
            None,
            VerifyMode::Install {
                now: Timestamp::new(time::OffsetDateTime::now_utc()),
                allow_older: false,
            },
            self.options.timings.stall_timeout,
        )
        .await
        .map_err(describe)?;

        // Signature verified: only now may the install folder appear.
        let root = match installs::find(&self.db, job.package)
            .await
            .map_err(|e| fail("database", e.to_string()))?
        {
            Some((root, _)) => root,
            None => {
                let dir = install::free_dir_name(&library, &slug(job.package.package_id));
                installs::begin(
                    &self.db,
                    job.package,
                    job.library_id,
                    dir.clone(),
                    job.version_id,
                    i64::try_from(sequence).unwrap_or(i64::MAX),
                    platform.as_str().to_owned(),
                )
                .await
                .map_err(|e| fail("database", e.to_string()))?;
                library.join(dir)
            }
        };

        let urls = self
            .remote
            .pack_urls(job.package.server_id, job.version_id)
            .await
            .map_err(|e| fail("server", e.message))?;
        let total: u64 = release.manifest().files.iter().map(|f| f.size).sum();
        let report = install::install(&root, Arc::new(release), urls, &self.options, control)
            .await
            .map_err(describe)?;
        Ok(match report.outcome {
            InstallOutcome::Installed(_) => {
                installs::mark_installed(&self.db, job.package, total)
                    .await
                    .map_err(|e| fail("database", e.to_string()))?;
                RunResult::Installed
            }
            InstallOutcome::Paused(PauseReason::User) => RunResult::Paused(PauseReason::User),
            InstallOutcome::Paused(PauseReason::DiskFull) => {
                RunResult::Paused(PauseReason::DiskFull)
            }
            InstallOutcome::Cancelled => RunResult::Cancelled,
        })
    }
}

/// The production [`Remote`]: the launcher's API client per server.
pub struct ServersRemote(pub Arc<crate::servers::Servers>);

/// Pack URLs and integrity reports of one version through the API client.
pub struct ApiPackUrls {
    client: crate::api::ApiClient,
    version_id: Uuid,
}

fn remote_error(error: crate::api::ApiError) -> RemoteError {
    use crate::api::ApiError;
    let (retryable, code) = match &error {
        ApiError::Timeout | ApiError::Network(_) => (true, None),
        ApiError::Problem { status, code, .. } => {
            (*status == 429 || *status >= 500, Some(code.clone()))
        }
        _ => (false, None),
    };
    RemoteError {
        retryable,
        code,
        message: error.to_string(),
    }
}

impl PackUrlSource for ApiPackUrls {
    async fn pack_urls(
        &self,
        packs: &[u32],
    ) -> Result<Vec<vgames_proto::versions::PackUrl>, RemoteError> {
        let body = vgames_proto::versions::DownloadUrlsRequest {
            packs: packs.iter().copied().collect(),
        };
        let list: vgames_proto::versions::PackUrlList = self
            .client
            .authed(
                reqwest::Method::POST,
                &format!("v1/versions/{}/download-urls", self.version_id),
                Some(&body),
            )
            .await
            .map_err(remote_error)?;
        Ok(list.items)
    }

    async fn report_integrity(
        &self,
        report: vgames_proto::versions::IntegrityReport,
    ) -> Result<(), RemoteError> {
        self.client
            .authed_empty(
                reqwest::Method::POST,
                &format!("v1/versions/{}/integrity-reports", self.version_id),
                Some(&report),
            )
            .await
            .map_err(remote_error)
    }
}

impl Remote for ServersRemote {
    type Urls = ApiPackUrls;

    async fn descriptor(
        &self,
        package: PackageRef,
        platform: Platform,
    ) -> Result<ReleaseDescriptor, RemoteError> {
        let client = self
            .0
            .api(package.server_id)
            .await
            .map_err(|e| RemoteError::fatal(e.to_string()))?;
        client
            .authed::<(), _>(
                reqwest::Method::GET,
                &format!(
                    "v1/packages/{}/releases/{}",
                    package.package_id,
                    platform.as_str()
                ),
                None,
            )
            .await
            .map_err(remote_error)
    }

    async fn trust(&self, server_id: Uuid) -> Option<Arc<TrustState>> {
        self.0.trust_state(server_id).await.ok().flatten()
    }

    async fn pack_urls(
        &self,
        server_id: Uuid,
        version_id: Uuid,
    ) -> Result<Arc<ApiPackUrls>, RemoteError> {
        let client = self
            .0
            .api(server_id)
            .await
            .map_err(|e| RemoteError::fatal(e.to_string()))?;
        Ok(Arc::new(ApiPackUrls { client, version_id }))
    }
}

/// `package-xxxxxxxx`: the folder name until a catalog title cache exists.
fn slug(package_id: Uuid) -> String {
    format!("package-{}", &package_id.simple().to_string()[..8])
}

fn describe(error: InstallError) -> Failure {
    let code = match &error {
        _ if error.is_integrity() => "integrity",
        InstallError::NotEnoughSpace { .. } => "not_enough_space",
        InstallError::FileTooLarge { .. } => "file_too_large",
        InstallError::Download(DownloadError::Integrity { .. }) => "integrity",
        _ => "install_failed",
    };
    fail(code, error.to_string())
}

/// A single normal path component (never a separator, `.` or `..`).
fn safe_dir_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    )
}

impl<R: Remote> JobRunner for TransferRunner<R> {
    async fn run(&self, job: Job, control: DownloadControl) -> RunResult {
        match self.try_run(&job, &control).await {
            Ok(result) => result,
            Err(Failure { code, message }) => {
                tracing::warn!(package = %job.package.package_id, code, "install job failed");
                RunResult::Failed {
                    code: code.to_owned(),
                    message,
                }
            }
        }
    }

    /// Removes the partial install of a cancelled first-time install. A row
    /// that is already `installed` is never touched.
    async fn discard(&self, job: &Job) {
        let Ok(Some((root, dir))) = installs::find(&self.db, job.package).await else {
            return;
        };
        if !safe_dir_name(&dir) {
            return;
        }
        match installs::delete_unfinished(&self.db, job.package).await {
            Ok(Some(_)) => {
                let result =
                    tokio::task::spawn_blocking(move || install::remove_tree_no_follow(&root))
                        .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!("removing the partial install failed");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "unregistering the partial install failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::download_jobs::{self, JobOptions};
    use crate::events::{AppEvent, EventBus, InstallOutcome as Finished};
    use crate::queue::InstallQueue;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use vgames_pack::Compression;
    use vgames_proto::versions::{ManifestLink, SignatureEnvelope};
    use vgames_transfer::http::ClientOptions;
    use vgames_transfer::testkit::{FileSpec, MockApi, Rig, TestPackage, read_tree};

    struct Fixed {
        package: TestPackage,
        rig: Rig,
        api: Arc<MockApi>,
        untrusted: bool,
    }

    impl Remote for Fixed {
        type Urls = MockApi;

        async fn descriptor(
            &self,
            package: PackageRef,
            platform: Platform,
        ) -> Result<ReleaseDescriptor, RemoteError> {
            let e = &self.package.expected;
            assert_eq!((package.package_id, platform), (e.package_id, e.platform));
            let envelope: SignatureEnvelope =
                serde_json::from_slice(&self.package.envelope.to_bytes()).unwrap();
            Ok(ReleaseDescriptor {
                package_id: e.package_id,
                version_id: e.version_id,
                platform: serde_json::from_value(serde_json::json!(platform.as_str())).unwrap(),
                sequence: e.sequence as i64,
                version_label: "1.0".into(),
                total_size: self.package.total_bytes() as i64,
                pack_count: self.package.packs.len() as i32,
                manifest: ManifestLink {
                    url: self.rig.manifest_url(),
                    size: self.package.manifest.len() as i64,
                    blake3: vgames_core::Digest::of(&self.package.manifest).to_hex(),
                    expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                },
                signature: envelope,
                yanked_version_ids: vec![],
                published_at: time::OffsetDateTime::now_utc(),
            })
        }
        async fn trust(&self, _: Uuid) -> Option<Arc<TrustState>> {
            (!self.untrusted).then(|| Arc::new(self.package.trust.clone()))
        }
        async fn pack_urls(&self, _: Uuid, _: Uuid) -> Result<Arc<MockApi>, RemoteError> {
            Ok(Arc::clone(&self.api))
        }
    }

    async fn run_one(untrusted: bool, tamper: bool) -> (Finished, PathBuf, TestPackage, Db) {
        let package = TestPackage::build(
            &[
                FileSpec::random("bin/game", 300_000, 1),
                FileSpec::random("data/a.bin", 50_000, 2),
            ],
            &["empty"],
            Compression::None,
        );
        let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
        if tamper {
            rig.corrupt(0, 10);
        }
        let api = MockApi::new(&rig, package.packs.iter().map(|p| p.len() as u64).collect());
        let e = package.expected.clone();
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let lib_dir = dir.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();
        let library = libraries::add(&db, &lib_dir, "Games")
            .await
            .unwrap()
            .root
            .id;
        let id = e.server_id.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'T', zeroblob(32), 'VG1', 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let snapshot = TestPackage {
            source: tempfile::tempdir().unwrap(),
            files: vec![],
            manifest: package.manifest.clone(),
            envelope: package.envelope.clone(),
            trust: package.trust.clone(),
            expected: package.expected.clone(),
            packs: package.packs.clone(),
        };
        let remote = Arc::new(Fixed {
            package,
            rig,
            api,
            untrusted,
        });
        let mut options = DownloadOptions {
            client: ClientOptions {
                use_system_proxy: false,
                ..ClientOptions::default()
            },
            ..DownloadOptions::default()
        };
        options.timings.stall_timeout = Duration::from_secs(2);
        let http = vgames_transfer::http::transfer_client(&options.client).unwrap();
        let runner = Arc::new(TransferRunner::new(
            db.clone(),
            remote,
            http,
            options,
            Some(e.platform),
        ));
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let queue = InstallQueue::new(db.clone(), bus, runner).await.unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(Arc::clone(&queue).run(shutdown.clone()));
        let pkg = PackageRef {
            server_id: e.server_id,
            package_id: e.package_id,
        };
        queue
            .enqueue(
                pkg,
                e.version_id,
                library,
                JobKind::Install,
                JobOptions::default(),
            )
            .await
            .unwrap();
        let outcome = loop {
            if let AppEvent::InstallFinished(f) =
                tokio::time::timeout(Duration::from_secs(30), events.recv())
                    .await
                    .unwrap()
                    .unwrap()
            {
                break f.outcome;
            }
        };
        shutdown.cancel();
        // Keep the tempdir alive for the caller by leaking it into the returned path owner.
        let root = lib_dir;
        std::mem::forget(dir);
        (outcome, root, snapshot, db)
    }

    #[tokio::test]
    async fn queued_job_installs_a_verified_package_and_registers_it() {
        let (outcome, lib, _pkg, db) = run_one(false, false).await;
        assert!(matches!(outcome, Finished::Installed), "{outcome:?}");
        let tree = read_tree(
            &lib.join(
                std::fs::read_dir(&lib)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .file_name(),
            ),
        );
        assert_eq!(tree.get("bin/game").map(Vec::len), Some(300_000));
        let state: String = db
            .call(|c| Ok(c.query_row("SELECT state FROM installs", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(state, "installed");
        assert!(download_jobs::list(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn untrusted_server_fails_before_any_file_is_created() {
        let (outcome, lib, _pkg, db) = run_one(true, false).await;
        assert!(
            matches!(outcome, Finished::Failed { ref code, .. } if code == "untrusted"),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_dir(&lib)
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .is_ok_and(|e| e.file_name() != ".vgames-library.json"))
                .count(),
            0
        );
        let rows: i64 = db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM installs", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn a_flipped_byte_fails_with_an_integrity_code() {
        let (outcome, _lib, _pkg, _db) = run_one(false, true).await;
        assert!(
            matches!(outcome, Finished::Failed { ref code, .. } if code == "integrity"),
            "{outcome:?}"
        );
    }
}
