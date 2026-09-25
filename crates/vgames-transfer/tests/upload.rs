#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! A2-T05 acceptance: the publish flow against a GCS-resumable simulator and
//! a mock API that verifies like the server (02-package-format §6), and a
//! round trip of every upload through the download engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use vgames_core::SecretKey;
use vgames_core::manifest::Platform;
use vgames_core::verify::ExpectedRelease;
use vgames_pack::Compression;
use vgames_pack::manifest::Execution;
use vgames_proto::versions::VersionState;
use vgames_transfer::download::{DownloadControl, DownloadOptions};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::install::{self, InstallOutcome};
use vgames_transfer::testkit::package::{PACKAGE_ID, publisher_key};
use vgames_transfer::testkit::{
    FileSpec, MockApi, MockPublishApi, Rig, UploadFault, UploadRig, install_mode, random_files,
    read_tree, trust_state, write_tree,
};
use vgames_transfer::upload::{
    self, PlannedFolder, PublishReport, PublishRequest, UploadControl, UploadError, UploadOptions,
    UploadPhase, plan_folder,
};

const MIB: u64 = 1024 * 1024;

fn options() -> UploadOptions {
    UploadOptions {
        piece_size: MIB as usize,
        request_timeout: Duration::from_secs(5),
        piece_timeout: Duration::from_millis(800),
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(50),
        aimd_interval: Duration::from_millis(200),
        progress_interval: Duration::from_millis(50),
        poll_interval: Duration::from_millis(20),
        client: ClientOptions {
            use_system_proxy: false,
            ..ClientOptions::default()
        },
        ..UploadOptions::default()
    }
}

struct Setup {
    rig: Arc<UploadRig>,
    api: Arc<MockPublishApi>,
    source: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Setup {
    async fn new(files: &[FileSpec], dirs: &[&str]) -> Self {
        let rig = UploadRig::start().await;
        let api = MockPublishApi::new(Arc::clone(&rig));
        let source = tempfile::tempdir().unwrap();
        write_tree(source.path(), files, dirs);
        Self {
            rig,
            api,
            source,
            work: tempfile::tempdir().unwrap(),
        }
    }

    fn resume_path(&self) -> PathBuf {
        self.work.path().join("uploads/resume.json")
    }

    fn folder(&self, compression: Compression) -> PlannedFolder {
        plan_folder(self.source.path(), compression, 2).unwrap()
    }

    fn request(&self, folder: PlannedFolder) -> PublishRequest {
        PublishRequest {
            package_id: PACKAGE_ID,
            version_label: "1.0.0".into(),
            platform: Platform::LinuxX86_64,
            folder,
            execution: Execution::default(),
            resume_file: Some(self.resume_path()),
            publish: true,
        }
    }

    async fn publish(&self, compression: Compression) -> Result<PublishReport, UploadError> {
        self.publish_with(
            self.folder(compression),
            &publisher_key(),
            &options(),
            &UploadControl::new(),
        )
        .await
    }

    async fn publish_with(
        &self,
        folder: PlannedFolder,
        signer: &SecretKey,
        options: &UploadOptions,
        control: &UploadControl,
    ) -> Result<PublishReport, UploadError> {
        upload::publish(
            self.request(folder),
            signer,
            Arc::clone(&self.api),
            options,
            control,
        )
        .await
    }

    /// Installs what was uploaded with the download engine and compares it
    /// with the source folder.
    async fn assert_installs_identically(&self, report: &PublishReport) {
        let version = &report.version;
        let manifest = self
            .rig
            .object(&MockPublishApi::manifest_object(version))
            .unwrap();
        let release = install::verify_release(
            &trust_state(),
            self.api.signature(version.id).unwrap(),
            manifest.clone(),
            &ExpectedRelease {
                server_id: version.server_id,
                package_id: version.package_id,
                version_id: version.id,
                platform: Platform::LinuxX86_64,
                sequence: version.sequence as u64,
            },
            None,
            install_mode(),
        )
        .unwrap();
        let packs: Vec<Arc<Vec<u8>>> = (0..release.manifest().packs.len() as u32)
            .map(|i| {
                Arc::new(
                    self.rig
                        .object(&MockPublishApi::pack_object(version, i))
                        .unwrap(),
                )
            })
            .collect();
        let rig = Rig::start(manifest, packs.clone()).await;
        let api = MockApi::new(&rig, packs.iter().map(|p| p.len() as u64).collect());
        let root = self.work.path().join("installed");
        let download = DownloadOptions {
            client: ClientOptions {
                use_system_proxy: false,
                ..ClientOptions::default()
            },
            ..DownloadOptions::default()
        };
        let report = install::install(
            &root,
            Arc::new(release),
            api,
            &download,
            &DownloadControl::new(None),
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(read_tree(self.source.path()), read_tree(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

fn stored_total(folder: &PlannedFolder) -> u64 {
    folder.source.packing.total_stored()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publishes_and_round_trips_through_the_download_engine() {
    for (seed, compression) in [(1, Compression::None), (2, Compression::Auto)] {
        let files = random_files(seed, 40, 9 * MIB);
        let setup = Setup::new(&files, &["Game/Saved"]).await;
        let control = UploadControl::new();
        let mut progress = control.progress();
        let watcher = tokio::spawn(async move {
            let mut phases = Vec::new();
            while progress.changed().await.is_ok() {
                let phase = progress.borrow().phase;
                if phases.last() != Some(&phase) {
                    phases.push(phase);
                }
                if phase == UploadPhase::Done {
                    break;
                }
            }
            phases
        });
        let folder = setup.folder(compression);
        let total = stored_total(&folder);
        let report = setup
            .publish_with(folder, &publisher_key(), &options(), &control)
            .await
            .unwrap();
        assert_eq!(report.version.state, VersionState::Published);
        assert!(!report.resumed);
        let phases = watcher.await.unwrap();
        assert_eq!(phases.last(), Some(&UploadPhase::Done));
        assert!(
            phases.contains(&UploadPhase::Uploading) && phases.contains(&UploadPhase::Verifying)
        );
        assert_eq!(
            setup.rig.data_bytes(),
            total,
            "every stored byte sent exactly once"
        );
        assert!(
            !setup.resume_path().exists(),
            "the resume file is removed after finalize"
        );
        setup.assert_installs_identically(&report).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_storage_faults() {
    let files = random_files(3, 30, 9 * MIB);
    let setup = Setup::new(&files, &[]).await;
    for fault in [
        UploadFault::DropMidBody { after: 300_000 },
        UploadFault::Status(503),
        UploadFault::PartialCommit { keep: 256 * 1024 },
        UploadFault::Status(500),
        UploadFault::LoseSession,
        UploadFault::Stall(Duration::from_secs(2)),
        UploadFault::Status(429),
    ] {
        setup.rig.inject(fault);
    }
    let report = setup.publish(Compression::Auto).await.unwrap();
    assert_eq!(report.version.state, VersionState::Published);
    assert_eq!(setup.rig.pending_faults(), 0);
    assert!(
        setup.rig.status_queries() > 0,
        "failures ask the session where it stopped"
    );
    setup.assert_installs_identically(&report).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_at_random_points_resumes_the_same_version() {
    let files = random_files(4, 30, 12 * MIB);
    let setup = Setup::new(&files, &[]).await;
    setup.rig.set_put_delay(Duration::from_millis(15));
    let total = stored_total(&setup.folder(Compression::None));
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut version = None;
    let mut interrupted = 0;
    for _ in 0..20 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let cut = Duration::from_millis(seed % 120);
        let attempt = tokio::time::timeout(cut, setup.publish(Compression::None)).await;
        match attempt {
            // Dropping the future is what a killed process leaves behind:
            // the resume file and the sessions on the server.
            Err(_) => interrupted += 1,
            Ok(result) => {
                version = Some(result.unwrap());
                break;
            }
        }
    }
    let report = match version {
        Some(r) => r,
        None => setup.publish(Compression::None).await.unwrap(),
    };
    assert!(interrupted >= 5, "{interrupted} interruptions");
    assert!(
        report.resumed,
        "the final run continued the interrupted upload"
    );
    assert_eq!(
        setup
            .api
            .create_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one version only"
    );
    // Interrupted pieces are resent, but nothing near a full second upload.
    assert!(
        setup.rig.data_bytes() < total + total / 2,
        "{} of {total}",
        setup.rig.data_bytes()
    );
    setup.assert_installs_identically(&report).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_sessions_are_restarted() {
    let files = random_files(5, 20, 9 * MIB);
    let setup = Setup::new(&files, &[]).await;
    // After three stored pieces, every session expires.
    setup.rig.drop_sessions_after(3);
    let report = setup.publish(Compression::None).await.unwrap();
    assert!(
        setup.rig.sessions_started() > report_packs(&setup, &report),
        "new sessions were opened"
    );
    setup.assert_installs_identically(&report).await;
}

fn report_packs(setup: &Setup, report: &PublishReport) -> u64 {
    let manifest = setup
        .rig
        .object(&MockPublishApi::manifest_object(&report.version))
        .unwrap();
    vgames_core::manifest::parse_and_validate(&manifest)
        .unwrap()
        .packs
        .len() as u64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_source_file_aborts_with_its_path() {
    let files = vec![
        FileSpec::random("Game/a.bin", 6 * MIB, 1),
        FileSpec::random("Game/b.bin", 6 * MIB, 2),
    ];
    let setup = Setup::new(&files, &[]).await;
    setup.rig.set_put_delay(Duration::from_millis(40));
    let folder = setup.folder(Compression::None);
    let path = setup.source.path().join("Game/b.bin");
    let (key, opts, control) = (publisher_key(), options(), UploadControl::new());
    let (result, ()) = tokio::join!(setup.publish_with(folder, &key, &opts, &control), async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        std::fs::write(&path, b"edited while uploading").unwrap();
    });
    match result {
        Err(UploadError::SourceChanged { path }) => assert!(path.contains("b.bin"), "{path}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        setup
            .api
            .finalize_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untrusted_key_is_refused_by_the_server() {
    let files = vec![FileSpec::random("a.bin", MIB, 1)];
    let setup = Setup::new(&files, &[]).await;
    let rogue = SecretKey::from_seed(&[42; 32]);
    let err = setup
        .publish_with(
            setup.folder(Compression::None),
            &rogue,
            &options(),
            &UploadControl::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.remote_code(), Some("manifest_invalid"), "{err:?}");

    // A trusted key held by someone else is refused as well.
    let setup = Setup::new(&files, &[]).await;
    *setup.api.caller.lock().unwrap() = uuid::Uuid::from_u128(77);
    let err = setup.publish(Compression::None).await.unwrap_err();
    assert_eq!(err.remote_code(), Some("manifest_invalid"), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_keeps_the_resume_file() {
    let files = random_files(6, 20, 9 * MIB);
    let setup = Setup::new(&files, &[]).await;
    setup.rig.set_put_delay(Duration::from_millis(20));
    let control = UploadControl::new();
    let (key, opts) = (publisher_key(), options());
    let (result, ()) = tokio::join!(
        setup.publish_with(setup.folder(Compression::None), &key, &opts, &control),
        async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            control.cancel();
        }
    );
    assert!(matches!(result, Err(UploadError::Cancelled)), "{result:?}");
    assert!(setup.resume_path().exists());
    let report = setup.publish(Compression::None).await.unwrap();
    assert!(report.resumed);
    setup.assert_installs_identically(&report).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_tree_starts_a_new_version() {
    let files = random_files(7, 10, 5 * MIB);
    let setup = Setup::new(&files, &[]).await;
    setup.rig.set_put_delay(Duration::from_millis(20));
    // Interrupt the first upload once it has saved its resume state.
    tokio::select! {
        result = setup.publish(Compression::None) => panic!("finished too early: {result:?}"),
        () = async {
            while !setup.resume_path().exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } => {}
    }
    std::fs::write(setup.source.path().join("new-file.txt"), b"added").unwrap();
    setup.rig.set_put_delay(Duration::ZERO);
    let report = setup.publish(Compression::None).await.unwrap();
    assert!(!report.resumed);
    assert_eq!(
        setup
            .api
            .create_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    setup.assert_installs_identically(&report).await;
}

#[cfg(unix)]
#[test]
fn symlinks_refuse_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
    match plan_folder(dir.path(), Compression::None, 1) {
        Err(UploadError::InvalidTree(rejected)) => assert_eq!(rejected.len(), 1),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("planned a tree with a symlink"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_before_any_pack_started_ends_the_run() {
    let files = random_files(8, 5, MIB);
    let setup = Setup::new(&files, &[]).await;
    let control = UploadControl::new();
    control.cancel();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        setup.publish_with(
            setup.folder(Compression::None),
            &publisher_key(),
            &options(),
            &control,
        ),
    )
    .await
    .expect("a cancelled publish must end");
    assert!(matches!(result, Err(UploadError::Cancelled)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_package_of_empty_files_publishes() {
    let files = vec![
        FileSpec::random("a/empty.flag", 0, 1),
        FileSpec::random("b/also.empty", 0, 2),
    ];
    let setup = Setup::new(&files, &["c/folder"]).await;
    let report = tokio::time::timeout(Duration::from_secs(20), setup.publish(Compression::None))
        .await
        .expect("no hang without packs")
        .unwrap();
    assert_eq!(report.version.state, VersionState::Published);
    assert_eq!(report_packs(&setup, &report), 0);
    setup.assert_installs_identically(&report).await;
}
