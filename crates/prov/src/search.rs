//! Search over names, front metadata and provenance (ROADMAP P5).
//!
//! An in-memory index over:
//! - **names** (from the ref store): exact and prefix lookups;
//! - **metadata**: the `meta` of every provenance record, indexed under the
//!   record's subjects. Fronts attach searchable metadata to an object as a
//!   record with `op = "meta"` and input `subject` ([`attach_meta`]); build,
//!   attest and promote records are searchable the same way;
//! - **full text**: lowercase alphanumeric tokens of names and metadata
//!   values. A query matches objects that have every query token.
//!
//! **Consistency.** The provenance log is append-only, so [`SearchIndex::refresh`]
//! indexes only records added since the last refresh (walking log nodes back
//! to the last indexed head), and re-reads the name table, which the ref store
//! holds in memory. A query after `refresh` reflects the current ref log.

use crate::{LogNode, Record};
use pico2peta_core::{Hash, Name, Repository, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Provenance op for plain metadata about an object.
pub const OP_META: &str = "meta";

/// Records searchable metadata (`key → value`) about `subject`.
pub fn attach_meta(
    repo: &dyn Repository,
    log_ref: &Name,
    subject: Hash,
    meta: BTreeMap<String, String>,
) -> Result<Hash> {
    let rec = Record {
        op: OP_META.into(),
        inputs: [("subject".to_string(), subject)].into_iter().collect(),
        meta,
        ..Default::default()
    };
    Ok(crate::record(repo, log_ref, &[rec])?.into_iter().next().unwrap_or(subject))
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The object.
    pub hash: Hash,
    /// Names currently pointing at it (may be empty).
    pub names: Vec<String>,
}

/// Lowercase alphanumeric tokens (other characters separate tokens).
pub fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

/// The search index (see the module docs).
#[derive(Default)]
pub struct SearchIndex {
    names: BTreeMap<String, Hash>,
    by_hash: HashMap<Hash, Vec<String>>,
    /// token → objects (from metadata).
    meta_tokens: HashMap<String, BTreeSet<Hash>>,
    /// token → objects (from the names pointing at them).
    name_tokens: HashMap<String, BTreeSet<Hash>>,
    /// object → metadata `(op, key, value)` seen for it.
    meta: HashMap<Hash, Vec<(String, String, String)>>,
    /// Provenance log head indexed so far.
    head: Option<Hash>,
    /// Records indexed (for statistics).
    records: u64,
}

impl SearchIndex {
    /// Builds the index for the log at `log_ref`.
    pub fn build(repo: &dyn Repository, log_ref: &Name) -> Result<SearchIndex> {
        let mut s = SearchIndex::default();
        s.refresh(repo, log_ref)?;
        Ok(s)
    }

    /// Catches up with the ref store and the provenance log (incremental).
    pub fn refresh(&mut self, repo: &dyn Repository, log_ref: &Name) -> Result<()> {
        // Names: the ref store's in-memory table.
        self.names.clear();
        self.by_hash.clear();
        self.name_tokens.clear();
        for (n, v) in repo.list_refs("")? {
            for t in tokens(n.as_str()) {
                self.name_tokens.entry(t).or_default().insert(v.hash);
            }
            self.by_hash.entry(v.hash).or_default().push(n.to_string());
            self.names.insert(n.to_string(), v.hash);
        }
        // New provenance records: walk back from the head to the last indexed head.
        let head = repo.resolve(log_ref)?.map(|v| v.hash);
        if head == self.head {
            return Ok(());
        }
        let mut new_records: Vec<Hash> = Vec::new();
        let mut next = head;
        let mut seen = HashSet::new();
        while let Some(h) = next {
            if Some(h) == self.head || !seen.insert(h) {
                break;
            }
            let node = LogNode::decode(&repo.get(&h, None)?)?;
            new_records.extend(node.records.iter().rev().copied());
            next = node.prev;
        }
        new_records.reverse();
        let mut done = HashSet::new();
        for rh in new_records {
            if !done.insert(rh) {
                continue;
            }
            let rec = Record::decode(&repo.get(&rh, None)?)?;
            self.index_record(&rec);
            self.records += 1;
        }
        self.head = head;
        Ok(())
    }

    fn index_record(&mut self, rec: &Record) {
        // Metadata describes the record's subjects: its inputs for meta /
        // attest / promote records, its outputs for derivations (a build's
        // metadata describes what it built).
        let subjects: Vec<Hash> = if rec.outputs.is_empty() {
            rec.inputs.values().copied().collect()
        } else {
            rec.outputs.values().copied().collect()
        };
        for s in subjects {
            let mut toks: Vec<String> = tokens(&rec.op);
            for (k, v) in &rec.meta {
                toks.extend(tokens(v));
                self.meta.entry(s).or_default().push((rec.op.clone(), k.clone(), v.clone()));
            }
            for t in toks {
                self.meta_tokens.entry(t).or_default().insert(s);
            }
        }
    }

    /// The object `name` points at.
    pub fn exact(&self, name: &str) -> Option<Hash> {
        self.names.get(name).copied()
    }

    /// Names starting with `prefix`, sorted, with their objects.
    pub fn prefix(&self, prefix: &str) -> Vec<(String, Hash)> {
        self.names
            .range(prefix.to_string()..)
            .take_while(|(n, _)| n.starts_with(prefix))
            .map(|(n, h)| (n.clone(), *h))
            .collect()
    }

    /// Objects matching every token of `query` (in names or metadata).
    pub fn text(&self, query: &str) -> Vec<Hit> {
        let q = tokens(query);
        if q.is_empty() {
            return Vec::new();
        }
        let mut result: Option<BTreeSet<Hash>> = None;
        for t in &q {
            let mut m: BTreeSet<Hash> = self.meta_tokens.get(t).cloned().unwrap_or_default();
            if let Some(ns) = self.name_tokens.get(t) {
                m.extend(ns.iter().copied());
            }
            result = Some(match result {
                None => m,
                Some(r) => r.intersection(&m).copied().collect(),
            });
        }
        result
            .unwrap_or_default()
            .into_iter()
            .map(|h| Hit { hash: h, names: self.by_hash.get(&h).cloned().unwrap_or_default() })
            .collect()
    }

    /// Metadata `(op, key, value)` recorded about `h`.
    pub fn metadata(&self, h: &Hash) -> &[(String, String, String)] {
        self.meta.get(h).map_or(&[], |v| v.as_slice())
    }

    /// Provenance records indexed.
    pub fn records(&self) -> u64 {
        self.records
    }
}
