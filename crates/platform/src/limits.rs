//! Operator limits: repositories per organization, patches per repository and
//! commands per minute per user. Unset limits are unlimited. They are set once
//! per process (`set`), from `O5S_MAX_REPOSITORIES`, `O5S_MAX_PATCHES` and
//! `O5S_COMMANDS_PER_MINUTE` on the native server and the Worker's variables.
use crate::{Actor, Refusal};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub max_repositories: Option<usize>,
    pub max_patches: Option<usize>,
    pub commands_per_minute: Option<u32>,
}

static LIMITS: OnceLock<Limits> = OnceLock::new();

impl Limits {
    /// From environment-style lookups; unparsable values are errors.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        fn parse<T: std::str::FromStr>(name: &str, v: Option<String>) -> Result<Option<T>, String> {
            v.filter(|v| !v.is_empty())
                .map(|v| v.parse().map_err(|_| format!("{name} must be a number")))
                .transpose()
        }
        Ok(Limits {
            max_repositories: parse("O5S_MAX_REPOSITORIES", get("O5S_MAX_REPOSITORIES"))?,
            max_patches: parse("O5S_MAX_PATCHES", get("O5S_MAX_PATCHES"))?,
            commands_per_minute: parse("O5S_COMMANDS_PER_MINUTE", get("O5S_COMMANDS_PER_MINUTE"))?,
        })
    }
}

/// Install this process's limits; later calls keep the first.
pub fn set(limits: Limits) {
    let _ = LIMITS.set(limits);
}

pub fn get() -> Limits {
    LIMITS.get().copied().unwrap_or_default()
}

/// Refuse creating one more repository when the organization has `count`.
pub fn check_repositories(count: usize) -> Result<(), Refusal> {
    match get().max_repositories {
        Some(max) if count >= max => Err(Refusal(format!(
            "repository quota reached ({max} per organization)"
        ))),
        _ => Ok(()),
    }
}

/// Refuse a new proposal in a repository that holds `count` patches.
pub fn check_patches(count: usize) -> Result<(), Refusal> {
    match get().max_patches {
        Some(max) if count >= max => Err(Refusal(format!(
            "patch quota reached ({max} per repository)"
        ))),
        _ => Ok(()),
    }
}

/// A token bucket per user: `per_minute` commands, refilled continuously,
/// with bursts of up to a minute's worth.
#[derive(Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<(u64, u64), (f64, u64)>>,
}

impl RateLimiter {
    /// Take one command for `actor` at `now_ms`; false when over the limit.
    pub fn allow(&self, actor: &Actor, now_ms: u64) -> bool {
        let Some(per_minute) = get().commands_per_minute else {
            return true;
        };
        self.allow_at(actor, now_ms, per_minute)
    }

    pub fn allow_at(&self, actor: &Actor, now_ms: u64, per_minute: u32) -> bool {
        let capacity = f64::from(per_minute);
        let mut buckets = self.buckets.lock().unwrap();
        if buckets.len() > 100_000 {
            buckets.clear();
        }
        let (tokens, last) = buckets
            .entry((actor.tenant, actor.user))
            .or_insert((capacity, now_ms));
        let refill = now_ms.saturating_sub(*last) as f64 * capacity / 60_000.0;
        *tokens = (*tokens + refill).min(capacity);
        *last = now_ms;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_refill_per_user() {
        let r = RateLimiter::default();
        let a = Actor { tenant: 1, user: 1 };
        let b = Actor { tenant: 1, user: 2 };
        assert!(r.allow_at(&a, 0, 2) && r.allow_at(&a, 0, 2));
        assert!(!r.allow_at(&a, 0, 2));
        assert!(r.allow_at(&b, 0, 2));
        // Half a minute refills one of two.
        assert!(r.allow_at(&a, 30_000, 2));
        assert!(!r.allow_at(&a, 30_000, 2));
    }

    #[test]
    fn limits_parse_from_the_environment() {
        let env = |k: &str| match k {
            "O5S_MAX_PATCHES" => Some("10".to_string()),
            "O5S_COMMANDS_PER_MINUTE" => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            Limits::from_lookup(env).unwrap(),
            Limits {
                max_repositories: None,
                max_patches: Some(10),
                commands_per_minute: None
            }
        );
        assert!(Limits::from_lookup(|_| Some("x".into())).is_err());
    }
}
