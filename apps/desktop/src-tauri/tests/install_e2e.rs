//! INS-08 (M2): publish → install → crash → resume → verify → launch → uninstall, against the
//! real API in process (PostgreSQL, fs storage) and the launcher's own download worker.
//!
//! ```sh
//! VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/postgres \
//!   cargo test -p vgames-desktop --test install_e2e -- --ignored --nocapture
//! ```
//!
//! `VGAMES_E2E_SCALE` (default 0.01) scales the published package: 100,000 small files and
//! 8 GiB of large files at 1.0 (the nightly run), 1,000 files and ~80 MiB at the default.
//!
//! The crash is real: the first half of the download runs in a child process (this test
//! binary, re-run as `install_e2e_child`) that is killed with SIGKILL at about 40 %.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "support/install.rs"]
mod support;

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use support::{Api, Desktop, PackageSpec};
use uuid::Uuid;
use vgames_desktop_lib::events::{AppEvent, InstallOutcome, InstallPhase, PackageRef};
use vgames_transfer::download::journal::{Journal, JournalKey};

const CHILD_ENV: &str = "VGAMES_E2E_CHILD";
/// The first run is killed once this share of the download is on disk.
const KILL_AT: f64 = 0.40;
/// The first run is throttled to finish in about this many seconds, so the kill (at `KILL_AT`,
/// after the next journal flush) lands mid-way.
const FIRST_RUN_SECONDS: u64 = 20;

/// What the parent hands to the first (killed) run.
#[derive(Serialize, Deserialize)]
struct ChildConfig {
    data_dir: PathBuf,
    server_id: Uuid,
    token: String,
    package: PackageRef,
    library_id: Uuid,
    limit_kib: u32,
}

fn scale() -> f64 {
    std::env::var("VGAMES_E2E_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|s: &f64| *s > 0.0 && *s <= 1.0)
        .unwrap_or(0.01)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn install_e2e() {
    let scale = scale();
    let work = tempfile::tempdir().unwrap();
    let api = Api::start().await;

    // Publish: upload, sign, server-side verify job, publish (the `vgames publish` path).
    let source = work.path().join("source");
    let spec = PackageSpec::scaled(scale);
    let total = spec.generate(&source);
    let game = api.publish_package("E2E Game", &source).await;
    eprintln!(
        "published {} ({} files, {} bytes, scale {scale})",
        game.package_id,
        spec.file_count(),
        total
    );

    // The launcher: add the server (fingerprint, trust bundle) and a library folder.
    let data_dir = work.path().join("launcher");
    let library_dir = work.path().join("Games");
    std::fs::create_dir_all(&library_dir).unwrap();
    let library_id = {
        let desktop = Desktop::open(&data_dir, api.server_id, &api.token).await;
        desktop.add_server(&api.url).await;
        let library =
            vgames_desktop_lib::db::libraries::add(&desktop.state.db, &library_dir, "Games")
                .await
                .unwrap();
        library.root.id
    };
    let package = PackageRef {
        server_id: api.server_id,
        package_id: game.package_id,
    };

    // Disk usage is sampled for the whole run: data files never exceed the final size; only
    // the journal (`.vgames`) comes on top.
    let sampler = DiskSampler::start(library_dir.clone());

    // First run: install_start, then SIGKILL at ~40 %.
    let config = ChildConfig {
        data_dir: data_dir.clone(),
        server_id: api.server_id,
        token: api.token.clone(),
        package,
        library_id,
        // Keep the full-scale producer below the journal flush throughput.
        limit_kib: u32::try_from((total / FIRST_RUN_SECONDS / 1024).clamp(64, 64 * 1024)).unwrap(),
    };
    let manifest_bytes = api.manifest_bytes(&game);
    let manifest = vgames_core::manifest::parse_and_validate(&manifest_bytes).unwrap();
    let chunk_sizes: Vec<_> = manifest.chunks.iter().map(|chunk| chunk.size).collect();
    let journal_key = JournalKey {
        version_id: game.version_id,
        manifest_blake3: *blake3::hash(&manifest_bytes).as_bytes(),
        chunk_count: u32::try_from(chunk_sizes.len()).unwrap(),
    };
    let (killed_at, resumed_from, durable_chunks) =
        run_and_kill_child(&config, total, &library_dir, journal_key, &chunk_sizes);
    eprintln!("first run killed at {killed_at} of {total} bytes");

    // Second run: a fresh launcher on the same profile resumes the queued job.
    let desktop = Desktop::open(&data_dir, api.server_id, &api.token).await;
    desktop.unlimited().await;
    let mut events = desktop.state.bus.subscribe();
    desktop.state.downloads.start();
    let mut first_progress = None;
    let outcome = loop {
        match tokio::time::timeout(Duration::from_secs(600), events.recv()).await {
            // Progress can coalesce the checkpoint with newly downloaded chunks.
            Ok(Ok(AppEvent::InstallProgress(p)))
                if p.package == package && p.phase == InstallPhase::Downloading =>
            {
                first_progress.get_or_insert(p.bytes_done);
            }
            Ok(Ok(AppEvent::InstallFinished(f))) if f.package == package => break f.outcome,
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(e)) => panic!("event bus closed: {e}"),
            Err(_) => panic!("the resumed install did not finish"),
        }
    };
    assert!(matches!(outcome, InstallOutcome::Installed), "{outcome:?}");
    // The kill followed a journal flush past `KILL_AT`: what it recorded is kept, and only
    // chunks in flight at the kill are fetched again.
    assert!(first_progress.is_some_and(|bytes| bytes >= resumed_from));
    eprintln!("second run resumed at {resumed_from} of {total} bytes");
    assert!(
        resumed_from as f64 >= total as f64 * KILL_AT * 0.75 && resumed_from <= killed_at,
        "the second run did not resume ({resumed_from} of {total} bytes, killed at {killed_at})"
    );

    assert_eq!(
        desktop.state.downloads.chunks_verified(),
        u64::try_from(chunk_sizes.len()).unwrap() - u64::from(durable_chunks),
        "the restart fetched chunks already recorded in the durable checkpoint"
    );

    // Byte-identical tree.
    let row = vgames_desktop_lib::db::installs::row(&desktop.state.db, package)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "installed");
    assert_eq!(
        read_tree(&row.root),
        read_tree(&source),
        "installed tree differs"
    );

    let peak = sampler.stop();
    eprintln!(
        "peak disk use: {} data + {} journal, final size {total}",
        peak.data, peak.journal
    );
    assert!(
        peak.data <= total,
        "data files reached {} of {total} bytes",
        peak.data
    );

    // A flipped byte in a pack is refused before that chunk is written.
    flipped_byte_is_refused(
        &api,
        &desktop,
        &work.path().join("small"),
        library_id,
        &row.root,
    )
    .await;

    // Launch the dummy executable through the launcher's own path (Linux x86_64 build only).
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let marker = row.root.join("bin/launched.txt");
        let mut events = desktop.state.bus.subscribe();
        let pid = desktop
            .state
            .launcher
            .launch(
                package,
                vgames_desktop_lib::launch::target::TargetChoice::Default,
            )
            .await
            .expect("launch");
        assert!(pid > 0);
        loop {
            match tokio::time::timeout(Duration::from_secs(60), events.recv()).await {
                Ok(Ok(AppEvent::GameStopped(s))) if s.package == package => break,
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                other => panic!("the game did not stop: {other:?}"),
            }
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "launched");
    }

    // Uninstall, leftovers (the marker) included.
    let target = vgames_desktop_lib::installs::manage::target(&desktop.state.db, package, false)
        .await
        .unwrap();
    vgames_desktop_lib::installs::manage::uninstall(&desktop.state.db, package, target, true, None)
        .await
        .unwrap();
    assert!(!row.root.exists(), "the install folder is still there");
    assert!(
        vgames_desktop_lib::db::installs::row(&desktop.state.db, package)
            .await
            .unwrap()
            .is_none()
    );

    desktop.state.shutdown.cancel();
    api.stop().await;
}

/// The first, killed run. Does nothing unless started by [`install_e2e`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "started by install_e2e"]
async fn install_e2e_child() {
    let Ok(raw) = std::env::var(CHILD_ENV) else {
        return;
    };
    let config: ChildConfig = serde_json::from_str(&raw).unwrap();
    let desktop = Desktop::open(&config.data_dir, config.server_id, &config.token).await;
    desktop.limit(config.limit_kib).await;
    let mut events = desktop.state.bus.subscribe();
    desktop.state.downloads.start();
    vgames_desktop_lib::installs::start(
        &desktop.state.catalog,
        &desktop.state.downloads,
        config.package,
        config.library_id,
    )
    .await
    .expect("install_start");
    loop {
        match events.recv().await {
            Ok(AppEvent::InstallProgress(p)) if p.package == config.package => {
                println!("E2E_PROGRESS {} {}", p.bytes_done, p.bytes_total);
            }
            Ok(AppEvent::InstallFinished(f)) if f.package == config.package => {
                println!("E2E_DONE {:?}", f.outcome);
                return;
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(e) => panic!("event bus closed: {e}"),
        }
    }
}

/// Runs the child until its reported progress and durable journal both reach `KILL_AT`,
/// then SIGKILLs it. Returns the observed bytes, durable bytes and durable chunk count.
/// (The journal is persisted every 2 s
/// after fsyncing the files written since the last one; a kill before the first flush keeps
/// nothing, which would prove nothing.)
fn run_and_kill_child(
    config: &ChildConfig,
    total: u64,
    library: &Path,
    key: JournalKey,
    chunk_sizes: &[u64],
) -> (u64, u64, u32) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "install_e2e_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, serde_json::to_string(config).unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Drained on its own thread, so the child never blocks on a full pipe.
    let stdout = child.stdout.take().unwrap();
    let (lines, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if lines.send(line).is_err() {
                return;
            }
        }
    });
    let mut reached = 0;
    let mut journal_path = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("E2E_PROGRESS ") {
                    reached = rest.split(' ').next().unwrap().parse().unwrap();
                } else if line.starts_with("E2E_DONE") {
                    panic!(
                        "the first run finished before it was killed ({line}); lower its bandwidth"
                    );
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the first run exited on its own: {:?}", child.wait());
            }
        }
        if reached as f64 >= total as f64 * KILL_AT {
            if journal_path.is_none() {
                journal_path = walk(library)
                    .into_iter()
                    .find(|p| p.file_name().is_some_and(|n| n == "journal.bin"));
            }
            if let Some(path) = journal_path.as_ref()
                && journalled_bytes(path, key, chunk_sizes) as f64 >= total as f64 * KILL_AT
            {
                child.kill().unwrap();
                break;
            }
        }
    }
    let status = child.wait().unwrap();
    assert!(!status.success(), "the first run was not killed: {status}");
    // Reap the child and drain its pipe before taking the exact restart checkpoint.
    for line in rx {
        if let Some(rest) = line.strip_prefix("E2E_PROGRESS ") {
            reached = reached.max(rest.split(' ').next().unwrap().parse().unwrap());
        }
    }
    let path = journal_path.unwrap();
    let durable = journalled_bytes(&path, key, chunk_sizes);
    let chunks = Journal::load_or_new(&path, key).done().count();
    assert!(durable < total, "the kill must leave chunks to resume");
    (reached.max(durable), durable, chunks)
}

/// A late mtime can belong to an earlier snapshot waiting for slow fsyncs.
/// Only the authenticated journal bitset proves which bytes are durable.
fn journalled_bytes(path: &Path, key: JournalKey, chunk_sizes: &[u64]) -> u64 {
    let journal = Journal::load_or_new(path, key);
    chunk_sizes
        .iter()
        .enumerate()
        .filter(|(index, _)| journal.done().get(u32::try_from(*index).unwrap()))
        .map(|(_, size)| size)
        .sum()
}

#[test]
fn a_late_journal_write_does_not_make_earlier_chunks_durable() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("journal.bin");
    let key = JournalKey {
        version_id: Uuid::nil(),
        manifest_blake3: [7; 32],
        chunk_count: 2,
    };
    let crossed = std::time::SystemTime::now();
    let mut journal = Journal::load_or_new(&path, key);
    journal.mark(0);
    journal.persist().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(crossed + Duration::from_secs(1)))
        .unwrap();
    assert!(std::fs::metadata(&path).unwrap().modified().unwrap() > crossed);
    assert_eq!(journalled_bytes(&path, key, &[10, 990]), 10);
    journal.mark(1);
    journal.persist().unwrap();
    assert_eq!(journalled_bytes(&path, key, &[10, 990]), 1000);
}

/// Publishes a small package, flips one byte inside one of its chunks in storage, and checks
/// that the install fails without that chunk's bytes ever reaching the file.
async fn flipped_byte_is_refused(
    api: &Api,
    desktop: &Desktop,
    source: &Path,
    library_id: Uuid,
    other_install: &Path,
) {
    let spec = PackageSpec::small();
    spec.generate(source);
    let small = api.publish_package("E2E Small", source).await;
    let manifest = api.manifest(&small);
    // The middle of the largest chunk of pack 0.
    let (index, chunk) = manifest
        .chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.pack == 0)
        .max_by_key(|(_, c)| c.stored_size)
        .unwrap();
    assert_eq!(
        chunk.stored_size, chunk.size,
        "the flipped-byte check needs a stored (uncompressed) chunk"
    );
    let within = chunk.size / 2;
    api.flip_pack_byte(&small, 0, chunk.offset + within);

    // Which file, and where, would receive the flipped byte.
    let chunk_start: u64 = manifest.chunks[..index].iter().map(|c| c.size).sum();
    let stream_pos = chunk_start + within;
    let (path, file_pos) = manifest
        .files
        .iter()
        .filter_map(|f| {
            let first = f.chunk? as usize;
            let start = manifest.chunks[..first].iter().map(|c| c.size).sum::<u64>() + f.offset;
            (start..start + f.size)
                .contains(&stream_pos)
                .then(|| (f.path.clone(), stream_pos - start))
        })
        .next()
        .expect("a file covers the flipped byte");
    let good = std::fs::read(source.join(&path)).unwrap()[usize::try_from(file_pos).unwrap()];

    let package = PackageRef {
        server_id: api.server_id,
        package_id: small.package_id,
    };
    let mut events = desktop.state.bus.subscribe();
    vgames_desktop_lib::installs::start(
        &desktop.state.catalog,
        &desktop.state.downloads,
        package,
        library_id,
    )
    .await
    .expect("install_start");
    let outcome = loop {
        match tokio::time::timeout(Duration::from_secs(120), events.recv()).await {
            Ok(Ok(AppEvent::InstallFinished(f))) if f.package == package => break f.outcome,
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("the corrupted install did not finish: {other:?}"),
        }
    };
    assert!(
        matches!(&outcome, InstallOutcome::Failed { code, .. } if code == "damaged_file"),
        "a corrupted pack was not refused as damaged: {outcome:?}"
    );
    // Wherever the install left that file (or nothing), the flipped byte is not in it.
    let library = vgames_desktop_lib::db::libraries::list(&desktop.state.db)
        .await
        .unwrap()
        .into_iter()
        .find(|l| l.root.id == library_id)
        .unwrap();
    for entry in walk(&library.root.path) {
        if !entry.starts_with(other_install)
            && entry.ends_with(&path)
            && let Ok(bytes) = std::fs::read(&entry)
            && let Some(byte) = bytes.get(usize::try_from(file_pos).unwrap())
        {
            assert!(
                *byte == good || *byte == 0,
                "the corrupted chunk was written to {}",
                entry.display()
            );
        }
    }
    // The queue holds the failed job; clear it so later steps see an idle worker.
    desktop.state.downloads.remove(package).await.ok();
}

/// Every regular file under `root` (relative path → bytes), outside the launcher's `.vgames`.
fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walk(root)
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (!rel.starts_with(".vgames")).then(|| (rel, std::fs::read(&p).unwrap()))
        })
        .collect()
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

#[derive(Debug, Default, Clone, Copy)]
struct Peak {
    data: u64,
    journal: u64,
}

/// Samples a library folder's size every 20 ms on its own thread.
struct DiskSampler {
    stop: Arc<AtomicBool>,
    data: Arc<AtomicU64>,
    journal: Arc<AtomicU64>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DiskSampler {
    fn start(root: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let data = Arc::new(AtomicU64::new(0));
        let journal = Arc::new(AtomicU64::new(0));
        let thread = std::thread::spawn({
            let (stop, data, journal) =
                (Arc::clone(&stop), Arc::clone(&data), Arc::clone(&journal));
            move || {
                while !stop.load(Ordering::SeqCst) {
                    let (mut d, mut j) = (0u64, 0u64);
                    for path in walk(&root) {
                        let Ok(meta) = std::fs::symlink_metadata(&path) else {
                            continue;
                        };
                        let in_journal = path
                            .strip_prefix(&root)
                            .map(|rel| {
                                rel.components()
                                    .any(|c| c.as_os_str().to_string_lossy().starts_with(".vgames"))
                            })
                            .unwrap_or(false);
                        if in_journal {
                            j += meta.len();
                        } else {
                            d += meta.len();
                        }
                    }
                    data.fetch_max(d, Ordering::SeqCst);
                    journal.fetch_max(j, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        });
        Self {
            stop,
            data,
            journal,
            thread: Mutex::new(Some(thread)),
        }
    }

    fn stop(&self) -> Peak {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.lock().unwrap().take() {
            t.join().unwrap();
        }
        Peak {
            data: self.data.load(Ordering::SeqCst),
            journal: self.journal.load(Ordering::SeqCst),
        }
    }
}
