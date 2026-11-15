//! Retention per namespace profile (ROADMAP P5).
//!
//! `[profiles.<p>] keep_versions = N` keeps the newest N versions of every
//! series under the profile's namespaces. A series is the set of names that
//! share a parent (`pypi/proj/1.0`, `pypi/proj/1.1` → series `pypi/proj`), and
//! versions are ordered by the last segment, comparing digit runs numerically
//! (`1.10` > `1.9`). Older names are deleted with OCC (`delete_ref`), and GC
//! then collects what nothing else references. Name rules still apply: an
//! immutable ("published forever") name refuses deletion and is reported as
//! kept. With `only_if_regenerable = true`, an old version is deleted only if
//! provenance can regenerate its object from inputs that are still stored.

use crate::Provenance;
use pico2peta_core::{Name, ReplaceOutcome, Repository, Result};
use pico2peta_repo::LocalRepo;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// What [`apply`] did (or would do, with `dry_run`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Names deleted (or that would be).
    pub deleted: Vec<String>,
    /// Old names kept because a rule forbids deleting them.
    pub kept_by_rule: Vec<String>,
    /// Old names kept because their object isn't regenerable.
    pub kept_not_regenerable: Vec<String>,
    /// Names whose delete lost an OCC race (they moved concurrently; retried on the next pass).
    pub conflicts: Vec<String>,
}

/// Version-aware comparison: digit runs compare numerically, the rest as text.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let split = |s: &str| -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let d = c.is_ascii_digit();
            match out.last_mut() {
                Some((kd, run)) if *kd == d => run.push(c),
                _ => out.push((d, c.to_string())),
            }
        }
        out
    };
    let (xa, xb) = (split(a), split(b));
    for (p, q) in xa.iter().zip(&xb) {
        let o = match (p.0, q.0) {
            (true, true) => {
                let (pa, qa) = (p.1.trim_start_matches('0'), q.1.trim_start_matches('0'));
                pa.len().cmp(&qa.len()).then_with(|| pa.cmp(qa))
            }
            _ => p.1.cmp(&q.1),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    xa.len().cmp(&xb.len()).then_with(|| a.cmp(b))
}

/// Applies every profile's retention to its namespaces.
pub fn apply(repo: &LocalRepo, log_ref: &Name, dry_run: bool) -> Result<RetentionReport> {
    let mut report = RetentionReport::default();
    let mut prov: Option<Provenance<'_>> = None;
    let rules = repo.rules()?;
    for (prefix, _, profile) in repo.namespace_profiles() {
        let Some(keep) = profile.keep_versions else { continue };
        // The profile that actually governs each name (longest prefix wins).
        let mut series: BTreeMap<String, Vec<(String, pico2peta_core::RefValue)>> = BTreeMap::new();
        // `list_refs` matches segment-aligned prefixes ("pypi" covers "pypi/a/1"),
        // while [namespaces] keys are string prefixes ("pypi/"); `profile_for`
        // below keeps only names this profile really governs.
        let listing = prefix.trim_end_matches('/');
        for (n, v) in repo.list_refs(listing)? {
            let s = n.as_str();
            if repo.profile_for(s) != profile {
                continue;
            }
            let Some((parent, _)) = s.rsplit_once('/') else { continue };
            series.entry(parent.to_string()).or_default().push((s.to_string(), v));
        }
        for (_, mut names) in series {
            names.sort_by(|a, b| version_cmp(last(&a.0), last(&b.0)));
            let old = names.len().saturating_sub(keep);
            for (name, v) in names.into_iter().take(old) {
                if profile.only_if_regenerable {
                    if prov.is_none() {
                        prov = Some(Provenance::load(repo, log_ref)?);
                    }
                    let p = prov.as_ref().ok_or_else(|| pico2peta_core::Error::Corrupt("provenance".into()))?;
                    if p.regenerable(repo, &v.hash)?.is_none() {
                        report.kept_not_regenerable.push(name);
                        continue;
                    }
                }
                let nm = Name::new(name.clone())?;
                if !pico2peta_refs::RefState::rules_allow_delete(&rules, &nm) {
                    report.kept_by_rule.push(name);
                    continue;
                }
                if dry_run {
                    report.deleted.push(name);
                    continue;
                }
                match repo.delete_ref(&nm, &[(nm.clone(), pico2peta_core::Observed::Version(v.version))]) {
                    Ok(ReplaceOutcome::Ok { .. }) => report.deleted.push(name),
                    Ok(ReplaceOutcome::Conflict { .. }) => report.conflicts.push(name),
                    Err(pico2peta_core::Error::Forbidden { .. }) | Err(pico2peta_core::Error::Unauthorized(_)) => {
                        report.kept_by_rule.push(name)
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(report)
}

fn last(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}
