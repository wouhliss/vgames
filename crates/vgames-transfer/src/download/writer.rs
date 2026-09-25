//! Writer threads (02 §7.5 and §7.8): each stored chunk is decoded (zstd
//! bounded to its size) and BLAKE3-checked **before** any of its bytes is
//! written; then its extents go to their final files with positional writes.
//! A dedicated pool of blocking threads, each with a small LRU of open
//! handles, so hashing and disk I/O never run on the async runtime.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use vgames_core::manifest::Encoding;
use vgames_pack::encode::{self, ChunkError, ExpectedChunk};

use super::pool::PooledBuf;
use super::scheduler::{Scheduler, Stop};
use super::table::{ChunkInfo, ChunkTable};
use super::{DownloadError, PauseReason};
use crate::fsutil;
use crate::sys;

/// A chunk's stored bytes, complete, on their way to verification.
pub struct ChunkJob {
    pub chunk: u32,
    pub stored: PooledBuf,
}

/// Counters shared with the progress reporter and tests.
#[derive(Debug, Default)]
pub struct Stats {
    /// Chunks whose hash was computed (each chunk once, plus retries).
    pub chunks_verified: AtomicU64,
    pub verify_failures: AtomicU64,
    /// Chunks written this run.
    pub chunks_written: AtomicU64,
    /// Decoded bytes written this run.
    pub bytes_written: AtomicU64,
    /// Stored bytes received from the network this run.
    pub network_bytes: AtomicU64,
}

/// Chunks written since the last journal flush, and the files they touched.
#[derive(Debug, Default)]
pub struct Durable {
    pub chunks: Vec<u32>,
    pub files: BTreeSet<u32>,
}

/// Where each manifest file's bytes go (`None`: not needed by this download).
pub type Targets = Vec<Option<PathBuf>>;

pub struct WriterShared {
    pub table: Arc<ChunkTable>,
    pub targets: Arc<Targets>,
    pub scheduler: Arc<Scheduler>,
    pub durable: Mutex<Durable>,
    pub stats: Arc<Stats>,
    pub handles_per_thread: usize,
    pub scratch_capacity: usize,
}

pub struct Writers {
    sender: Option<mpsc::Sender<ChunkJob>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Writers {
    /// Starts `count` writer threads reading from a channel of `capacity` jobs
    /// (at least the buffer-pool size, so sending never waits: every job holds
    /// a pool buffer).
    pub fn start(
        count: usize,
        capacity: usize,
        shared: Arc<WriterShared>,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<ChunkJob>(capacity.max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let mut threads = Vec::with_capacity(count);
        for i in 0..count.max(1) {
            let receiver = Arc::clone(&receiver);
            let shared = Arc::clone(&shared);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("vgames-writer-{i}"))
                    .spawn(move || writer_loop(&receiver, &shared))?,
            );
        }
        Ok(Self {
            sender: Some(sender),
            threads,
        })
    }

    pub fn sender(&self) -> Option<mpsc::Sender<ChunkJob>> {
        self.sender.clone()
    }

    /// Closes the queue and waits (off the runtime) for the threads to drain it.
    pub async fn finish(mut self) {
        self.sender = None;
        let threads = std::mem::take(&mut self.threads);
        let _ = tokio::task::spawn_blocking(move || {
            for thread in threads {
                let _ = thread.join();
            }
        })
        .await;
    }
}

fn writer_loop(receiver: &Mutex<mpsc::Receiver<ChunkJob>>, shared: &WriterShared) {
    let mut scratch: Vec<u8> = Vec::with_capacity(shared.scratch_capacity);
    let mut handles = HandleCache::new(shared.handles_per_thread);
    loop {
        let job = {
            let Ok(mut rx) = receiver.lock() else { return };
            rx.blocking_recv()
        };
        let Some(job) = job else { return };
        process(job, shared, &mut scratch, &mut handles);
    }
}

fn process(
    mut job: ChunkJob,
    shared: &WriterShared,
    scratch: &mut Vec<u8>,
    handles: &mut HandleCache,
) {
    let Some(info) = shared.table.chunk(job.chunk).copied() else {
        return;
    };
    shared.stats.chunks_verified.fetch_add(1, Ordering::Relaxed);
    if let Err(error) = verify(&info, &mut job.stored, scratch) {
        shared.stats.verify_failures.fetch_add(1, Ordering::Relaxed);
        shared
            .scheduler
            .mismatch(&shared.table, job.chunk, error.to_string());
        return;
    }
    let data = job.stored.as_slice();
    let mut at = 0usize;
    let mut touched: Vec<u32> = Vec::new();
    for extent in shared.table.extents(job.chunk) {
        let len = usize::try_from(extent.len).unwrap_or(usize::MAX);
        let Some(part) = data.get(at..at.saturating_add(len)) else {
            return shared.scheduler.stop(Stop::Failed(DownloadError::Internal(
                "chunk extents exceed the chunk".into(),
            )));
        };
        at += len;
        let Some(Some(path)) = shared.targets.get(extent.file as usize) else {
            continue;
        };
        let result = handles
            .get(extent.file, path)
            .and_then(|file| fsutil::write_all_at(file, part, extent.file_offset));
        if let Err(error) = result {
            handles.forget(extent.file);
            if sys::is_disk_full(&error) {
                tracing::warn!(path = %path.display(), "disk full; pausing the download");
                // The chunk is not marked done: it is fetched again on resume.
                shared.scheduler.stop(Stop::Paused(PauseReason::DiskFull));
            } else {
                shared.scheduler.stop(Stop::Failed(DownloadError::Io {
                    op: "write",
                    path: path.clone(),
                    source: error,
                }));
            }
            return;
        }
        touched.push(extent.file);
    }
    shared.stats.chunks_written.fetch_add(1, Ordering::Relaxed);
    shared
        .stats
        .bytes_written
        .fetch_add(info.size, Ordering::Relaxed);
    if let Ok(mut durable) = shared.durable.lock() {
        durable.chunks.push(job.chunk);
        durable.files.extend(touched);
    }
    shared.scheduler.chunk_done();
}

/// Checks the stored bytes; afterwards `stored` holds the decoded chunk.
fn verify(
    info: &ChunkInfo,
    stored: &mut PooledBuf,
    scratch: &mut Vec<u8>,
) -> Result<(), ChunkError> {
    if stored.len() as u64 != info.stored_size {
        return Err(ChunkError::StoredSize {
            expected: info.stored_size,
            found: stored.len() as u64,
        });
    }
    match info.encoding {
        Encoding::Raw => {
            if info.stored_size != info.size {
                return Err(ChunkError::DecodedSize {
                    expected: info.size,
                    found: info.stored_size,
                });
            }
            if blake3::hash(stored.as_slice()).as_bytes() != &info.blake3 {
                return Err(ChunkError::HashMismatch);
            }
            Ok(())
        }
        Encoding::Zstd => {
            let expected = ExpectedChunk {
                encoding: vgames_pack::Encoding::Zstd,
                stored_size: info.stored_size,
                size: info.size,
                blake3: info.blake3,
            };
            encode::decode_and_verify(&expected, stored.as_slice(), scratch)?;
            // Hand the decoded bytes over without copying; the stored buffer
            // becomes this thread's scratch.
            stored.swap(scratch);
            Ok(())
        }
    }
}

/// A tiny LRU of open files per writer thread (≤ 128 handles in total).
struct HandleCache {
    capacity: usize,
    tick: u64,
    open: HashMap<u32, (File, u64)>,
}

impl HandleCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            tick: 0,
            open: HashMap::new(),
        }
    }

    fn get(&mut self, file: u32, path: &Path) -> std::io::Result<&File> {
        self.tick += 1;
        let tick = self.tick;
        if !self.open.contains_key(&file) {
            if self.open.len() >= self.capacity
                && let Some(oldest) = self
                    .open
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(k, _)| *k)
            {
                self.open.remove(&oldest);
            }
            let handle = fsutil::open_for_write(path)?;
            self.open.insert(file, (handle, tick));
        }
        match self.open.get_mut(&file) {
            Some((handle, used)) => {
                *used = tick;
                Ok(handle)
            }
            None => Err(std::io::Error::other("file handle cache is inconsistent")),
        }
    }

    fn forget(&mut self, file: u32) {
        self.open.remove(&file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_cache_evicts_the_least_recently_used() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..4)
            .map(|i| {
                let p = dir.path().join(format!("f{i}"));
                std::fs::write(&p, b"").unwrap();
                p
            })
            .collect();
        let mut cache = HandleCache::new(2);
        cache.get(0, &paths[0]).unwrap();
        cache.get(1, &paths[1]).unwrap();
        cache.get(0, &paths[0]).unwrap();
        cache.get(2, &paths[2]).unwrap();
        assert_eq!(cache.open.len(), 2);
        assert!(cache.open.contains_key(&0) && cache.open.contains_key(&2));
    }
}
