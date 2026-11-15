//! Per-namespace limits (`[[limits]]`): a request rate shared by every caller of a namespace,
//! and a storage quota on the logical bytes its names reach.
//!
//! * **Rate:** one token bucket per rule, taken by every request the server authorizes against
//!   a name under the rule's prefix (core `/v1` name operations and every front). Over the rate:
//!   429 with `Retry-After`. It complements the per-principal `server.rate_limit`: that one stops
//!   a noisy client, this one protects a namespace from all of its clients together.
//! * **Quota:** a background scan (every `limits_scan_secs`) sums the sizes of the distinct
//!   objects reachable from the names under each prefix, following the fronts' references (git
//!   commits → trees → blobs, OCI manifests → layers, ...). Shared content counts in full in
//!   every namespace that references it (charged by reference, not by physical bytes after
//!   dedup). While a namespace's last scan is at or over its quota, writes into it get 507
//!   `quota_exceeded`. Reads, ref deletes through `/v1`, and admins (for cleanup) are not
//!   blocked. The check is as fresh as the last scan, so a namespace can overshoot by what it
//!   writes in one scan interval.

use crate::config::{LimitRule, RateLimit};
use pico2peta_core::{Hash, ObjectStore, Repository, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Why a request was not admitted.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// The namespace's rate is exhausted; retry after this long.
    Rate { prefix: String, wait: Duration },
    /// The namespace was at or over its quota at the last scan.
    Quota { prefix: String, used: u64, max: u64 },
}

/// The configured rules plus their buckets and last measured usage.
pub struct NamespaceLimits {
    rules: Vec<LimitRule>,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    usage: Mutex<HashMap<String, u64>>,
}

impl NamespaceLimits {
    pub fn new(rules: Vec<LimitRule>) -> NamespaceLimits {
        NamespaceLimits { rules, buckets: Mutex::new(HashMap::new()), usage: Mutex::new(HashMap::new()) }
    }

    /// The rule governing `name`: the longest prefix it is under.
    pub fn rule_for(&self, name: &str) -> Option<&LimitRule> {
        self.rules.iter().filter(|r| pico2peta_core::is_under(name, &r.prefix)).max_by_key(|r| r.prefix.len())
    }

    /// Admits one request against `name`. `write`: it may add data (quota applies);
    /// `admin`: the caller is a global admin (quota does not apply; the rate does).
    pub fn admit(&self, name: &str, write: bool, admin: bool) -> std::result::Result<(), Refusal> {
        let Some(rule) = self.rule_for(name) else { return Ok(()) };
        if write
            && !admin
            && let Some(max) = rule.max_stored_bytes
            && let Some(used) = self.usage.lock().ok().and_then(|u| u.get(&rule.prefix).copied())
            && used >= max
        {
            return Err(Refusal::Quota { prefix: rule.prefix.clone(), used, max });
        }
        if let Some(rate) = rule.rate
            && let Err(wait) = self.take(&rule.prefix, rate)
        {
            return Err(Refusal::Rate { prefix: rule.prefix.clone(), wait });
        }
        Ok(())
    }

    fn take(&self, key: &str, cfg: RateLimit) -> std::result::Result<(), Duration> {
        let Ok(mut m) = self.buckets.lock() else { return Ok(()) };
        let now = Instant::now();
        let (tokens, at) = m.entry(key.to_string()).or_insert((cfg.burst, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * cfg.per_sec).min(cfg.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / cfg.per_sec))
        }
    }

    /// Whether any rule has a quota (the scan is needed).
    pub fn has_quotas(&self) -> bool {
        self.rules.iter().any(|r| r.max_stored_bytes.is_some())
    }

    /// Last measured usage per quota prefix.
    pub fn usage(&self) -> HashMap<String, u64> {
        self.usage.lock().map(|u| u.clone()).unwrap_or_default()
    }

    /// Measures every quota prefix (blocking; see the module docs).
    pub fn scan(&self, repo: &dyn Repository, tracer: &dyn pico2peta_repo::Tracer) -> Result<()> {
        for r in self.rules.iter().filter(|r| r.max_stored_bytes.is_some()) {
            let used = usage_under(repo, tracer, &r.prefix)?;
            if let Ok(mut u) = self.usage.lock() {
                u.insert(r.prefix.clone(), used);
            }
        }
        Ok(())
    }

    /// Sets a usage figure directly (tests).
    #[cfg(test)]
    pub fn set_usage(&self, prefix: &str, used: u64) {
        self.usage.lock().unwrap().insert(prefix.to_string(), used);
    }
}

/// Logical bytes of the distinct objects reachable from the names under `prefix`.
pub fn usage_under(repo: &dyn Repository, tracer: &dyn pico2peta_repo::Tracer, prefix: &str) -> Result<u64> {
    let mut seen: HashSet<Hash> = HashSet::new();
    let mut stack: Vec<Hash> = repo.list_refs(prefix)?.into_iter().map(|(_, v)| v.hash).collect();
    let mut total = 0u64;
    while let Some(h) = stack.pop() {
        if !seen.insert(h) {
            continue;
        }
        let size = match repo.size(&h) {
            Ok(s) => s,
            Err(pico2peta_core::Error::NotFound(_)) => continue, // collected meanwhile
            Err(e) => return Err(e),
        };
        total += size;
        let store: &dyn ObjectStore = repo;
        for c in tracer.children(&h, store)? {
            if !seen.contains(&c) {
                stack.push(c);
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(prefix: &str, rate: Option<(f64, f64)>, max: Option<u64>) -> LimitRule {
        LimitRule {
            prefix: prefix.into(),
            rate: rate.map(|(per_sec, burst)| RateLimit { per_sec, burst }),
            max_stored_bytes: max,
        }
    }

    #[test]
    fn longest_prefix_rate_and_quota() {
        let l = NamespaceLimits::new(vec![rule("team", Some((0.001, 2.0)), Some(100)), rule("team/big", None, None)]);
        assert!(l.rule_for("other/x").is_none());
        assert_eq!(l.rule_for("team/big/x").unwrap().prefix, "team/big");
        assert_eq!(l.rule_for("teamx/y").map(|r| r.prefix.as_str()), None);
        // Burst of 2, then refused; a namespace with its own rule is unaffected.
        assert!(l.admit("team/a", false, false).is_ok());
        assert!(l.admit("team/b", true, false).is_ok());
        assert!(matches!(l.admit("team/c", false, false), Err(Refusal::Rate { .. })));
        assert!(l.admit("team/big/x", true, false).is_ok());
        // Quota: unknown usage admits; at the quota, writes (not reads, not admins) are refused.
        let l = NamespaceLimits::new(vec![rule("team", None, Some(100))]);
        assert!(l.admit("team/a", true, false).is_ok());
        l.set_usage("team", 100);
        assert_eq!(l.admit("team/a", true, false), Err(Refusal::Quota { prefix: "team".into(), used: 100, max: 100 }));
        assert!(l.admit("team/a", false, false).is_ok());
        assert!(l.admit("team/a", true, true).is_ok());
        l.set_usage("team", 99);
        assert!(l.admit("team/a", true, false).is_ok());
    }
}
