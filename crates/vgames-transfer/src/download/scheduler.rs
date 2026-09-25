//! Work queue shared by the network workers and the writer threads: ranges
//! still to fetch, retries with backoff, per-chunk mismatch counts, and the
//! reason the download stopped (02 §7.10).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::table::{ChunkRange, ChunkTable};
use super::{DownloadError, PauseReason};

/// A range to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Task {
    pub range: ChunkRange,
    pub pack: u32,
    /// Consecutive transient failures of these chunks (drives the backoff).
    pub attempt: u32,
    /// Use a new connection (retry after a hash or protocol mismatch).
    pub fresh: bool,
}

/// Why the download stopped before completing.
#[derive(Debug)]
pub enum Stop {
    Paused(PauseReason),
    Cancelled,
    Failed(DownloadError),
}

pub enum Next {
    Task(Task),
    /// Every chunk is written.
    Done,
    /// Paused, cancelled or failed.
    Stop,
}

/// Where the scheduler's failures come from, for the integrity report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityFailure {
    pub pack: u32,
    pub chunk: u32,
    pub detail: String,
}

struct State {
    ready: VecDeque<Task>,
    delayed: Vec<(Instant, Task)>,
    /// Chunks not written yet.
    remaining: u64,
    mismatches: HashMap<u32, u8>,
    stop: Option<Stop>,
    integrity: Option<IntegrityFailure>,
}

pub struct Scheduler {
    state: Mutex<State>,
    notify: Notify,
    /// Cancelled when the download stops, to abort in-flight requests.
    stopped: CancellationToken,
    /// Timeouts, resets and 5xx since the last AIMD tick.
    congestion: AtomicU64,
}

impl Scheduler {
    pub fn new(table: &ChunkTable, ranges: Vec<ChunkRange>) -> Self {
        let remaining = ranges.iter().map(|r| u64::from(r.count)).sum();
        let ready = ranges
            .into_iter()
            .map(|range| Task {
                range,
                pack: table.chunk(range.first).map_or(0, |c| c.pack),
                attempt: 0,
                fresh: false,
            })
            .collect();
        Self {
            state: Mutex::new(State {
                ready,
                delayed: Vec::new(),
                remaining,
                mismatches: HashMap::new(),
                stop: None,
                integrity: None,
            }),
            notify: Notify::new(),
            stopped: CancellationToken::new(),
            congestion: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panicking holder cannot leave the queue half-updated in a way that
        // matters more than stopping; keep going with the data.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The next range to fetch, waiting for backoff delays and in-flight work.
    pub async fn next(&self) -> Next {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let wake_at = {
                let mut s = self.lock();
                if s.stop.is_some() {
                    return Next::Stop;
                }
                if s.remaining == 0 {
                    return Next::Done;
                }
                let now = Instant::now();
                let mut i = 0;
                while i < s.delayed.len() {
                    if s.delayed.get(i).is_some_and(|(at, _)| *at <= now) {
                        let (_, task) = s.delayed.swap_remove(i);
                        s.ready.push_front(task);
                    } else {
                        i += 1;
                    }
                }
                if let Some(task) = s.ready.pop_front() {
                    return Next::Task(task);
                }
                s.delayed.iter().map(|(at, _)| *at).min()
            };
            match wake_at {
                Some(at) => {
                    tokio::select! {
                        () = &mut notified => {}
                        () = tokio::time::sleep_until(at.into()) => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    /// True while there is (or may be) work: not stopped and not complete.
    pub fn is_running(&self) -> bool {
        let s = self.lock();
        s.stop.is_none() && s.remaining > 0
    }

    pub fn remaining(&self) -> u64 {
        self.lock().remaining
    }

    /// A token cancelled when the download stops.
    pub fn stopped(&self) -> &CancellationToken {
        &self.stopped
    }

    /// A chunk was verified and written.
    pub fn chunk_done(&self) {
        let mut s = self.lock();
        s.remaining = s.remaining.saturating_sub(1);
        if s.remaining == 0 {
            drop(s);
            self.notify.notify_waiters();
        }
    }

    /// Puts chunks back: at the front at once (`delay` zero) or after `delay`.
    pub fn requeue(&self, task: Task, delay: Duration) {
        let mut s = self.lock();
        if s.stop.is_some() {
            return;
        }
        if delay.is_zero() {
            s.ready.push_front(task);
        } else {
            s.delayed.push((Instant::now() + delay, task));
        }
        drop(s);
        self.notify.notify_one();
        // A waiter sleeping on an older deadline must see the new one.
        self.notify.notify_waiters();
    }

    /// A chunk failed verification, or its range broke the HTTP contract
    /// (02 §7.10: retry once on a fresh connection, then report and fail).
    pub fn mismatch(&self, table: &ChunkTable, chunk: u32, detail: String) {
        let pack = table.chunk(chunk).map_or(0, |c| c.pack);
        let mut s = self.lock();
        let count = s.mismatches.entry(chunk).or_insert(0);
        *count += 1;
        if *count >= 2 {
            let failure = IntegrityFailure {
                pack,
                chunk,
                detail,
            };
            s.integrity.get_or_insert(failure.clone());
            if s.stop.is_none() {
                s.stop = Some(Stop::Failed(DownloadError::Integrity {
                    pack,
                    chunk,
                    detail: failure.detail,
                }));
            }
            drop(s);
            self.wake_all();
            return;
        }
        tracing::warn!(pack, chunk, %detail, "chunk mismatch; retrying on a fresh connection");
        s.ready.push_front(Task {
            range: ChunkRange {
                first: chunk,
                count: 1,
            },
            pack,
            attempt: 0,
            fresh: true,
        });
        drop(s);
        self.notify.notify_one();
    }

    /// Mismatch count of a chunk so far.
    pub fn mismatches(&self, chunk: u32) -> u8 {
        self.lock().mismatches.get(&chunk).copied().unwrap_or(0)
    }

    /// Stops the download; the first reason wins.
    pub fn stop(&self, reason: Stop) {
        let mut s = self.lock();
        if s.stop.is_none() {
            s.stop = Some(reason);
        }
        drop(s);
        self.wake_all();
    }

    fn wake_all(&self) {
        self.stopped.cancel();
        self.notify.notify_waiters();
    }

    /// Takes the stop reason (`None` when the download completed).
    pub fn take_stop(&self) -> Option<Stop> {
        self.lock().stop.take()
    }

    pub fn integrity_failure(&self) -> Option<IntegrityFailure> {
        self.lock().integrity.clone()
    }

    /// Records a timeout, reset or 5xx for the AIMD controller.
    pub fn congestion(&self) {
        self.congestion.fetch_add(1, Ordering::Relaxed);
    }

    /// Congestion events since the last call.
    pub fn take_congestion(&self) -> u64 {
        self.congestion.swap(0, Ordering::Relaxed)
    }

    /// Exponential backoff with full jitter: uniform in
    /// `[0, min(max, base × 2^attempt)]` (02 §7.10: 0.5 s → 30 s).
    pub fn backoff(&self, attempt: u32, base: Duration, max: Duration) -> Duration {
        let cap = base
            .saturating_mul(1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX))
            .min(max);
        // OS CSPRNG (01-security §2). If it is unavailable, use the full cap.
        let Ok(random) = getrandom::u64() else {
            return cap;
        };
        let fraction = (random >> 11) as f64 / (1u64 << 53) as f64;
        cap.mul_f64(fraction)
    }
}

#[cfg(test)]
mod tests {
    use super::super::table::tests::manifest;
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn table() -> ChunkTable {
        ChunkTable::new(&manifest(&[16 * MIB], 256 * MIB)).unwrap()
    }

    #[tokio::test]
    async fn hands_out_ranges_then_done() {
        let t = table();
        let s = Scheduler::new(&t, super::super::table::plan_ranges(&t, |_| true, 8 * MIB));
        let mut got = Vec::new();
        for _ in 0..2 {
            match s.next().await {
                Next::Task(task) => got.push(task.range),
                _ => panic!("expected a task"),
            }
        }
        assert_eq!(got.len(), 2);
        for _ in 0..4 {
            s.chunk_done();
        }
        assert!(matches!(s.next().await, Next::Done));
    }

    #[tokio::test]
    async fn waits_for_delayed_retries() {
        let t = table();
        let s = Scheduler::new(&t, vec![ChunkRange { first: 0, count: 4 }]);
        let Next::Task(task) = s.next().await else {
            panic!()
        };
        s.requeue(task, Duration::from_millis(80));
        let start = Instant::now();
        assert!(matches!(s.next().await, Next::Task(_)));
        assert!(start.elapsed() >= Duration::from_millis(70));
    }

    #[tokio::test]
    async fn second_mismatch_fails_with_integrity() {
        let t = table();
        let s = Scheduler::new(&t, vec![ChunkRange { first: 0, count: 4 }]);
        let _ = s.next().await;
        s.mismatch(&t, 2, "hash".into());
        let Next::Task(retry) = s.next().await else {
            panic!()
        };
        assert!(retry.fresh);
        assert_eq!(retry.range, ChunkRange { first: 2, count: 1 });
        s.mismatch(&t, 2, "hash".into());
        assert!(matches!(s.next().await, Next::Stop));
        assert!(s.stopped().is_cancelled());
        assert_eq!(s.integrity_failure().unwrap().chunk, 2);
        assert!(matches!(
            s.take_stop(),
            Some(Stop::Failed(DownloadError::Integrity { chunk: 2, .. }))
        ));
    }

    #[test]
    fn backoff_is_bounded_and_jittered() {
        let t = table();
        let s = Scheduler::new(&t, Vec::new());
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(30);
        let samples: Vec<Duration> = (0..200).map(|_| s.backoff(10, base, max)).collect();
        assert!(samples.iter().all(|d| *d <= max));
        assert!(samples.iter().any(|d| *d > Duration::from_secs(15)));
        assert!(samples.iter().any(|d| *d < Duration::from_secs(15)));
        assert!((0..50).all(|_| s.backoff(0, base, max) <= base));
    }
}
