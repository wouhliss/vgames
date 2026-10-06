//! INS-03 acceptance on the loopback rig (`vgames_transfer::testkit`): the
//! worker drives real signed packages through `fetch_release` → `install`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::broadcast;
use vgames_core::Digest;
use vgames_core::trust::TrustState;
use vgames_pack::Compression;
use vgames_proto::versions::{IntegrityReport, ManifestLink, PackUrl, ReleaseDescriptor};
use vgames_transfer::download::{PackUrlSource, RemoteError};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::testkit::{
    FileSpec, Identity, MockApi, Rig, TestPackage, read_tree, trust_state_with,
};

use vgames_transfer::testkit::package::SERVER_ID;

use super::hook::CheckFuture;
use super::*;
use crate::catalog::CompatBlocker;
use crate::db::download_jobs::NewInstall;

const MIB: u64 = 1024 * 1024;
const WAIT: Duration = Duration::from_secs(60);

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
    o.journal_interval = Duration::from_millis(50);
    o.progress_interval = Duration::from_millis(20);
    o
}

struct Served {
    package: TestPackage,
    rig: Rig,
    api: Arc<MockApi>,
}

/// Pack links from the rig, shared with the test.
struct RigPacks(Arc<MockApi>);

impl PackUrlSource for RigPacks {
    async fn pack_urls(&self, packs: &[u32]) -> Result<Vec<PackUrl>, RemoteError> {
        self.0.pack_urls(packs).await
    }
    async fn report_integrity(&self, report: IntegrityReport) -> Result<(), RemoteError> {
        self.0.report_integrity(report).await
    }
}

struct TestBackend {
    served: Mutex<HashMap<Uuid, Arc<Served>>>,
    trust: Mutex<Arc<TrustState>>,
    http: reqwest::Client,
}

impl TestBackend {
    fn served(&self, package_id: Uuid) -> Option<Arc<Served>> {
        self.served.lock().unwrap().get(&package_id).cloned()
    }
}

impl Backend for TestBackend {
    type Packs = RigPacks;

    async fn descriptor(
        &self,
        package: PackageRef,
        platform: vgames_proto::packages::Platform,
    ) -> Result<ReleaseDescriptor, crate::catalog::CatalogError> {
        let served = self
            .served(package.package_id)
            .ok_or(crate::catalog::CatalogError::NotFound)?;
        let p = &served.package;
        Ok(ReleaseDescriptor {
            package_id: package.package_id,
            version_id: p.expected.version_id,
            platform,
            sequence: 1,
            version_label: "1.1".into(),
            total_size: p.total_bytes() as i64,
            pack_count: p.packs.len() as i32,
            manifest: ManifestLink {
                url: served.rig.manifest_url(),
                size: p.manifest.len() as i64,
                blake3: Digest::of(&p.manifest).to_hex(),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            },
            signature: serde_json::from_slice(&p.envelope.to_bytes()).unwrap(),
            yanked_version_ids: vec![],
            published_at: time::OffsetDateTime::now_utc(),
        })
    }

    async fn trust(
        &self,
        _server_id: Uuid,
        _refresh: bool,
    ) -> Result<Option<Arc<TrustState>>, crate::catalog::CatalogError> {
        Ok(Some(Arc::clone(&self.trust.lock().unwrap())))
    }

    async fn packs(
        &self,
        package: PackageRef,
        _version_id: Uuid,
    ) -> Result<RigPacks, crate::catalog::CatalogError> {
        let served = self
            .served(package.package_id)
            .ok_or(crate::catalog::CatalogError::NotFound)?;
        Ok(RigPacks(Arc::clone(&served.api)))
    }

    fn transfer_http(&self) -> &reqwest::Client {
        &self.http
    }
}

struct Harness {
    db: Db,
    events: broadcast::Receiver<AppEvent>,
    backend: Arc<TestBackend>,
    downloads: Arc<Downloads<TestBackend>>,
    library: crate::db::libraries::Library,
    free: Arc<AtomicU64>,
    stop: CancellationToken,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let library_path = dir.path().join("Games");
        std::fs::create_dir(&library_path).unwrap();
        let library = crate::db::libraries::add(&db, &library_path, "Games")
            .await
            .unwrap();
        db.call(|conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [SERVER_ID.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let backend = Arc::new(TestBackend {
            served: Mutex::new(HashMap::new()),
            trust: Mutex::new(Arc::new(vgames_transfer::testkit::trust_state())),
            http: vgames_transfer::http::transfer_client(&ClientOptions {
                use_system_proxy: false,
                ..ClientOptions::default()
            })
            .unwrap(),
        });
        let free = Arc::new(AtomicU64::new(u64::MAX));
        let mut h = Self {
            db,
            events: EventBus::new().subscribe(),
            backend,
            downloads: Downloads::new(
                Db::open_in_memory().unwrap(),
                EventBus::new(),
                Arc::new(TestBackend {
                    served: Mutex::new(HashMap::new()),
                    trust: Mutex::new(Arc::new(vgames_transfer::testkit::trust_state())),
                    http: reqwest::Client::new(),
                }),
                options(),
                CancellationToken::new(),
            ),
            library,
            free,
            stop: CancellationToken::new(),
            _dir: dir,
        };
        h.restart();
        h
    }

    /// A new worker on the same database (a launcher restart).
    fn restart(&mut self) {
        self.stop.cancel();
        self.stop = CancellationToken::new();
        let bus = EventBus::new();
        self.events = bus.subscribe();
        let free = Arc::clone(&self.free);
        self.downloads = Downloads::with_free_space(
            self.db.clone(),
            bus,
            Arc::clone(&self.backend),
            options(),
            self.stop.clone(),
            Arc::new(move |_| free.load(Ordering::SeqCst)),
        );
        self.downloads.start();
    }

    async fn serve(&self, package: TestPackage) -> Arc<Served> {
        let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
        let api = MockApi::new(&rig, package.packs.iter().map(|p| p.len() as u64).collect());
        let served = Arc::new(Served { package, rig, api });
        self.backend
            .served
            .lock()
            .unwrap()
            .insert(served.package.expected.package_id, Arc::clone(&served));
        served
    }

    fn package_ref(served: &Served) -> PackageRef {
        PackageRef {
            server_id: SERVER_ID,
            package_id: served.package.expected.package_id,
        }
    }

    fn root(&self, served: &Served) -> PathBuf {
        self.library
            .root
            .path
            .join(served.package.expected.package_id.to_string())
    }

    async fn queue(&self, served: &Served) -> PackageRef {
        let package = Self::package_ref(served);
        jobs::begin_install(
            &self.db,
            NewInstall {
                package,
                library_id: self.library.root.id,
                dir_name: package.package_id.to_string(),
                version_id: served.package.expected.version_id,
                sequence: 1,
                platform: "linux-x86_64".into(),
                size_bytes: served.package.total_bytes() as i64,
                title: "Gilded Garden".into(),
                slug: "garden".into(),
                version_label: "1.1".into(),
                cover_asset_id: None,
            },
        )
        .await
        .unwrap();
        self.downloads.wake();
        package
    }

    async fn next_event(&mut self) -> AppEvent {
        loop {
            match tokio::time::timeout(WAIT, self.events.recv()).await {
                Ok(Ok(event)) => return event,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                other => panic!("no event: {other:?}"),
            }
        }
    }

    async fn finished(&mut self, package: PackageRef) -> InstallOutcome {
        loop {
            if let AppEvent::InstallFinished(f) = self.next_event().await
                && f.package == package
            {
                return f.outcome;
            }
        }
    }

    async fn job_state(&self, package: PackageRef) -> Option<DownloadState> {
        jobs::list(&self.db)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.package == package)
            .and_then(|j| decode_state(&j))
    }

    /// Waits until the stored job matches.
    async fn until_state(
        &mut self,
        package: PackageRef,
        matches: impl Fn(&DownloadState) -> bool,
    ) -> DownloadState {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(state) = self.job_state(package).await
                && matches(&state)
            {
                return state;
            }
            assert!(tokio::time::Instant::now() < deadline, "job never matched");
            let _ = tokio::time::timeout(Duration::from_millis(200), self.events.recv()).await;
        }
    }

    /// Waits until the running job has written some bytes.
    async fn until_progress(&mut self, package: PackageRef) {
        loop {
            if let AppEvent::InstallProgress(p) = self.next_event().await
                && p.package == package
                && p.bytes_done > 0
            {
                return;
            }
        }
    }
}

fn package(seed: u64, size: u64, package_id: Uuid) -> TestPackage {
    TestPackage::build_with(
        &[
            FileSpec::random("Game.exe", 2 * MIB, seed),
            FileSpec::random("Data/a.pak", size, seed + 1),
            FileSpec::random("Data/b.txt", 7_000, seed + 2),
        ],
        &["Saves"],
        Compression::None,
        24 * MIB,
        &Identity {
            package_id,
            ..Identity::default()
        },
    )
}

fn assert_installed(h: &Harness, served: &Served) {
    let installed = read_tree(&h.root(served));
    let source = read_tree(served.package.source.path());
    assert_eq!(
        installed.keys().collect::<Vec<_>>(),
        source.keys().collect::<Vec<_>>()
    );
    for (path, bytes) in &source {
        assert!(installed[path] == *bytes, "{path} differs");
    }
}

fn chunks(served: &Served) -> u64 {
    served.package.release().manifest().chunks.len() as u64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_install_completes_and_is_recorded() {
    let mut h = Harness::new().await;
    let served = h.serve(package(1, 9 * MIB, Uuid::now_v7())).await;
    let package = h.queue(&served).await;
    let mut progress = 0;
    let outcome = loop {
        match h.next_event().await {
            AppEvent::InstallProgress(p) if p.package == package => progress += 1,
            AppEvent::InstallFinished(f) if f.package == package => break f.outcome,
            _ => {}
        }
    };
    assert!(matches!(outcome, InstallOutcome::Installed), "{outcome:?}");
    assert!(progress > 0, "progress events");
    assert_installed(&h, &served);
    let row = db::installs::row(&h.db, package).await.unwrap().unwrap();
    assert_eq!(row.state, "installed");
    assert_eq!(
        row.version_id,
        served.package.expected.version_id.to_string()
    );
    assert!(jobs::list(&h.db).await.unwrap().is_empty());
    let queue = h.downloads.list(|_, _| None).await.unwrap();
    assert_eq!(queue.history.len(), 1);
    assert_eq!(queue.history[0].title, "Gilded Garden");
    assert!(matches!(
        queue.history[0].outcome,
        InstallOutcome::Installed
    ));
    assert_eq!(h.downloads.chunks_verified(), chunks(&served));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_disk_pauses_and_resumes_once_space_is_free() {
    let mut h = Harness::new().await;
    let served = h.serve(package(2, 20 * MIB, Uuid::now_v7())).await;
    served.rig.set_piece_delay(Duration::from_millis(20));
    let package = h.queue(&served).await;
    h.until_progress(package).await;
    // Mid-install: the drive fills up.
    h.downloads.pause(package).await.unwrap();
    h.until_state(package, |s| {
        matches!(
            s,
            DownloadState::Paused {
                reason: PauseReason::User
            }
        )
    })
    .await;
    served.rig.set_piece_delay(Duration::ZERO);
    h.free.store(1024, Ordering::SeqCst);
    h.downloads.resume(package).await.unwrap();
    let state = h
        .until_state(package, |s| {
            matches!(
                s,
                DownloadState::Paused {
                    reason: PauseReason::DiskFull { .. }
                }
            )
        })
        .await;
    let DownloadState::Paused {
        reason:
            PauseReason::DiskFull {
                required_bytes,
                available_bytes,
                library_path,
            },
    } = state
    else {
        unreachable!()
    };
    assert_eq!(available_bytes, 1024);
    // Files are preallocated, so only the head room is still needed.
    assert_eq!(required_bytes, vgames_transfer::install::SPACE_MARGIN);
    assert_eq!(PathBuf::from(library_path), h.library.root.path);
    // Still full: nothing happens.
    h.downloads.recheck().await;
    assert!(matches!(
        h.job_state(package).await,
        Some(DownloadState::Paused { .. })
    ));
    // Space freed: the next check resumes it, without re-verifying chunks.
    h.free.store(u64::MAX, Ordering::SeqCst);
    h.downloads.recheck().await;
    assert!(matches!(
        h.finished(package).await,
        InstallOutcome::Installed
    ));
    assert_installed(&h, &served);
    assert_eq!(
        h.downloads.chunks_verified(),
        chunks(&served),
        "no chunk verified twice"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_library_pauses_until_it_is_back() {
    let mut h = Harness::new().await;
    let served = h.serve(package(3, 20 * MIB, Uuid::now_v7())).await;
    served.rig.set_piece_delay(Duration::from_millis(20));
    let package = h.queue(&served).await;
    h.until_progress(package).await;
    h.downloads.pause(package).await.unwrap();
    h.until_state(package, |s| {
        matches!(
            s,
            DownloadState::Paused {
                reason: PauseReason::User
            }
        )
    })
    .await;
    served.rig.set_piece_delay(Duration::ZERO);
    let away = h.library.root.path.with_file_name("Games-unplugged");
    std::fs::rename(&h.library.root.path, &away).unwrap();
    h.downloads.resume(package).await.unwrap();
    h.until_state(package, |s| {
        matches!(
            s,
            DownloadState::Paused {
                reason: PauseReason::LibraryOffline { .. }
            }
        )
    })
    .await;
    h.downloads.recheck().await;
    assert!(matches!(
        h.job_state(package).await,
        Some(DownloadState::Paused { .. })
    ));
    std::fs::rename(&away, &h.library.root.path).unwrap();
    h.downloads.recheck().await;
    assert!(matches!(
        h.finished(package).await,
        InstallOutcome::Installed
    ));
    assert_installed(&h, &served);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_keeps_or_deletes_the_partial_install() {
    for keep in [true, false] {
        let mut h = Harness::new().await;
        let served = h.serve(package(4, 20 * MIB, Uuid::now_v7())).await;
        served.rig.set_piece_delay(Duration::from_millis(20));
        let package = h.queue(&served).await;
        h.until_progress(package).await;
        h.downloads.cancel(package, keep).await.unwrap();
        assert!(matches!(
            h.finished(package).await,
            InstallOutcome::Cancelled { kept_partial } if kept_partial == keep
        ));
        assert!(jobs::list(&h.db).await.unwrap().is_empty());
        let row = db::installs::row(&h.db, package).await.unwrap();
        if keep {
            assert_eq!(row.unwrap().state, "installing", "shows as incomplete");
            assert!(h.root(&served).join(".vgames/journal.bin").exists());
        } else {
            assert!(row.is_none());
            let left: Vec<_> = std::fs::read_dir(&h.library.root.path)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                left,
                vec![".vgames-library.json".to_owned()],
                "only the library marker"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_waiting_job_cancelled_without_keep_leaves_only_the_marker() {
    let mut h = Harness::new().await;
    let served = h.serve(package(5, 20 * MIB, Uuid::now_v7())).await;
    served.rig.set_piece_delay(Duration::from_millis(20));
    let package = h.queue(&served).await;
    h.until_progress(package).await;
    h.downloads.pause(package).await.unwrap();
    h.until_state(package, |s| matches!(s, DownloadState::Paused { .. }))
        .await;
    h.downloads.cancel(package, false).await.unwrap();
    assert!(db::installs::row(&h.db, package).await.unwrap().is_none());
    assert!(!h.root(&served).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_mid_install_resumes_from_the_journal() {
    let mut h = Harness::new().await;
    let served = h.serve(package(6, 24 * MIB, Uuid::now_v7())).await;
    served.rig.set_piece_delay(Duration::from_millis(20));
    let package = h.queue(&served).await;
    h.until_progress(package).await;
    h.downloads.pause_all_at_checkpoint().await;
    assert_eq!(h.job_state(package).await, Some(DownloadState::Queued));
    let before = h.downloads.chunks_verified();
    assert!(before > 0 && before < chunks(&served));
    // The process ends here; the next one picks the job up again.
    served.rig.set_piece_delay(Duration::ZERO);
    h.restart();
    assert!(matches!(
        h.finished(package).await,
        InstallOutcome::Installed
    ));
    assert_installed(&h, &served);
    assert_eq!(
        before + h.downloads.chunks_verified(),
        chunks(&served),
        "the restarted worker verified only what was missing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flipped_byte_fails_with_a_report_and_is_not_retried() {
    let mut h = Harness::new().await;
    let served = h.serve(package(7, 9 * MIB, Uuid::now_v7())).await;
    served.rig.corrupt(0, 3 * MIB + 17);
    let package = h.queue(&served).await;
    assert!(
        matches!(h.finished(package).await, InstallOutcome::Failed { code, .. } if code == "damaged_file")
    );
    assert_eq!(
        h.job_state(package).await,
        Some(DownloadState::Failed {
            error: DownloadError::DamagedFile { reported: true }
        })
    );
    assert_eq!(served.api.reports().len(), 1);
    // Never retried automatically, even on a recheck.
    let requests = served.rig.pack_requests();
    h.downloads.recheck().await;
    h.downloads.wake();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(served.rig.pack_requests(), requests);
    // The player can remove it: the history says it failed.
    h.downloads.remove(package).await.unwrap();
    let queue = h.downloads.list(|_, _| None).await.unwrap();
    assert!(queue.jobs.is_empty());
    assert!(matches!(
        queue.history[0].outcome,
        InstallOutcome::Failed { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untrusted_signing_key_writes_nothing() {
    let mut h = Harness::new().await;
    *h.backend.trust.lock().unwrap() = Arc::new(trust_state_with(2, true));
    let served = h.serve(package(8, MIB, Uuid::now_v7())).await;
    let package = h.queue(&served).await;
    assert!(
        matches!(h.finished(package).await, InstallOutcome::Failed { code, .. } if code == "untrusted_key")
    );
    assert_eq!(
        h.job_state(package).await,
        Some(DownloadState::Failed {
            error: DownloadError::UntrustedKey
        })
    );
    assert!(!h.root(&served).exists(), "nothing written");
    assert_eq!(served.rig.pack_requests(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_all_at_checkpoint_resolves_with_two_active_installs() {
    let mut h = Harness::new().await;
    h.downloads
        .set_settings(DownloadSettings {
            bandwidth_limit_kib: None,
            concurrent_installs: 2,
        })
        .await
        .unwrap();
    let first = h.serve(package(9, 48 * MIB, Uuid::now_v7())).await;
    let second = h.serve(package(11, 48 * MIB, Uuid::now_v7())).await;
    // Slow enough (1 MiB pieces) that neither can finish before the checkpoint.
    first.rig.set_piece_delay(Duration::from_millis(250));
    second.rig.set_piece_delay(Duration::from_millis(250));
    let a = h.queue(&first).await;
    let b = h.queue(&second).await;
    h.until_state(a, |s| *s == DownloadState::Active).await;
    h.until_state(b, |s| *s == DownloadState::Active).await;
    tokio::time::timeout(WAIT, h.downloads.pause_all_at_checkpoint())
        .await
        .expect("both installs reached a checkpoint");
    let history = h.downloads.list(|_, _| None).await.unwrap().history;
    assert_eq!(
        h.job_state(a).await,
        Some(DownloadState::Queued),
        "{history:?}"
    );
    assert_eq!(
        h.job_state(b).await,
        Some(DownloadState::Queued),
        "{history:?}"
    );
    // Held: nothing starts until the checkpoint is released.
    h.downloads.wake();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.job_state(a).await, Some(DownloadState::Queued));
    first.rig.set_piece_delay(Duration::ZERO);
    second.rig.set_piece_delay(Duration::ZERO);
    h.downloads.release_checkpoint();
    let mut done = 0;
    while done < 2 {
        if let AppEvent::InstallFinished(f) = h.next_event().await {
            assert!(matches!(f.outcome, InstallOutcome::Installed));
            done += 1;
        }
    }
}

/// GAME-06's check, faked: the launch target comes first and is refused.
struct RefuseTarget {
    seen: Mutex<Vec<(PathBuf, Vec<u8>)>>,
    requests_at_check: Mutex<Option<(Arc<Served>, u64)>>,
}

impl PriorityHook for RefuseTarget {
    fn priority_files(
        &self,
        _package: PackageRef,
        _manifest: &vgames_core::manifest::Manifest,
    ) -> Vec<String> {
        vec!["Game.exe".into()]
    }

    fn check<'a>(&'a self, _package: PackageRef, paths: &'a [PathBuf]) -> CheckFuture<'a> {
        for path in paths {
            self.seen
                .lock()
                .unwrap()
                .push((path.clone(), std::fs::read(path).unwrap()));
        }
        if let Some((served, count)) = self.requests_at_check.lock().unwrap().as_mut() {
            *count = served.rig.pack_requests();
        }
        Box::pin(async { Err(CompatBlocker::D3d12UnsupportedOnMac) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_launch_target_stops_the_install_before_more_packs() {
    let mut h = Harness::new().await;
    let served = h.serve(package(12, 20 * MIB, Uuid::now_v7())).await;
    let hook = Arc::new(RefuseTarget {
        seen: Mutex::new(Vec::new()),
        requests_at_check: Mutex::new(Some((Arc::clone(&served), 0))),
    });
    h.downloads
        .set_priority_hook(Arc::clone(&hook) as Arc<dyn PriorityHook>);
    let package = h.queue(&served).await;
    assert!(
        matches!(h.finished(package).await, InstallOutcome::Failed { code, .. } if code == "blocked")
    );
    let seen = hook.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, h.root(&served).join("Game.exe"));
    assert_eq!(
        seen[0].1,
        served.package.files[0].bytes(),
        "only verified bytes"
    );
    let at_check = hook.requests_at_check.lock().unwrap().as_ref().unwrap().1;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        served.rig.pack_requests(),
        at_check,
        "no pack request after the refusal"
    );
    assert!(!h.root(&served).exists(), "partial files removed");
    assert!(db::installs::row(&h.db, package).await.unwrap().is_none());
    let queue = h.downloads.list(|_, _| None).await.unwrap();
    assert!(queue.jobs.is_empty());
}

/// The catalog against a mock API, for `installs::start`.
struct MockConnections(crate::api::ApiClient);

impl crate::catalog::Connections for MockConnections {
    async fn active(&self) -> Result<crate::api::ApiClient, crate::catalog::CatalogError> {
        Ok(self.0.clone())
    }
    async fn client(
        &self,
        _server_id: Uuid,
    ) -> Result<crate::api::ApiClient, crate::catalog::CatalogError> {
        Ok(self.0.clone())
    }
}

async fn catalog_for(
    h: &Harness,
    mock: &wiremock::MockServer,
) -> crate::catalog::Catalog<MockConnections> {
    let base: url::Url = mock.uri().parse().unwrap();
    let session = Arc::new(crate::api::Session::new(
        SERVER_ID,
        base.clone(),
        crate::secrets::VaultHandle(Arc::new(crate::secrets::MemoryVault::default())),
        Arc::new(|_| {}),
    ));
    session
        .store(
            &serde_json::from_value(serde_json::json!({
                "access_token": "a", "refresh_token": "r", "token_type": "Bearer", "expires_in": 900,
                "user": {"id": "01920000-0000-7000-8000-00000000000a", "username": "alice",
                         "role": "user", "created_at": "2026-09-24T10:00:00Z"}
            }))
            .unwrap(),
        )
        .await;
    let client = crate::api::ApiClient::new(crate::api::http_client().unwrap(), base, session);
    let cache = Arc::new(crate::images::ImageCache::open(h._dir.path().join("images")).unwrap());
    crate::catalog::Catalog::new(
        Arc::new(MockConnections(client)),
        h.db.clone(),
        Arc::new(crate::catalog::covers::Covers::new(
            cache,
            reqwest::Client::new(),
        )),
        Some(vgames_core::manifest::Platform::LinuxX86_64),
    )
}

async fn serve_detail(mock: &wiremock::MockServer, served: &Served, slug: &str) {
    use wiremock::matchers::{method, path};
    let p = &served.package;
    wiremock::Mock::given(method("GET"))
        .and(path(format!("/v1/packages/{}", p.expected.package_id)))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": p.expected.package_id,
                "slug": slug,
                "title": "Gilded Garden",
                "genres": [],
                "platforms": ["linux-x86_64"],
                "updated_at": "2026-10-01T12:00:00Z",
                "releases": [{
                    "platform": "linux-x86_64",
                    "version_id": p.expected.version_id,
                    "version_label": "1.1",
                    "sequence": 1,
                    "total_size": p.total_bytes(),
                    "published_at": "2026-10-01T12:00:00Z"
                }]
            })),
        )
        .mount(mock)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installs_start_queues_and_reports_progress_and_the_end() {
    let mut h = Harness::new().await;
    let served = h.serve(package(13, 9 * MIB, Uuid::now_v7())).await;
    let mock = wiremock::MockServer::start().await;
    // An unsafe slug never becomes a folder name: the package id does.
    serve_detail(&mock, &served, "../escape").await;
    let catalog = catalog_for(&h, &mock).await;
    let target = Harness::package_ref(&served);
    crate::installs::start(&catalog, &h.downloads, target, h.library.root.id)
        .await
        .unwrap();
    let mut progress = false;
    let outcome = loop {
        match h.next_event().await {
            AppEvent::InstallProgress(p) if p.package == target => progress = true,
            AppEvent::InstallFinished(f) if f.package == target => break f.outcome,
            _ => {}
        }
    };
    assert!(progress);
    assert!(matches!(outcome, InstallOutcome::Installed));
    assert_installed(&h, &served);
    assert_eq!(
        crate::installs::start(&catalog, &h.downloads, target, h.library.root.id).await,
        Err(crate::installs::InstallStartError::AlreadyInstalled)
    );

    // Not enough space: refused up front with the numbers.
    let other = h.serve(package(14, MIB, Uuid::now_v7())).await;
    serve_detail(&mock, &other, "other").await;
    h.free.store(1000, Ordering::SeqCst);
    assert!(matches!(
        crate::installs::start(
            &catalog,
            &h.downloads,
            Harness::package_ref(&other),
            h.library.root.id
        )
        .await,
        Err(crate::installs::InstallStartError::InsufficientSpace {
            available_bytes: 1000,
            ..
        })
    ));
}
