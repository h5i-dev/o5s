//! Fault injection for crash-consistency tests.
//!
//! Storage code calls [`Faults::hit`] before every write, fsync, rename or
//! backend call that matters for durability. A `Faults` armed with budget `n`
//! lets the first `n` hits succeed and fails every later hit with an
//! "injected crash" I/O error. The handle stays failed, which simulates a
//! process that died at that point. Tests sweep `n` over `0..total_hits`,
//! reopen the store without faults, and check the recovery invariants.
//!
//! Disarmed (the default and production case), a hit is one relaxed atomic
//! load.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Faults {
    /// Remaining successful hits. `-1` = disarmed.
    remaining: AtomicI64,
    /// Total hits so far (useful for sizing a sweep).
    hits: AtomicU64,
}

pub type FaultHandle = Arc<Faults>;

impl Faults {
    /// Never fails.
    pub fn disarmed() -> FaultHandle {
        Arc::new(Faults { remaining: AtomicI64::new(-1), hits: AtomicU64::new(0) })
    }

    /// Fails at hit number `budget` (0-based) and at every hit after it.
    pub fn crash_after(budget: u64) -> FaultHandle {
        Arc::new(Faults { remaining: AtomicI64::new(budget as i64), hits: AtomicU64::new(0) })
    }

    /// Number of hits seen.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// True once the injected crash has fired.
    pub fn crashed(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }

    /// A durability point. `point` is a static label used in the error message.
    pub fn hit(&self, point: &'static str) -> std::io::Result<()> {
        self.hits.fetch_add(1, Ordering::Relaxed);
        let mut cur = self.remaining.load(Ordering::Relaxed);
        loop {
            if cur < 0 {
                return Ok(());
            }
            if cur == 0 {
                return Err(std::io::Error::other(format!("injected crash at {point}")));
            }
            match self.remaining.compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return Ok(()),
                Err(c) => cur = c,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget() {
        let f = Faults::crash_after(2);
        assert!(f.hit("a").is_ok());
        assert!(f.hit("b").is_ok());
        assert!(f.hit("c").is_err());
        assert!(f.hit("d").is_err());
        assert!(f.crashed());
        assert_eq!(f.hits(), 4);
        let d = Faults::disarmed();
        for _ in 0..10 {
            assert!(d.hit("x").is_ok());
        }
    }
}
