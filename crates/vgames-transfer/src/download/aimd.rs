//! Connection-count controller (02 §7.6): start at 6; every tick add 2 while
//! aggregate throughput rose by at least 5%, up to 32; halve on timeouts,
//! resets and 5xx (never below 2).

use std::time::Instant;

pub struct Aimd {
    target: usize,
    min: usize,
    max: usize,
    last_bytes: u64,
    last_at: Instant,
    last_rate: Option<f64>,
}

impl Aimd {
    pub fn new(initial: usize, min: usize, max: usize) -> Self {
        Self {
            target: initial.clamp(min, max),
            min,
            max,
            last_bytes: 0,
            last_at: Instant::now(),
            last_rate: None,
        }
    }

    pub fn target(&self) -> usize {
        self.target
    }

    /// One control step. `bytes` is the total received so far, `congestion`
    /// the error events since the last step. A throttled download does not
    /// grow (more connections cannot help).
    pub fn update(&mut self, bytes: u64, congestion: u64, throttled: bool, now: Instant) -> usize {
        let dt = now.duration_since(self.last_at).as_secs_f64();
        let rate = if dt > 0.0 {
            bytes.saturating_sub(self.last_bytes) as f64 / dt
        } else {
            0.0
        };
        self.last_bytes = bytes;
        self.last_at = now;
        if congestion > 0 {
            self.target = (self.target / 2).max(self.min);
        } else if !throttled {
            let rose = match self.last_rate {
                None => rate > 0.0,
                Some(previous) => rate >= previous * 1.05 && rate > 0.0,
            };
            if rose {
                self.target = (self.target + 2).min(self.max);
            }
        }
        self.last_rate = Some(rate);
        self.target
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn grows_while_throughput_rises_and_halves_on_congestion() {
        let start = Instant::now();
        let mut a = Aimd::new(6, 2, 32);
        let mut bytes = 0u64;
        let mut rate = 100_000_000u64;
        let mut t = start;
        for _ in 0..20 {
            t += Duration::from_secs(2);
            bytes += rate * 2;
            rate = rate * 11 / 10;
            a.update(bytes, 0, false, t);
        }
        assert_eq!(a.target(), 32, "capped at 32");
        t += Duration::from_secs(2);
        bytes += rate * 2;
        assert_eq!(a.update(bytes, 3, false, t), 16);
        // Flat throughput: no growth.
        t += Duration::from_secs(2);
        bytes += rate * 2;
        assert_eq!(a.update(bytes, 0, false, t), 16);
        for _ in 0..10 {
            t += Duration::from_secs(2);
            a.update(bytes, 1, false, t);
        }
        assert_eq!(a.target(), 2, "never below the minimum");
    }

    #[test]
    fn throttled_downloads_do_not_grow() {
        let start = Instant::now();
        let mut a = Aimd::new(6, 2, 32);
        assert_eq!(
            a.update(1_000_000, 0, true, start + Duration::from_secs(2)),
            6
        );
    }
}
