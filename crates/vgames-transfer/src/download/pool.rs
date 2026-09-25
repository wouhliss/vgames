//! The chunk buffer pool (02 §7.7): at most `count` buffers, each large enough
//! for one stored chunk. Network workers wait for a free buffer before reading
//! more bytes, so memory stays bounded whatever the package size, and the
//! steady state allocates nothing (buffers are reused).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct BufferPool {
    permits: Arc<Semaphore>,
    free: Mutex<Vec<Vec<u8>>>,
    buffer_capacity: usize,
    allocated: AtomicUsize,
}

impl BufferPool {
    pub fn new(count: usize, buffer_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            permits: Arc::new(Semaphore::new(count)),
            free: Mutex::new(Vec::with_capacity(count)),
            buffer_capacity,
            allocated: AtomicUsize::new(0),
        })
    }

    /// Waits for a free buffer (empty, with at least `buffer_capacity` bytes of capacity).
    pub async fn acquire(self: &Arc<Self>) -> PooledBuf {
        // The semaphore is never closed, so this only fails if that changes.
        let permit = self.permits.clone().acquire_owned().await.ok();
        let reused = self.free.lock().ok().and_then(|mut free| free.pop());
        let buf = reused.unwrap_or_else(|| {
            self.allocated.fetch_add(1, Ordering::Relaxed);
            Vec::with_capacity(self.buffer_capacity)
        });
        PooledBuf {
            buf,
            pool: Arc::clone(self),
            _permit: permit,
        }
    }

    /// Buffers allocated so far (never more than the pool size).
    pub fn allocated(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    pub fn buffer_capacity(&self) -> usize {
        self.buffer_capacity
    }
}

/// A buffer on loan from the pool; it goes back when dropped.
pub struct PooledBuf {
    buf: Vec<u8>,
    pool: Arc<BufferPool>,
    // Released after the buffer is back in the free list (field drop order).
    _permit: Option<OwnedSemaphorePermit>,
}

impl PooledBuf {
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn extend_from_slice(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Exchanges contents with `other` (used to hand a decoded chunk to the
    /// writer without copying; `other` must have the pool's capacity).
    pub fn swap(&mut self, other: &mut Vec<u8>) {
        std::mem::swap(&mut self.buf, other);
    }
}

impl Drop for PooledBuf {
    fn drop(&mut self) {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        if buf.capacity() >= self.pool.buffer_capacity
            && let Ok(mut free) = self.pool.free.lock()
        {
            free.push(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn buffers_are_bounded_and_reused() {
        let pool = BufferPool::new(2, 1024);
        let mut a = pool.acquire().await;
        a.extend_from_slice(b"abc");
        let b = pool.acquire().await;
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(50), pool.acquire()).await;
        assert!(blocked.is_err(), "a third buffer must wait");
        drop(a);
        let c = pool.acquire().await;
        assert!(c.is_empty(), "returned buffers are cleared");
        drop((b, c));
        for _ in 0..100 {
            let _x = pool.acquire().await;
        }
        assert_eq!(pool.allocated(), 2);
    }
}
