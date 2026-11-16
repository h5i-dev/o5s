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

/// Version-aware comparison: digit runs compare numerically, the rest as text
/// (the extracted kernel's `retention::version_cmp`).
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    match pico2peta_kernel::retention::version_cmp(&a.as_bytes().to_vec(), &b.as_bytes().to_vec()) {
        c if c < 0 => Ordering::Less,
        0 => Ordering::Equal,
        _ => Ordering::Greater,
    }
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
            // Regenerability only matters for the old ones; the kernel ignores it otherwise.
            let old = names.len().saturating_sub(keep);
            let mut entries = Vec::with_capacity(names.len());
            for (k, (name, v)) in names.iter().enumerate() {
                let regenerable = if profile.only_if_regenerable && k < old {
                    if prov.is_none() {
                        prov = Some(Provenance::load(repo, log_ref)?);
                    }
                    let p = prov.as_ref().ok_or_else(|| pico2peta_core::Error::Corrupt("provenance".into()))?;
                    p.regenerable(repo, &v.hash)?.is_some()
                } else {
                    true
                };
                let nm = Name::new(name.clone())?;
                let covering = rules
                    .iter()
                    .filter(|(pfx, _)| nm.is_under(pfx))
                    .map(|(pfx, r)| (pfx.as_bytes().to_vec(), kernel_rule(*r)))
                    .collect();
                entries.push(pico2peta_kernel::retention::Entry {
                    name: name.as_bytes().to_vec(),
                    version: last(name).as_bytes().to_vec(),
                    regenerable,
                    rules: covering,
                });
            }
            // The decision is the extracted kernel's (`retention::plan`).
            let actions = pico2peta_kernel::retention::plan(keep, profile.only_if_regenerable, &entries)
                .map_err(|_| pico2peta_core::Error::Corrupt("retention: series not sorted".into()))?;
            for ((name, v), action) in names.into_iter().zip(actions) {
                use pico2peta_kernel::retention::Action;
                match action {
                    Action::Keep => {}
                    Action::KeepNotRegenerable => report.kept_not_regenerable.push(name),
                    Action::KeepByRule => report.kept_by_rule.push(name),
                    Action::Delete if dry_run => report.deleted.push(name),
                    Action::Delete => {
                        let nm = Name::new(name.clone())?;
                        match repo.delete_ref(&nm, &[(nm.clone(), pico2peta_core::Observed::Version(v.version))]) {
                            Ok(ReplaceOutcome::Ok { .. }) => report.deleted.push(name),
                            Ok(ReplaceOutcome::Conflict { .. }) => report.conflicts.push(name),
                            Err(pico2peta_core::Error::Forbidden { .. })
                            | Err(pico2peta_core::Error::Unauthorized(_)) => report.kept_by_rule.push(name),
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
    }
    Ok(report)
}

fn kernel_rule(r: pico2peta_core::Rule) -> pico2peta_kernel::Rule {
    match r {
        pico2peta_core::Rule::Mutable => pico2peta_kernel::Rule::Mutable,
        pico2peta_core::Rule::ImmutableAfterPublish => pico2peta_kernel::Rule::Immutable,
        pico2peta_core::Rule::HeadMutableReleaseImmutable => pico2peta_kernel::Rule::HeadRelease,
    }
}

fn last(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}
