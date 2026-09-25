//! Optional bandwidth cap shared by every connection (02 §7.12): a token
//! bucket refilled at the configured rate, with a quarter-second burst.
//! Callers take tokens *after* receiving bytes and sleep off any debt, so the
//! average rate converges on the limit.

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Throttle {
    state: Mutex<Bucket>,
}

struct Bucket {
    /// Bytes per second; `None` = unlimited.
    rate: Option<u64>,
    tokens: f64,
    last: Instant,
}

impl Throttle {
    pub fn new(rate: Option<u64>) -> Self {
        Self {
            state: Mutex::new(Bucket {
                rate: rate.filter(|r| *r > 0),
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn set_rate(&self, rate: Option<u64>) {
        if let Ok(mut b) = self.state.lock() {
            b.rate = rate.filter(|r| *r > 0);
            b.tokens = 0.0;
            b.last = Instant::now();
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.state.lock().ok().and_then(|b| b.rate)
    }

    /// Accounts for `bytes` and returns how long to wait before continuing.
    pub fn take(&self, bytes: u64) -> Duration {
        let Ok(mut b) = self.state.lock() else {
            return Duration::ZERO;
        };
        let Some(rate) = b.rate else {
            return Duration::ZERO;
        };
        let now = Instant::now();
        let rate = rate as f64;
        let burst = (rate / 4.0).max(64.0 * 1024.0);
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.last = now;
        b.tokens = (b.tokens + elapsed * rate).min(burst) - bytes as f64;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate)
        }
    }

    /// [`Self::take`], then sleeps.
    pub async fn consume(&self, bytes: u64) {
        let wait = self.take(bytes);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_waits() {
        let t = Throttle::new(None);
        assert_eq!(t.take(1 << 30), Duration::ZERO);
    }

    #[test]
    fn debt_turns_into_waiting_time() {
        let t = Throttle::new(Some(1_000_000));
        // 1 MB at 1 MB/s with a 250 KB burst: about 0.75-1 s of debt.
        let wait = t.take(1_000_000);
        assert!(
            wait > Duration::from_millis(700) && wait <= Duration::from_secs(1),
            "{wait:?}"
        );
        t.set_rate(None);
        assert_eq!(t.take(1_000_000), Duration::ZERO);
    }

    #[tokio::test]
    async fn average_rate_follows_the_limit() {
        let t = Throttle::new(Some(4_000_000));
        let start = Instant::now();
        for _ in 0..20 {
            t.consume(100_000).await;
        }
        // 2 MB at 4 MB/s ≈ 0.5 s, minus the burst.
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
    }
}
