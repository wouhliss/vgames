#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! A2-T04 acceptance: installs over a loopback storage rig with fault
//! injection (02-package-format §7).

use std::sync::Arc;
use std::time::Duration;

use vgames_pack::Compression;
use vgames_transfer::download::{DownloadControl, DownloadError, DownloadOptions, Phase, RunStats};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::install::{self, InstallError, InstallOutcome, InstallState};
use vgames_transfer::testkit::{
    Fault, FileSpec, MockApi, Rig, TestPackage, empty_dirs, random_files, read_tree,
};

const MIB: u64 = 1024 * 1024;

fn options() -> DownloadOptions {
    let mut o = DownloadOptions {
        client: ClientOptions {
            use_system_proxy: false,
            ..ClientOptions::default()
        },
        ..DownloadOptions::default()
    };
    o.timings.stall_timeout = Duration::from_millis(700);
    o.timings.backoff_base = Duration::from_millis(5);
    o.timings.backoff_max = Duration::from_millis(50);
    o.journal_interval = Duration::from_millis(100);
    o.aimd_interval = Duration::from_millis(200);
    o.progress_interval = Duration::from_millis(50);
    o
}

struct Setup {
    package: TestPackage,
    rig: Rig,
    api: Arc<MockApi>,
    target: tempfile::TempDir,
}

impl Setup {
    async fn new(package: TestPackage) -> Self {
        let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
        let api = MockApi::new(&rig, package.packs.iter().map(|p| p.len() as u64).collect());
        Self {
            package,
            rig,
            api,
            target: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> std::path::PathBuf {
        self.target.path().join("game")
    }

    async fn install(
        &self,
        options: &DownloadOptions,
    ) -> Result<(InstallOutcome, RunStats), InstallError> {
        let control = DownloadControl::new(None);
        self.install_with(options, &control).await
    }

    async fn install_with(
        &self,
        options: &DownloadOptions,
        control: &DownloadControl,
    ) -> Result<(InstallOutcome, RunStats), InstallError> {
        let report = install::install(
            &self.root(),
            Arc::new(self.package.release()),
            Arc::clone(&self.api),
            options,
            control,
        )
        .await?;
        Ok((report.outcome, report.stats))
    }

    fn assert_identical(&self) {
        let source = read_tree(self.package.source.path());
        let installed = read_tree(&self.root());
        assert_eq!(
            source.keys().collect::<Vec<_>>(),
            installed.keys().collect::<Vec<_>>()
        );
        for (path, bytes) in &source {
            assert!(installed[path] == *bytes, "{path} differs");
        }
        assert_eq!(
            empty_dirs(self.package.source.path()),
            empty_dirs(&self.root())
        );
        let record = install::read_record(&self.root()).unwrap().unwrap();
        assert_eq!(record.state, InstallState::Installed);
        assert!(!self.root().join(".vgames/journal.bin").exists());
        assert_eq!(
            std::fs::read(self.root().join(".vgames/manifest.json")).unwrap(),
            self.package.manifest
        );
    }
}

fn chunk_count(package: &TestPackage) -> u64 {
    package.release().manifest().chunks.len() as u64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installs_random_packages_byte_identical() {
    for seed in 1..=4u64 {
        let files = random_files(seed, 60, 9 * MIB);
        let compression = if seed % 2 == 0 {
            Compression::Auto
        } else {
            Compression::None
        };
        let package = TestPackage::build_with(
            &files,
            &["Game/Saved", "Empty/Deep/Folder"],
            compression,
            24 * MIB,
            &Default::default(),
        );
        let setup = Setup::new(package).await;
        let (outcome, stats) = setup.install(&options()).await.unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed(_)));
        setup.assert_identical();
        let chunks = chunk_count(&setup.package);
        assert_eq!(
            stats.chunks_verified, chunks,
            "each chunk verified exactly once"
        );
        assert_eq!(stats.chunks_written, chunks);
        assert!(stats.buffers_allocated <= 48);
        #[cfg(unix)]
        for file in setup.package.files.iter() {
            use std::os::unix::fs::PermissionsExt;
            let path = file.path.split('/').fold(setup.root(), |p, c| p.join(c));
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o100 != 0, file.executable, "{}", file.path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_and_tiny_packages_install() {
    let files = vec![
        FileSpec::random("a/empty.flag", 0, 1),
        FileSpec::random("b/one.byte", 1, 2),
    ];
    let setup = Setup::new(TestPackage::build(&files, &["c"], Compression::None)).await;
    let (outcome, _) = setup.install(&options()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    setup.assert_identical();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_every_transient_fault() {
    let files = random_files(11, 40, 12 * MIB);
    let package = TestPackage::build_with(
        &files,
        &[],
        Compression::Auto,
        32 * MIB,
        &Default::default(),
    );
    let setup = Setup::new(package).await;
    for fault in [
        Fault::DropMidBody {
            after: 3 * MIB + 17,
        },
        Fault::Status(503),
        Fault::Status(500),
        Fault::Status(429),
        Fault::Stall(Duration::from_secs(2)),
        Fault::DropMidBody { after: 10 },
    ] {
        setup.rig.inject(None, fault);
    }
    let (outcome, stats) = setup.install(&options()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    assert_eq!(setup.rig.pending_faults(), 0);
    setup.assert_identical();
    assert_eq!(
        stats.chunks_verified,
        chunk_count(&setup.package),
        "transient faults never re-verify"
    );
    assert!(setup.api.reports().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_violations_are_retried_once_on_a_fresh_connection() {
    let files = random_files(12, 30, 9 * MIB);
    for fault in [
        Fault::WrongContentRange,
        Fault::Truncated,
        Fault::IgnoreRange,
    ] {
        let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
        setup.rig.inject(None, fault);
        let (outcome, _) = setup.install(&options()).await.unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed(_)));
        assert_eq!(setup.rig.pending_faults(), 0);
        setup.assert_identical();
        assert!(setup.api.reports().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_links_are_refreshed() {
    let files: Vec<FileSpec> = (0..4)
        .map(|i| FileSpec::random(&format!("data/{i}.bin"), 8 * MIB, i))
        .collect();
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    // One chunk per range (8 ranges). After two requests, every link issued
    // so far expires: the remaining ranges get 403 and must refresh.
    setup.rig.expire_links_after(2);
    let options = DownloadOptions {
        max_range_bytes: 4 * MIB,
        ..options()
    };
    let result = setup.install(&options).await;
    let (outcome, stats) = result.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    setup.assert_identical();
    assert!(setup.api.url_calls() >= 2, "links were refreshed");
    assert_eq!(stats.chunks_verified, chunk_count(&setup.package));
    assert!(setup.api.reports().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeatedly_refused_identical_urls_have_a_retry_limit() {
    let package = TestPackage::build(
        &[FileSpec::random("data.bin", 1024, 7)],
        &[],
        Compression::None,
    );
    let setup = Setup::new(package).await;
    // The mock refreshes the expiration while returning the same URL text.
    for _ in 0..12 {
        setup.rig.inject(None, Fault::Status(403));
    }
    let options = DownloadOptions {
        initial_connections: 1,
        min_connections: 1,
        max_connections: 1,
        adaptive: false,
        ..options()
    };
    let error = tokio::time::timeout(Duration::from_secs(5), setup.install(&options))
        .await
        .expect("repeatedly refused links must terminate")
        .unwrap_err();
    assert!(matches!(
        error,
        InstallError::Download(DownloadError::LinksRefused {
            pack: 0,
            status: 403
        })
    ));
    assert_eq!(setup.rig.pack_requests(), 5);
    assert_eq!(setup.api.url_calls(), 5);
    assert_eq!(
        install::read_record(&setup.root()).unwrap().unwrap().state,
        InstallState::Installing
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_redownloads_files_missing_or_truncated_since_the_journal() {
    use vgames_transfer::download::journal::{Journal, JournalKey};

    for truncate in [false, true] {
        let package = TestPackage::build(
            &[FileSpec::random("data.bin", 1024, 7)],
            &[],
            Compression::None,
        );
        let setup = Setup::new(package).await;
        setup.install(&options()).await.unwrap();
        let release = setup.package.release();
        // Model an interrupted finalization: all chunks were durable and the
        // journal remains, but a file disappears or shrinks before resuming.
        let mut journal = Journal::load_or_new(
            &setup.root().join(".vgames/journal.bin"),
            JournalKey {
                version_id: release.manifest().version_id,
                manifest_blake3: *release.verified.digest.as_bytes(),
                chunk_count: chunk_count(&setup.package) as u32,
            },
        );
        for chunk in 0..chunk_count(&setup.package) as u32 {
            journal.mark(chunk);
        }
        journal.persist().unwrap();
        let path = setup.root().join("data.bin");
        if truncate {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(17)
                .unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }

        let (outcome, stats) = setup.install(&options()).await.unwrap();

        assert!(matches!(outcome, InstallOutcome::Installed(_)));
        assert_eq!(stats.chunks_written, chunk_count(&setup.package));
        setup.assert_identical();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flipped_byte_is_never_written_and_is_reported() {
    let files = vec![
        FileSpec::random("Game/big.bin", 10 * MIB, 5),
        FileSpec::random("Game/small.txt", 1000, 6),
    ];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    // Corrupt chunk 1 (bytes 4-8 MiB of big.bin) at every serve.
    let corrupt_at = 4 * MIB + 12345;
    setup.rig.corrupt(0, corrupt_at);
    let err = setup.install(&options()).await.unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Download(DownloadError::Integrity {
                pack: 0,
                chunk: 1,
                ..
            })
        ),
        "{err:?}"
    );
    assert!(err.is_integrity());
    let reports = setup.api.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        (reports[0].pack_index, reports[0].chunk_index),
        (0, Some(1))
    );
    // The corrupted chunk's region was never written: still preallocated zeros.
    let big = std::fs::read(setup.root().join("Game/big.bin")).unwrap();
    assert!(
        big[4 * MIB as usize..8 * MIB as usize]
            .iter()
            .all(|b| *b == 0)
    );
    // The install is not marked installed, and resuming later is possible.
    let record = install::read_record(&setup.root()).unwrap().unwrap();
    assert_eq!(record.state, InstallState::Installing);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_and_cancel() {
    let files = random_files(21, 30, 16 * MIB);
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    setup.rig.set_piece_delay(Duration::from_millis(20));
    let control = DownloadControl::new(None);
    let mut progress = control.progress();
    let pauser = {
        let control = control.clone();
        tokio::spawn(async move {
            loop {
                progress.changed().await.unwrap();
                let p = progress.borrow().clone();
                if p.phase == Phase::Downloading && p.bytes_done > 0 {
                    control.pause();
                    return p;
                }
            }
        })
    };
    let (outcome, first) = setup.install_with(&options(), &control).await.unwrap();
    let seen = pauser.await.unwrap();
    assert!(seen.bytes_total > 0);
    assert!(matches!(outcome, InstallOutcome::Paused(_)), "{outcome:?}");
    assert!(first.chunks_written > 0);
    setup.rig.set_piece_delay(Duration::ZERO);
    control.resume();
    let (outcome, second) = setup.install_with(&options(), &control).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    setup.assert_identical();
    assert_eq!(
        first.chunks_verified + second.chunks_verified,
        chunk_count(&setup.package),
        "no chunk verified twice across pause/resume"
    );

    // Cancel, then delete the partial install.
    let other = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    other.rig.set_piece_delay(Duration::from_millis(20));
    let control = DownloadControl::new(None);
    let canceller = {
        let control = control.clone();
        let mut progress = control.progress();
        tokio::spawn(async move {
            while progress.changed().await.is_ok() {
                if progress.borrow().bytes_done > 0 {
                    control.cancel();
                    return;
                }
            }
        })
    };
    let (outcome, _) = other.install_with(&options(), &control).await.unwrap();
    canceller.await.unwrap();
    assert_eq!(outcome, InstallOutcome::Cancelled);
    let leftovers =
        install::remove_install(&other.root(), other.package.release().manifest()).unwrap();
    assert!(leftovers.paths.is_empty(), "{leftovers:?}");
    assert!(!other.root().exists());
}

#[cfg(unix)]
#[test]
fn uninstall_never_descends_through_a_linked_manifest_directory() {
    let package = TestPackage::build(&[], &["linked/empty"], Compression::None);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("install");
    let outside = folder.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(outside.join("empty")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    let leftovers = install::remove_install(&root, package.release().manifest()).unwrap();
    assert!(outside.join("empty").is_dir());
    assert_eq!(leftovers.paths, vec![root.join("linked")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttle_limits_the_rate() {
    let files = vec![FileSpec::random("big.bin", 6 * MIB, 3)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let control = DownloadControl::new(Some(8 * MIB));
    let start = std::time::Instant::now();
    let (outcome, _) = setup.install_with(&options(), &control).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    // 6 MiB at 8 MiB/s with a 2 MiB burst: at least ~0.4 s.
    assert!(
        start.elapsed() >= Duration::from_millis(400),
        "{:?}",
        start.elapsed()
    );
    setup.assert_identical();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refuses_when_space_or_file_size_is_short() {
    let files = vec![FileSpec::random("a.bin", MIB, 1)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let manifest = setup.package.release().manifest().clone();
    let check = install::check_space(&setup.root(), &manifest).unwrap();
    assert_eq!(check.required, MIB + install::SPACE_MARGIN);
    // A foreign folder is never reused.
    std::fs::create_dir_all(setup.root()).unwrap();
    std::fs::write(setup.root().join("user-file.txt"), b"mine").unwrap();
    let err = setup.install(&options()).await.unwrap_err();
    assert!(matches!(err, InstallError::Conflict(_)), "{err:?}");
    assert_eq!(
        std::fs::read(setup.root().join("user-file.txt")).unwrap(),
        b"mine"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_created_before_the_signature_verifies() {
    use vgames_core::{Context, Envelope, SecretKey};
    use vgames_transfer::download::manifest::ManifestLink;
    let files = vec![FileSpec::random("a.bin", MIB, 1)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let client = vgames_transfer::http::transfer_client(&options().client).unwrap();
    let link = ManifestLink {
        url: setup.rig.manifest_url(),
        size: setup.package.manifest.len() as u64,
        blake3: vgames_core::Digest::of(&setup.package.manifest),
    };
    // Signed by an unknown key.
    let rogue = Envelope::sign(
        &SecretKey::from_seed(&[9; 32]),
        Context::Manifest,
        &setup.package.manifest,
    );
    let err = install::fetch_release(
        &client,
        &link,
        rogue,
        &setup.package.trust,
        &setup.package.expected,
        None,
        vgames_transfer::testkit::install_mode(),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, InstallError::Verify(_)), "{err:?}");
    assert!(err.is_integrity());
    assert!(!setup.root().exists());

    // A manifest that does not match its announced hash never parses.
    let wrong = ManifestLink {
        blake3: vgames_core::Digest::of(b"other"),
        ..link.clone()
    };
    let err = install::fetch_release(
        &client,
        &wrong,
        Envelope {
            payload_blake3: wrong.blake3,
            ..setup.package.envelope.clone()
        },
        &setup.package.trust,
        &setup.package.expected,
        None,
        vgames_transfer::testkit::install_mode(),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, InstallError::Manifest(_)), "{err:?}");

    // The genuine one verifies.
    let release = install::fetch_release(
        &client,
        &link,
        setup.package.envelope.clone(),
        &setup.package.trust,
        &setup.package.expected,
        None,
        vgames_transfer::testkit::install_mode(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(release.manifest_bytes, setup.package.manifest);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planted_symlink_is_refused() {
    let files = vec![FileSpec::random("Game/a.bin", 1000, 1)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let outside = setup.target.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(setup.root().join(".vgames")).unwrap();
    std::os::unix::fs::symlink(&outside, setup.root().join("Game")).unwrap();
    let err = setup.install(&options()).await;
    // The folder is "not empty" (it holds the planted link) or the link is refused.
    assert!(matches!(
        err,
        Err(InstallError::Conflict(_) | InstallError::UnsafePath(_))
    ));
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

/// A write that fails with ENOSPC pauses the download (02 §7.10) instead of
/// failing it; the chunk is fetched again on resume.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_disk_pauses_the_download() {
    use vgames_transfer::download::journal::{Journal, JournalKey};
    use vgames_transfer::download::table::ChunkTable;
    use vgames_transfer::download::{DownloadSpec, PauseReason, RunOutcome, run};
    use vgames_transfer::fsutil::Target;

    let files = vec![FileSpec::random("a.bin", 6 * MIB, 1)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let release = setup.package.release();
    let table = ChunkTable::new(release.manifest()).unwrap();
    let journal_path = setup.target.path().join("journal.bin");
    let spec = DownloadSpec {
        journal: Journal::load_or_new(
            &journal_path,
            JournalKey {
                version_id: release.manifest().version_id,
                manifest_blake3: *release.verified.digest.as_bytes(),
                chunk_count: table.len(),
            },
        ),
        table: Arc::new(table),
        // /dev/full answers every write with ENOSPC.
        targets: Arc::new(vec![Some(Target::device_for_tests("/dev/full"))]),
        wanted: None,
    };
    let control = DownloadControl::new(None);
    let report = run(spec, Arc::clone(&setup.api), &options(), &control)
        .await
        .unwrap();
    assert_eq!(report.outcome, RunOutcome::Paused(PauseReason::DiskFull));
    assert_eq!(
        report.journal.done().count(),
        0,
        "nothing is journaled as written"
    );
    assert_eq!(control.current_progress().phase, Phase::Paused);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_enough_space_is_refused_with_the_numbers() {
    let files = vec![FileSpec::random("a.bin", MIB, 1)];
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    let mut manifest = setup.package.release().manifest().clone();
    manifest.files[0].size = 1 << 60;
    match install::check_space(&setup.root(), &manifest) {
        Err(InstallError::NotEnoughSpace {
            required,
            available,
        }) => {
            assert_eq!(required, (1 << 60) + install::SPACE_MARGIN);
            assert!(available < required);
        }
        other => panic!("{other:?}"),
    }
    assert!(!setup.root().exists(), "the check creates nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_api_errors_retry_or_fail() {
    use vgames_transfer::download::RemoteError;
    let files = random_files(41, 10, 6 * MIB);
    let setup = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    {
        let mut errors = setup.api.url_errors.lock().unwrap();
        for _ in 0..3 {
            errors.push_back(RemoteError::retryable("503 from the API"));
        }
    }
    let (outcome, _) = setup.install(&options()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    setup.assert_identical();

    let other = Setup::new(TestPackage::build(&files, &[], Compression::None)).await;
    other.api.url_errors.lock().unwrap().push_back(RemoteError {
        retryable: false,
        code: Some("version_yanked".into()),
        message: "this version was withdrawn".into(),
    });
    let err = other.install(&options()).await.unwrap_err();
    assert!(
        matches!(&err, InstallError::Download(DownloadError::Remote(e)) if e.code.as_deref() == Some("version_yanked")),
        "{err:?}"
    );
}

/// The launch target first (GAME-06 inspects it before the rest is fetched):
/// only its byte ranges are requested, its bytes are verified and complete,
/// and a second call installs the rest without fetching it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn priority_files_come_first_and_alone() {
    let files = vec![
        FileSpec::random("Data/big.pak", 9 * MIB, 1),
        FileSpec::random("Game.exe", 3 * MIB, 2),
        FileSpec::random("Data/small.txt", 5_000, 3),
        FileSpec::random("readme.txt", 0, 4),
    ];
    let package = TestPackage::build_with(
        &files,
        &[],
        Compression::None,
        24 * MIB,
        &Default::default(),
    );
    let setup = Setup::new(package).await;
    let manifest = setup.package.release().manifest().clone();
    let exe = manifest
        .files
        .iter()
        .find(|f| f.path == "Game.exe")
        .unwrap();
    let mut first = options();
    first.priority_files = vec!["Game.exe".into(), "readme.txt".into(), "not/listed".into()];

    let (outcome, stats) = setup.install(&first).await.unwrap();
    let InstallOutcome::PriorityFilesReady { paths } = outcome else {
        panic!("expected the priority files, got {outcome:?}");
    };
    // The install root is canonical (`/private/var` on macOS, `\\?\` on Windows).
    let root = std::fs::canonicalize(setup.root()).unwrap();
    assert_eq!(
        paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap())
            .collect::<Vec<_>>(),
        vec![root.join("Game.exe"), root.join("readme.txt")]
    );
    assert_eq!(
        std::fs::read(&paths[0]).unwrap(),
        files[1].bytes(),
        "the launch target is complete"
    );
    assert_ne!(
        std::fs::read(setup.root().join("Data/big.pak")).unwrap(),
        files[0].bytes(),
        "nothing else was fetched"
    );
    // Only the executable's chunks were requested.
    let first_chunk = exe.chunk.unwrap();
    let chunks = exe.size.div_ceil(manifest.chunk_size) as u32;
    let wanted: std::collections::BTreeSet<(u32, u64)> = (first_chunk..first_chunk + chunks)
        .map(|c| {
            let chunk = &manifest.chunks[c as usize];
            (chunk.pack, chunk.offset)
        })
        .collect();
    for (pack, start, end) in setup.rig.ranges() {
        assert!(
            wanted
                .iter()
                .any(|(p, offset)| *p == pack && *offset >= start && *offset < end),
            "unexpected request: pack {pack} bytes {start}..{end}"
        );
    }
    assert_eq!(stats.chunks_verified, u64::from(chunks));
    let record = install::read_record(&setup.root()).unwrap().unwrap();
    assert_eq!(record.state, InstallState::Installing);

    let (outcome, stats) = setup.install(&options()).await.unwrap();
    assert!(matches!(outcome, InstallOutcome::Installed(_)));
    setup.assert_identical();
    assert_eq!(
        stats.chunks_verified,
        chunk_count(&setup.package) - u64::from(chunks),
        "the priority chunks were not fetched again"
    );
}
