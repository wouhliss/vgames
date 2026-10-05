//! Network workers: one range request at a time over its own HTTP/1.1
//! connection, strict `206` + `Content-Range` checks, stall timeout, pooled
//! buffers, and the retry rules of 02 §7.10.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use super::pool::BufferPool;
use super::scheduler::{Next, Scheduler, Stop, Task};
use super::table::{ChunkRange, ChunkTable};
use super::throttle::Throttle;
use super::writer::{ChunkJob, Stats};
use super::{DownloadError, PackUrlSource, RemoteError};
use crate::http::redact_url;

/// Signed URLs expiring sooner than this are refreshed before use.
const URL_MARGIN: time::Duration = time::Duration::seconds(60);
/// Consecutive rejections of fresh URLs for one pack before giving up.
const MAX_URL_REJECTIONS: u32 = 4;
/// Largest batch for `POST …/download-urls`.
const URL_BATCH: usize = 500;

/// Retry and timeout settings.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub stall_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Transient failures in a row (without progress) before a range fails.
    pub max_attempts: u32,
}

/// Signed pack URLs, refreshed on expiry or rejection (single flight).
pub struct UrlCache {
    urls: Mutex<HashMap<u32, (Arc<str>, OffsetDateTime)>>,
    refresh: tokio::sync::Mutex<()>,
    /// Per pack: cached URL generations refused in a row, and the last one.
    /// A refresh may legitimately return the same URL text.
    rejections: Mutex<HashMap<u32, (u32, Arc<str>)>>,
}

impl UrlCache {
    pub fn new() -> Self {
        Self {
            urls: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
            rejections: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, pack: u32) -> Option<Arc<str>> {
        let urls = self.urls.lock().ok()?;
        let (url, expires) = urls.get(&pack)?;
        (*expires - OffsetDateTime::now_utc() > URL_MARGIN).then(|| Arc::clone(url))
    }

    /// Fetches URLs for many packs up front, in batches.
    pub async fn prefetch<A: PackUrlSource>(
        &self,
        api: &A,
        packs: &[u32],
    ) -> Result<(), RemoteError> {
        for batch in packs.chunks(URL_BATCH) {
            self.store(api.pack_urls(batch).await?);
        }
        Ok(())
    }

    fn store(&self, items: Vec<vgames_proto::versions::PackUrl>) {
        if let Ok(mut urls) = self.urls.lock() {
            for item in items {
                urls.insert(item.pack_index, (Arc::from(item.url), item.expires_at));
            }
        }
    }

    /// A usable URL for `pack`.
    pub async fn get<A: PackUrlSource>(&self, api: &A, pack: u32) -> Result<Arc<str>, RemoteError> {
        if let Some(url) = self.cached(pack) {
            return Ok(url);
        }
        self.refresh(api, pack, None).await
    }

    /// Replaces the URL of `pack`, unless another worker already replaced
    /// `stale` in the meantime.
    pub async fn refresh<A: PackUrlSource>(
        &self,
        api: &A,
        pack: u32,
        stale: Option<&Arc<str>>,
    ) -> Result<Arc<str>, RemoteError> {
        let _single_flight = self.refresh.lock().await;
        if let Some(url) = self.cached(pack)
            && stale.is_none_or(|s| !Arc::ptr_eq(s, &url))
        {
            return Ok(url);
        }
        self.store(api.pack_urls(&[pack]).await?);
        self.urls
            .lock()
            .ok()
            .and_then(|u| u.get(&pack).map(|(url, _)| Arc::clone(url)))
            .ok_or_else(|| {
                RemoteError::fatal(format!("the server sent no download link for pack {pack}"))
            })
    }

    /// Counts each refreshed URL generation once, even when its text is
    /// unchanged. Late refusals from an older generation do not count again.
    fn rejected(&self, pack: u32, url: &Arc<str>) -> u32 {
        let Ok(urls) = self.urls.lock() else {
            return u32::MAX;
        };
        let current = urls
            .get(&pack)
            .is_some_and(|(cached, _)| Arc::ptr_eq(cached, url));
        self.rejections
            .lock()
            .map(|mut r| {
                if !current {
                    return r.get(&pack).map_or(0, |entry| entry.0);
                }
                let entry = r.entry(pack).or_insert_with(|| (1, Arc::clone(url)));
                if !Arc::ptr_eq(&entry.1, url) {
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = Arc::clone(url);
                }
                entry.0
            })
            .unwrap_or(u32::MAX)
    }

    fn accepted(&self, pack: u32) {
        if let Ok(mut r) = self.rejections.lock() {
            r.remove(&pack);
        }
    }
}

impl Default for UrlCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything a worker needs.
pub struct WorkerCtx<A> {
    pub table: Arc<ChunkTable>,
    pub scheduler: Arc<Scheduler>,
    pub pool: Arc<BufferPool>,
    pub sender: mpsc::Sender<ChunkJob>,
    pub client: reqwest::Client,
    pub fresh_client: reqwest::Client,
    pub urls: UrlCache,
    pub api: Arc<A>,
    pub throttle: Arc<Throttle>,
    pub stats: Arc<Stats>,
    pub timings: Timings,
    /// Workers alive, and how many the AIMD controller wants.
    pub active: AtomicUsize,
    pub target: AtomicUsize,
}

/// Worker loop: takes ranges until the download completes or stops, or until
/// there are more workers than the controller's target.
pub async fn worker<A: PackUrlSource>(ctx: Arc<WorkerCtx<A>>) {
    loop {
        if leave_if_above_target(&ctx.active, &ctx.target) {
            return;
        }
        match ctx.scheduler.next().await {
            Next::Task(task) => run_task(&ctx, task).await,
            Next::Done | Next::Stop => {
                ctx.active.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// Decrements `active` and returns `true` when there are more workers than
/// `target`; otherwise leaves `active` unchanged. `target` is re-read on every
/// attempt, as the AIMD controller may lower it concurrently.
fn leave_if_above_target(active: &AtomicUsize, target: &AtomicUsize) -> bool {
    let mut current = active.load(Ordering::SeqCst);
    loop {
        if current <= target.load(Ordering::SeqCst) {
            return false;
        }
        match active.compare_exchange_weak(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

enum Kind {
    /// The download is stopping: requeue silently.
    Stopped,
    /// 400/401/403: the signed URL expired or was refused.
    UrlRejected(StatusCode),
    /// Timeout, reset, 5xx, 429: back off and retry.
    Transient(String),
    /// Wrong status, `Content-Range` or length: counts as a mismatch.
    Protocol(String),
    Fatal(DownloadError),
}

struct Failure {
    /// First chunk of the range not handed to the writers.
    next: u32,
    kind: Kind,
}

fn remainder(task: &Task, next: u32) -> Option<Task> {
    let end = task.range.end();
    (next < end).then_some(Task {
        range: ChunkRange {
            first: next,
            count: end - next,
        },
        ..*task
    })
}

async fn run_task<A: PackUrlSource>(ctx: &WorkerCtx<A>, task: Task) {
    let url = match ctx.urls.get(&*ctx.api, task.pack).await {
        Ok(url) => url,
        Err(error) => return remote_failure(ctx, task, error),
    };
    let Err(failure) = fetch(ctx, &url, &task).await else {
        ctx.urls.accepted(task.pack);
        return;
    };
    let Some(rest) = remainder(&task, failure.next) else {
        return;
    };
    let scheduler = &ctx.scheduler;
    match failure.kind {
        Kind::Stopped => scheduler.requeue(rest, Duration::ZERO),
        Kind::UrlRejected(status) => {
            if ctx.urls.rejected(task.pack, &url) > MAX_URL_REJECTIONS {
                return scheduler.stop(Stop::Failed(DownloadError::LinksRefused {
                    pack: task.pack,
                    status: status.as_u16(),
                }));
            }
            tracing::debug!(pack = task.pack, %status, "download link refused; refreshing it");
            match ctx.urls.refresh(&*ctx.api, task.pack, Some(&url)).await {
                Ok(_) => scheduler.requeue(rest, Duration::ZERO),
                Err(error) => remote_failure(ctx, rest, error),
            }
        }
        Kind::Transient(reason) => {
            scheduler.congestion();
            let attempt = if failure.next > task.range.first {
                1
            } else {
                task.attempt + 1
            };
            if attempt > ctx.timings.max_attempts {
                return scheduler.stop(Stop::Failed(DownloadError::Network(reason)));
            }
            let delay =
                scheduler.backoff(attempt, ctx.timings.backoff_base, ctx.timings.backoff_max);
            tracing::debug!(pack = task.pack, attempt, ?delay, %reason, "range failed; retrying");
            scheduler.requeue(Task { attempt, ..rest }, delay);
        }
        Kind::Protocol(detail) => {
            scheduler.mismatch(&ctx.table, rest.range.first, detail);
            if rest.range.count > 1 {
                scheduler.requeue(
                    Task {
                        range: ChunkRange {
                            first: rest.range.first + 1,
                            count: rest.range.count - 1,
                        },
                        fresh: true,
                        ..rest
                    },
                    Duration::ZERO,
                );
            }
        }
        Kind::Fatal(error) => scheduler.stop(Stop::Failed(error)),
    }
}

fn remote_failure<A>(ctx: &WorkerCtx<A>, task: Task, error: RemoteError) {
    if error.retryable && task.attempt < ctx.timings.max_attempts {
        let attempt = task.attempt + 1;
        let delay =
            ctx.scheduler
                .backoff(attempt, ctx.timings.backoff_base, ctx.timings.backoff_max);
        tracing::debug!(pack = task.pack, attempt, %error, "cannot get a download link; retrying");
        ctx.scheduler.requeue(Task { attempt, ..task }, delay);
    } else {
        ctx.scheduler
            .stop(Stop::Failed(DownloadError::Remote(error)));
    }
}

async fn fetch<A>(ctx: &WorkerCtx<A>, url: &str, task: &Task) -> Result<(), Failure> {
    let first = task.range.first;
    let end_chunk = task.range.end();
    let fail = |next: u32, kind: Kind| Err(Failure { next, kind });
    let internal = |what: &str| Kind::Fatal(DownloadError::Internal(what.to_owned()));
    let Some((start, end)) = ctx.table.byte_range(first, task.range.count) else {
        return fail(first, internal("range crosses packs"));
    };
    let Some(pack_size) = ctx.table.pack_size(task.pack) else {
        return fail(first, internal("unknown pack"));
    };
    let stall = ctx.timings.stall_timeout;
    let stopped = ctx.scheduler.stopped();
    let client = if task.fresh {
        &ctx.fresh_client
    } else {
        &ctx.client
    };
    let request = client
        .get(url)
        .header(RANGE, format!("bytes={start}-{}", end - 1));

    let mut response = tokio::select! {
        biased;
        () = stopped.cancelled() => return fail(first, Kind::Stopped),
        result = tokio::time::timeout(stall, request.send()) => match result {
            Err(_) => return fail(first, Kind::Transient("no response in time".into())),
            Ok(Err(error)) => return fail(first, Kind::Transient(error.without_url().to_string())),
            Ok(Ok(response)) => response,
        },
    };

    let status = response.status();
    match status {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return fail(first, Kind::UrlRejected(status));
        }
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS => {
            return fail(first, Kind::Transient(format!("status {status}")));
        }
        s if s.is_server_error() => {
            return fail(first, Kind::Transient(format!("status {status}")));
        }
        _ => {
            return fail(
                first,
                Kind::Protocol(format!(
                    "unexpected status {status} for {}",
                    redact_url(url)
                )),
            );
        }
    }
    let expected_range = format!("bytes {start}-{}/{pack_size}", end - 1);
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok());
    if content_range != Some(expected_range.as_str()) {
        return fail(
            first,
            Kind::Protocol(format!(
                "Content-Range {:?} instead of {expected_range:?}",
                content_range.unwrap_or_default()
            )),
        );
    }
    if let Some(length) = response.content_length()
        && length != end - start
    {
        return fail(
            first,
            Kind::Protocol(format!(
                "Content-Length {length} instead of {}",
                end - start
            )),
        );
    }

    let mut next = first;
    let mut buffer = None;
    // Keep the final chunk until EOF proves the range has exactly the
    // requested length. Otherwise a trailing byte arrives after all chunks
    // have already been marked done, leaving nothing for the retry path.
    let mut final_job = None;
    loop {
        let piece = tokio::select! {
            biased;
            () = stopped.cancelled() => return fail(next, Kind::Stopped),
            result = tokio::time::timeout(stall, response.chunk()) => match result {
                Err(_) => return fail(next, Kind::Transient(format!("no data for {} s", stall.as_secs()))),
                Ok(Err(error)) => return fail(next, Kind::Transient(error.without_url().to_string())),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(piece))) => piece,
            },
        };
        ctx.stats
            .network_bytes
            .fetch_add(piece.len() as u64, Ordering::Relaxed);
        let wait = ctx.throttle.take(piece.len() as u64);
        if !wait.is_zero() {
            tokio::select! {
                biased;
                () = stopped.cancelled() => return fail(next, Kind::Stopped),
                () = tokio::time::sleep(wait) => {}
            }
        }
        let mut data: &[u8] = &piece;
        while !data.is_empty() {
            if final_job.is_some() {
                return fail(next, Kind::Protocol("more bytes than requested".into()));
            }
            let Some(info) = ctx.table.chunk(next).filter(|_| next < end_chunk) else {
                return fail(next, Kind::Protocol("more bytes than requested".into()));
            };
            let buf = match &mut buffer {
                Some(buf) => buf,
                None => {
                    let acquired = tokio::select! {
                        biased;
                        () = stopped.cancelled() => return fail(next, Kind::Stopped),
                        buf = ctx.pool.acquire() => buf,
                    };
                    buffer.insert(acquired)
                }
            };
            let want = usize::try_from(info.stored_size)
                .unwrap_or(usize::MAX)
                .saturating_sub(buf.len());
            let (head, tail) = data.split_at(want.min(data.len()));
            buf.extend_from_slice(head);
            data = tail;
            if buf.len() as u64 == info.stored_size
                && let Some(stored) = buffer.take()
            {
                let job = ChunkJob {
                    chunk: next,
                    stored,
                };
                if next + 1 == end_chunk {
                    final_job = Some(job);
                } else {
                    if ctx.sender.send(job).await.is_err() {
                        return fail(next, Kind::Stopped);
                    }
                    next += 1;
                }
            }
        }
    }
    if let Some(job) = final_job {
        if ctx.sender.send(job).await.is_err() {
            return fail(next, Kind::Stopped);
        }
        next += 1;
    }
    if next < end_chunk {
        return fail(next, Kind::Transient("the response ended early".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_counts_are_per_refresh_even_for_identical_urls() {
        let cache = UrlCache::new();
        let link = || vgames_proto::versions::PackUrl {
            pack_index: 0,
            url: "https://storage.example/pack".into(),
            size: 1,
            expires_at: OffsetDateTime::now_utc() + time::Duration::hours(6),
        };
        cache.store(vec![link()]);
        let first = cache.cached(0).unwrap();
        assert_eq!(cache.rejected(0, &first), 1);
        assert_eq!(cache.rejected(0, &first), 1);
        cache.store(vec![link()]);
        let second = cache.cached(0).unwrap();
        assert_eq!(cache.rejected(0, &second), 2);
        assert_eq!(cache.rejected(0, &first), 2);
        assert_eq!(cache.rejected(0, &second), 2);
        cache.accepted(0);
        assert_eq!(cache.rejected(0, &second), 1);
    }

    #[test]
    fn only_workers_above_the_target_leave() {
        let active = AtomicUsize::new(5);
        let target = AtomicUsize::new(3);
        assert!(leave_if_above_target(&active, &target));
        assert!(leave_if_above_target(&active, &target));
        assert_eq!(active.load(Ordering::SeqCst), 3);
        assert!(!leave_if_above_target(&active, &target));
        assert_eq!(active.load(Ordering::SeqCst), 3);

        target.store(0, Ordering::SeqCst);
        assert!(leave_if_above_target(&active, &target));
        assert_eq!(active.load(Ordering::SeqCst), 2);

        let none = AtomicUsize::new(0);
        assert!(!leave_if_above_target(&none, &target));
        assert_eq!(none.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_leavers_never_go_below_the_target() {
        let active = Arc::new(AtomicUsize::new(64));
        let target = Arc::new(AtomicUsize::new(16));
        let left: usize = (0..8)
            .map(|_| {
                let (active, target) = (Arc::clone(&active), Arc::clone(&target));
                std::thread::spawn(move || {
                    (0..64)
                        .filter(|_| leave_if_above_target(&active, &target))
                        .count()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum();
        assert_eq!(left, 48);
        assert_eq!(active.load(Ordering::SeqCst), 16);
    }
}
