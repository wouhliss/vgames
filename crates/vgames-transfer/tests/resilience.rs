#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! A2-T04 acceptance, the hard parts: killing the process at random points
//! and resuming, disk footprint during a download, and bounded memory.
//!
//! The kill and memory tests re-run this test binary as a child process
//! (`child_entry`), which installs from a rig served by the parent.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vgames_core::Envelope;
use vgames_pack::Compression;
use vgames_transfer::download::{DownloadControl, DownloadOptions};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::install::{self, InstallOutcome};
use vgames_transfer::testkit::{
    FileSpec, Identity, Links, MockApi, Rig, TestPackage, default_expected, install_mode,
    random_files, read_tree, trust_state,
};

const MIB: u64 = 1024 * 1024;
const CHILD_ENV: &str = "VGAMES_TRANSFER_CHILD_JOB";

fn options() -> DownloadOptions {
    let mut o = DownloadOptions {
        client: ClientOptions {
            use_system_proxy: false,
            ..ClientOptions::default()
        },
        ..DownloadOptions::default()
    };
    o.timings.stall_timeout = Duration::from_secs(2);
    o.timings.backoff_base = Duration::from_millis(5);
    o.timings.backoff_max = Duration::from_millis(50);
    o.journal_interval = Duration::from_millis(50);
    o.aimd_interval = Duration::from_millis(200);
    o.progress_interval = Duration::from_millis(50);
    o
}

/// What the child process does.
#[derive(Serialize, Deserialize)]
struct Job {
    base_url: String,
    root: PathBuf,
    pack_sizes: Vec<u64>,
    connections: usize,
    max_range_bytes: u64,
    /// Write peak RSS (KiB) here when done.
    rss_out: Option<PathBuf>,
}

fn write_job(dir: &Path, job: &Job, package: &TestPackage) {
    std::fs::write(dir.join("job.json"), serde_json::to_vec(job).unwrap()).unwrap();
    std::fs::write(dir.join("manifest.json"), &package.manifest).unwrap();
    std::fs::write(dir.join("manifest.sig"), package.envelope.to_bytes()).unwrap();
}

/// Child side: runs one install described by `$VGAMES_TRANSFER_CHILD_JOB`.
/// Does nothing in a normal test run.
#[test]
fn child_entry() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let job: Job = serde_json::from_slice(&std::fs::read(dir.join("job.json")).unwrap()).unwrap();
    let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
    let envelope = Envelope::parse(&std::fs::read(dir.join("manifest.sig")).unwrap()).unwrap();
    let release = install::verify_release(
        &trust_state(),
        envelope,
        manifest,
        &default_expected(),
        None,
        install_mode(),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let api = MockApi::with_links(Links::remote(&job.base_url), job.pack_sizes.clone());
        let options = DownloadOptions {
            initial_connections: job.connections,
            max_connections: job.connections.max(2),
            adaptive: false,
            max_range_bytes: job.max_range_bytes,
            ..options()
        };
        let control = DownloadControl::new(None);
        let report = install::install(&job.root, Arc::new(release), api, &options, &control)
            .await
            .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        if let Some(out) = &job.rss_out {
            std::fs::write(
                out,
                format!("{} {}", peak_rss_kib(), report.stats.buffers_allocated),
            )
            .unwrap();
        }
    });
}

/// This process's peak resident memory, in KiB.
#[cfg(target_os = "linux")]
fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn peak_rss_kib() -> u64 {
    // SAFETY: `getrusage` only writes the zeroed struct it is given.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0;
        }
        usage
    };
    // Bytes on macOS.
    u64::try_from(usage.ru_maxrss).unwrap_or(0) / 1024
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn peak_rss_kib() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the counters struct is plain data, zeroed, with its size passed in `cb`; the
    // current-process pseudo handle needs no closing.
    let counters = unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = size;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) == 0 {
            return 0;
        }
        counters
    };
    counters.PeakWorkingSetSize as u64 / 1024
}

fn spawn_child(job_dir: &Path) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["child_entry", "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, job_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn assert_identical(package: &TestPackage, root: &Path) {
    let source = read_tree(package.source.path());
    let installed = read_tree(root);
    assert_eq!(source.len(), installed.len());
    for (path, bytes) in &source {
        assert!(installed.get(path) == Some(bytes), "{path} differs");
    }
}

struct Xorshift(u64);
impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn killed_at_random_points_resumes_to_the_same_tree() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let mut files = random_files(77, 40, 6 * MIB);
    files.push(FileSpec::random("Game/big.pak", 9 * MIB, 99));
    let package = TestPackage::build_with(
        &files,
        &["Game/Saved"],
        Compression::Auto,
        8 * MIB,
        &Identity::default(),
    );
    let rig = runtime.block_on(Rig::start(package.manifest.clone(), package.packs.clone()));
    // ~2 ms per MiB per connection keeps a run around a few hundred ms.
    rig.set_piece_delay(Duration::from_millis(2));
    let sizes: Vec<u64> = package.packs.iter().map(|p| p.len() as u64).collect();
    let work = tempfile::tempdir().unwrap();
    let mut rng = Xorshift(0x5eed_1234_abcd_ef01);
    let iterations = 50;
    let mut killed_mid_download = 0;
    for i in 0..iterations {
        let root = work.path().join(format!("game-{i}"));
        let job_dir = work.path().join(format!("job-{i}"));
        std::fs::create_dir_all(&job_dir).unwrap();
        write_job(
            &job_dir,
            &Job {
                base_url: rig.base_url(),
                root: root.clone(),
                pack_sizes: sizes.clone(),
                connections: 4,
                max_range_bytes: 4 * MIB,
                rss_out: None,
            },
            &package,
        );
        // One or two kills before the parent finishes the install.
        let kills = 1 + (i % 5 == 4) as usize;
        for _ in 0..kills {
            let bytes_before = rig.pack_bytes();
            let kill_after_bytes = bytes_before + (1 + rng.next() % 16) * MIB;
            let mut child = spawn_child(&job_dir);
            // Process startup varies widely across platforms. Wait until this
            // child has actually requested pack bytes, then vary the kill
            // point within the transfer rather than often killing after a
            // fast child has already completed (especially on macOS).
            let deadline = Instant::now() + Duration::from_secs(10);
            while rig.pack_bytes() < kill_after_bytes && child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().ok();
                    child.wait().unwrap();
                    panic!("iteration {i}: child did not start downloading");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            std::thread::sleep(Duration::from_millis(rng.next() % 8));
            let finished = child.try_wait().unwrap().is_some();
            child.kill().ok();
            child.wait().unwrap();
            if !finished {
                killed_mid_download += 1;
            }
        }
        let api = MockApi::new(&rig, sizes.clone());
        let control = DownloadControl::new(None);
        let report = runtime
            .block_on(install::install(
                &root,
                Arc::new(package.release()),
                api,
                &options(),
                &control,
            ))
            .unwrap_or_else(|e| panic!("iteration {i}: {e:?}"));
        assert!(
            matches!(report.outcome, InstallOutcome::Installed(_)),
            "iteration {i}"
        );
        assert_identical(&package, &root);
        std::fs::remove_dir_all(&root).unwrap();
    }
    assert!(
        killed_mid_download >= iterations / 2,
        "{killed_mid_download} kills landed mid-run"
    );
}

/// Blocks allocated by every regular file under `root`.
#[cfg(unix)]
fn disk_usage(root: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.blocks() * 512;
            }
        }
    }
    total
}

/// Bytes allocated by every regular file under `root` (the compressed size on NTFS, which is the
/// allocated size for an uncompressed file, sparse ranges excluded).
#[cfg(windows)]
#[allow(unsafe_code)]
fn disk_usage(root: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
    let mut total = 0;
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
                continue;
            }
            let wide: Vec<u16> = entry
                .path()
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            let mut high = 0u32;
            // SAFETY: `wide` is a NUL-terminated path that outlives the call; `high` is a valid
            // out pointer.
            let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
            if low == INVALID_FILE_SIZE {
                // Deleted between the listing and the call (a temporary file), or unreadable.
                continue;
            }
            total += (u64::from(high) << 32) | u64::from(low);
        }
    }
    total
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disk_usage_never_exceeds_the_final_size_plus_journal() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let files = random_files(31, 50, 10 * MIB);
    let package = TestPackage::build(&files, &[], Compression::Auto);
    let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
    rig.set_piece_delay(Duration::from_millis(3));
    let api = MockApi::new(&rig, package.packs.iter().map(|p| p.len() as u64).collect());
    let target = tempfile::tempdir().unwrap();
    let root = target.path().join("game");
    let peak = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (root, peak, done) = (root.clone(), Arc::clone(&peak), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut samples = 0u64;
            while !done.load(Ordering::SeqCst) {
                peak.fetch_max(disk_usage(&root), Ordering::SeqCst);
                samples += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            samples
        })
    };
    let control = DownloadControl::new(None);
    let report = install::install(
        &root,
        Arc::new(package.release()),
        api,
        &options(),
        &control,
    )
    .await
    .unwrap();
    done.store(true, Ordering::SeqCst);
    let samples = sampler.join().unwrap();
    assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
    let final_usage = disk_usage(&root);
    let peak = peak.load(Ordering::SeqCst);
    // Journal + its temp file + the install record's temp file: one block each.
    let allowance = 3 * 4096;
    eprintln!(
        "budget disk: peak {peak} bytes, final {final_usage} bytes, {} bytes beyond (allowance \
         {allowance}, {samples} samples)",
        peak.saturating_sub(final_usage),
    );
    assert!(
        peak <= final_usage + allowance,
        "peak {peak} > final {final_usage} + {allowance} ({samples} samples)"
    );
    assert!(samples > 20, "the sampler ran during the download");
    assert_identical(&package, &root);
}

#[test]
fn memory_stays_bounded_at_32_connections() {
    use std::time::Instant;
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    // 512 MiB in 2 packs, 8 MiB ranges: 64 ranges for 32 connections.
    let files: Vec<FileSpec> = (0..16)
        .map(|i| FileSpec::random(&format!("data/{i:02}.bin"), 32 * MIB, i + 1))
        .collect();
    let package = TestPackage::build(&files, &[], Compression::None);
    let rig = runtime.block_on(Rig::start(package.manifest.clone(), package.packs.clone()));
    let work = tempfile::tempdir().unwrap();
    let rss_out = work.path().join("rss.txt");
    let root = work.path().join("game");
    write_job(
        work.path(),
        &Job {
            base_url: rig.base_url(),
            root: root.clone(),
            pack_sizes: package.packs.iter().map(|p| p.len() as u64).collect(),
            connections: 32,
            max_range_bytes: 8 * MIB,
            rss_out: Some(rss_out.clone()),
        },
        &package,
    );
    let start = Instant::now();
    let status = spawn_child(work.path()).wait().unwrap();
    assert!(status.success(), "child failed");
    let elapsed = start.elapsed();
    let out = std::fs::read_to_string(&rss_out).unwrap();
    let mut parts = out.split_whitespace();
    let peak_kib: u64 = parts.next().unwrap().parse().unwrap();
    let buffers: u64 = parts.next().unwrap().parse().unwrap();
    eprintln!("buffers allocated: {buffers}");
    eprintln!(
        "budget memory: 32 connections, 512 MiB: peak RSS {} MiB (budget 256), {:.0} MiB/s ({} build, \
         including process start)",
        peak_kib / 1024,
        512.0 / elapsed.as_secs_f64(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    assert!(peak_kib > 0, "peak RSS was measured");
    assert!(peak_kib <= 256 * 1024, "peak RSS {} MiB", peak_kib / 1024);
    assert_identical(&package, &root);
}
