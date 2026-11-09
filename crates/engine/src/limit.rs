//! Speed limits: a token bucket that many connections share.
//!
//! The manager owns one global bucket and every job owns one more; each
//! connection passes received bytes through both at once (see
//! [`acquire_all`] and `job::Job::pace`), so the stricter limit wins.
//!
//! The bucket works with *debt*: taking more bytes than are available is
//! allowed, the balance goes negative, and the caller sleeps until the debt
//! has been refilled. So a 64 KiB chunk at 64 KiB/s waits one second, a
//! 16 KiB chunk a quarter of that, and the long-run rate is exact no matter
//! how big the chunks are. Waiting happens in short slices that re-read the
//! rate, so changing the limit takes effect immediately, even for a
//! connection that is already waiting.
//!
//! Unused allowance is kept for 250 ms at most: enough to absorb late
//! wake-ups (timers on a busy system), too little for a noticeable burst.
//! Bytes that turn out to be useless (past the end of a segment) are
//! refunded, so the limit applies to what actually ends up in the file.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Lowest accepted limit. Below this a single network read could make a
/// connection wait so long that the server or our read timeout gives up.
pub const MIN_RATE: u64 = 4 * 1024;
/// Unused allowance is kept for at most this long, so an idle period isn't
/// followed by a big burst.
const BURST: Duration = Duration::from_millis(250);
/// Re-check the rate at least this often while waiting.
const WAIT_SLICE: Duration = Duration::from_millis(50);

pub struct RateLimiter {
    /// Bytes per second; 0 = unlimited.
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    /// Available bytes; negative = debt that must be refilled first.
    tokens: f64,
    last: Instant,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RateLimiter {
    pub fn new(rate: Option<u64>) -> Self {
        let l = Self {
            rate: AtomicU64::new(0),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        };
        l.set_rate(rate);
        l
    }

    /// `None` (or 0) removes the limit.
    pub fn set_rate(&self, rate: Option<u64>) {
        let rate = rate.filter(|&r| r > 0).map_or(0, |r| r.max(MIN_RATE));
        let mut b = self.bucket.lock();
        Self::refill(&mut b, self.rate.load(Relaxed));
        if rate == 0 {
            b.tokens = 0.0;
        }
        self.rate.store(rate, Relaxed);
    }

    /// Whether a limit is set.
    pub fn is_limited(&self) -> bool {
        self.rate.load(Relaxed) > 0
    }

    #[cfg(test)]
    pub fn rate(&self) -> Option<u64> {
        Some(self.rate.load(Relaxed)).filter(|&r| r > 0)
    }

    fn refill(b: &mut Bucket, rate: u64) {
        let now = Instant::now();
        if rate > 0 {
            let dt = now.duration_since(b.last).as_secs_f64();
            b.tokens = (b.tokens + rate as f64 * dt).min(Self::cap(rate));
        }
        b.last = now;
    }

    /// Take `n` bytes from the bucket. Returns how long the caller must wait
    /// before the bucket is out of debt (zero if unlimited or not in debt).
    pub fn charge(&self, n: u64) -> Duration {
        let rate = self.rate.load(Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        let mut b = self.bucket.lock();
        Self::refill(&mut b, rate);
        b.tokens -= n as f64;
        Self::debt_wait(&b, rate)
    }

    /// Give back `n` bytes that were charged but turned out to be useless
    /// (e.g. data past the end of a segment). Never raises the balance
    /// above the usual burst allowance.
    pub fn refund(&self, n: u64) {
        let rate = self.rate.load(Relaxed);
        if rate == 0 {
            return;
        }
        let mut b = self.bucket.lock();
        Self::refill(&mut b, rate);
        let cap = Self::cap(rate);
        b.tokens = (b.tokens + n as f64).min(cap.max(b.tokens));
    }

    fn cap(rate: u64) -> f64 {
        (rate as f64 * BURST.as_secs_f64()).max(16.0 * 1024.0)
    }

    /// Time until the current debt is paid off, at the current rate.
    pub fn pending(&self) -> Duration {
        let rate = self.rate.load(Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        let mut b = self.bucket.lock();
        Self::refill(&mut b, rate);
        Self::debt_wait(&b, rate)
    }

    fn debt_wait(b: &Bucket, rate: u64) -> Duration {
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate as f64)
        }
    }

    /// Charge `n` bytes and sleep until they are allowed. Returns the time
    /// spent waiting.
    #[cfg(test)]
    pub async fn acquire(&self, n: u64) -> Duration {
        acquire_all(&[self], n).await
    }
}

/// Charge `n` bytes to every limiter at once and sleep until all of them
/// allow it (so the strictest one wins; the waits overlap instead of adding
/// up). Returns the time spent waiting.
pub async fn acquire_all(limits: &[&RateLimiter], n: u64) -> Duration {
    let longest = |f: &dyn Fn(&RateLimiter) -> Duration| {
        limits.iter().map(|l| f(l)).max().unwrap_or_default()
    };
    let mut wait = longest(&|l| l.charge(n));
    if wait.is_zero() {
        return Duration::ZERO;
    }
    let start = Instant::now();
    while !wait.is_zero() {
        tokio::time::sleep(wait.min(WAIT_SLICE)).await;
        wait = longest(&|l| l.pending());
    }
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_waits() {
        let l = RateLimiter::new(None);
        assert_eq!(l.charge(1 << 30), Duration::ZERO);
        let l = RateLimiter::new(Some(0));
        assert_eq!(l.rate(), None);
        assert_eq!(l.charge(1 << 30), Duration::ZERO);
    }

    #[test]
    fn debt_turns_into_wait() {
        let l = RateLimiter::new(Some(1_000_000));
        // 0.5 MB at 1 MB/s is about half a second of debt.
        let w = l.charge(500_000).as_secs_f64();
        assert!((0.45..=0.51).contains(&w), "{w}");
        // More debt adds up.
        let w = l.charge(500_000).as_secs_f64();
        assert!((0.95..=1.01).contains(&w), "{w}");
    }

    #[test]
    fn raising_the_limit_shortens_the_wait() {
        let l = RateLimiter::new(Some(100_000));
        let w = l.charge(100_000).as_secs_f64();
        assert!(w > 0.9, "{w}");
        l.set_rate(Some(1_000_000));
        assert!(l.pending().as_secs_f64() < 0.11);
        l.set_rate(None);
        assert_eq!(l.pending(), Duration::ZERO);
    }

    #[test]
    fn refunds_pay_off_debt_but_dont_bank_credit() {
        let l = RateLimiter::new(Some(1_000_000));
        l.charge(500_000);
        l.refund(400_000);
        let w = l.pending().as_secs_f64();
        assert!((0.09..=0.101).contains(&w), "{w}");
        // A large refund leaves at most the burst allowance (250 ms).
        l.refund(10_000_000);
        assert_eq!(l.charge(240_000), Duration::ZERO);
        assert!(l.charge(20_000) > Duration::ZERO);
    }

    #[test]
    fn tiny_limits_are_raised_to_the_minimum() {
        let l = RateLimiter::new(Some(10));
        assert_eq!(l.rate(), Some(MIN_RATE));
    }

    #[tokio::test]
    async fn acquire_paces_to_the_rate() {
        let l = std::sync::Arc::new(RateLimiter::new(Some(2_000_000)));
        let start = Instant::now();
        let mut tasks = vec![];
        for _ in 0..4 {
            let l = l.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..25 {
                    l.acquire(10_000).await;
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        // 1 MB at 2 MB/s, minus the small initial allowance.
        let secs = start.elapsed().as_secs_f64();
        assert!((0.42..0.65).contains(&secs), "{secs}");
    }
}
