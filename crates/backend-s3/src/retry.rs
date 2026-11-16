//! Retry policy: capped exponential backoff with full jitter and a shared retry budget.

use crate::config::RetryConfig;
use crate::error::S3Error;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

/// Shared between all requests of one client.
pub struct RetryBudget {
    tokens: AtomicI64,
    max: i64,
}

impl RetryBudget {
    pub fn new(max: u32) -> Self {
        RetryBudget { tokens: AtomicI64::new(max as i64), max: max as i64 }
    }

    /// Takes `cost` tokens; `false` (and nothing taken) if not enough remain.
    pub fn try_acquire(&self, cost: u32) -> bool {
        let cost = cost as i64;
        self.tokens.try_update(Ordering::AcqRel, Ordering::Acquire, |t| (t >= cost).then_some(t - cost)).is_ok()
    }

    /// Refunds one token after a success (up to the maximum).
    pub fn on_success(&self) {
        let _ = self.tokens.try_update(Ordering::AcqRel, Ordering::Acquire, |t| (t < self.max).then_some(t + 1));
    }

    #[cfg(test)]
    pub fn available(&self) -> i64 {
        self.tokens.load(Ordering::Acquire)
    }
}

/// Delay before retry number `retry` (0-based) after error `err`.
pub fn backoff(cfg: &RetryConfig, retry: u32, err: &S3Error) -> Duration {
    let base = if err.is_throttle() { cfg.throttle_base_delay } else { cfg.base_delay };
    let exp = base.saturating_mul(1u32.checked_shl(retry.min(31)).unwrap_or(u32::MAX));
    let cap = exp.min(cfg.max_delay);
    // Full jitter: uniform in [0, cap].
    let nanos = cap.as_nanos().min(u64::MAX as u128) as u64;
    Duration::from_nanos(if nanos == 0 { 0 } else { fastrand::u64(0..=nanos) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_capped_and_jittered() {
        let cfg = RetryConfig::default();
        let e = S3Error::Transport("x".into());
        for r in 0..100 {
            assert!(backoff(&cfg, r, &e) <= cfg.max_delay);
        }
        let samples: std::collections::HashSet<_> = (0..50).map(|_| backoff(&cfg, 5, &e)).collect();
        assert!(samples.len() > 10, "jitter should spread delays");
        assert!(backoff(&cfg, 0, &e) <= cfg.base_delay);
    }

    #[test]
    fn throttles_back_off_harder() {
        let cfg = RetryConfig { max_delay: Duration::from_secs(1000), ..Default::default() };
        let t = S3Error::Http {
            method: "PUT",
            key: "k".into(),
            status: 503,
            code: "SlowDown".into(),
            message: String::new(),
        };
        let max_t = (0..200).map(|_| backoff(&cfg, 0, &t)).max().unwrap();
        assert!(max_t > cfg.base_delay);
        assert!(max_t <= cfg.throttle_base_delay);
    }

    #[test]
    fn budget() {
        let b = RetryBudget::new(10);
        assert!(b.try_acquire(5));
        assert!(b.try_acquire(5));
        assert!(!b.try_acquire(5));
        b.on_success();
        assert_eq!(b.available(), 1);
        for _ in 0..100 {
            b.on_success();
        }
        assert_eq!(b.available(), 10);
    }
}
