#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Loopback throughput benchmark (A2-T04 acceptance: ≥ 1.5 GB/s on 4 cores).
//! Ignored by default; run in release mode on a RAM disk so the disk is not
//! the bottleneck:
//!
//! ```sh
//! VGAMES_BENCH_DIR=/dev/shm cargo test --release -p vgames-transfer \
//!   --test bench_loopback -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use vgames_pack::Compression;
use vgames_transfer::download::{DownloadControl, DownloadOptions};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::install::{self, InstallOutcome};
use vgames_transfer::testkit::{FileSpec, MockApi, Rig, TestPackage};

const MIB: u64 = 1024 * 1024;

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn loopback_throughput() {
    let gib: u64 = std::env::var("VGAMES_BENCH_GIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let base = std::env::var_os("VGAMES_BENCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let files: Vec<FileSpec> = (0..gib * 4)
        .map(|i| FileSpec::random(&format!("data/{i:03}.bin"), 256 * MIB, i + 1))
        .collect();
    let package = TestPackage::build(&files, &[], Compression::None);
    let expected: Vec<(String, vgames_core::Digest)> = package
        .release()
        .manifest()
        .files
        .iter()
        .map(|f| (f.path.clone(), f.blake3))
        .collect();
    std::fs::remove_dir_all(package.source.path()).unwrap();
    let target = tempfile::tempdir_in(&base).unwrap();
    let root = target.path().join("game");
    let total = gib * 1024 * MIB;
    let disk = disk_baseline(target.path(), total);

    runtime.block_on(async {
        let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
        let api = MockApi::new(&rig, package.packs.iter().map(|p| p.len() as u64).collect());
        let options = DownloadOptions {
            client: ClientOptions {
                use_system_proxy: false,
                ..ClientOptions::default()
            },
            aimd_interval: Duration::from_millis(500),
            ..DownloadOptions::default()
        };
        let control = DownloadControl::new(None);
        let mut progress = control.progress();
        let watcher = tokio::spawn(async move {
            let mut started = None;
            let mut connections = 0;
            while progress.changed().await.is_ok() {
                let p = progress.borrow().clone();
                if p.phase == vgames_transfer::download::Phase::Downloading && started.is_none() {
                    started = Some(Instant::now());
                }
                connections = connections.max(p.connections);
                if p.phase == vgames_transfer::download::Phase::Finalizing {
                    break;
                }
            }
            (started, Instant::now(), connections)
        });
        let start = Instant::now();
        let report = install::install(&root, Arc::new(package.release()), api, &options, &control)
            .await
            .unwrap();
        let elapsed = start.elapsed();
        let (download_start, download_end, connections) = watcher.await.unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        let download = download_end.duration_since(download_start.unwrap_or(start));
        let rate = |d: Duration| total as f64 / d.as_secs_f64() / 1e9;
        eprintln!(
            "budget throughput: {gib} GiB loopback, download phase {:.2} s = {:.2} GB/s; whole install {:.2} s = {:.2} GB/s; \
             peak connections {connections}, {} buffers, {} ranges served",
            download.as_secs_f64(),
            rate(download),
            elapsed.as_secs_f64(),
            rate(elapsed),
            report.stats.buffers_allocated,
            rig.ranges().len(),
        );
        eprintln!(
            "budget throughput: the same folder takes plain sequential writes at {:.2} GB/s \
             (the disk's ceiling for this run)",
            disk
        );
    });
    for (path, blake3) in expected {
        let bytes = std::fs::read(root.join(&path)).unwrap();
        assert!(vgames_core::Digest::of(&bytes) == blake3, "{path}");
    }
}

/// Writes `total` bytes sequentially into one file under `dir` (8 MiB writes, then fsync) and
/// returns GB/s: what the disk alone allows, to tell a disk-bound run from an engine-bound one.
fn disk_baseline(dir: &std::path::Path, total: u64) -> f64 {
    use std::io::Write;
    let path = dir.join("baseline.bin");
    let buffer = vec![0x5au8; 8 * MIB as usize];
    let start = Instant::now();
    let mut file = std::fs::File::create(&path).unwrap();
    let mut written = 0;
    while written < total {
        file.write_all(&buffer).unwrap();
        written += buffer.len() as u64;
    }
    file.sync_all().unwrap();
    drop(file);
    let rate = written as f64 / start.elapsed().as_secs_f64() / 1e9;
    std::fs::remove_file(&path).unwrap();
    rate
}
