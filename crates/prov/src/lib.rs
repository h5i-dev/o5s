//! Provenance layer: records of *what was made from what*.
//!
//! A [`Record`] says that running `op` (with `params`) on `inputs` produced
//! `outputs`. Records are ordinary content-addressed objects with a
//! canonical, versioned encoding (`P2PPROV1`), so the same derivation always
//! has the same hash, and recording it twice is a no-op.
//!
//! # Index
//! Records are indexed by an append-only log of [`LogNode`] objects
//! (`P2PPLOG1`), each holding a batch of record hashes and its predecessor.
//! The head is the ref `_prov/log` (configurable), advanced with OCC
//! `replace` and retried on conflict, so any number of writers can record
//! concurrently. [`Provenance::load`] builds forward and backward maps.
//!
//! # Queries
//! * [`Provenance::producers`] / [`Provenance::consumers`]: one step.
//! * [`Provenance::lineage`]: every transitive input of an object.
//! * [`Provenance::impact`]: every transitive output, which is what must be
//!   re-verified when an input turns out to be bad.
//! * [`Provenance::regenerable`]: is a deleted (or deletable) output
//!   reproducible from inputs that are still stored?
//!
//! # GC
//! [`ProvTracer`] makes provenance objects keep their **inputs and params**
//! alive, but not their outputs. With the log head as a GC root, an
//! output that nothing else references can be collected and regenerated
//! later ("old versions deletable if provenance exists").
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use pico2peta_core::{Error, Hash, Name, ObjectStore, RefValue, ReplaceOutcome, Repository, Result};
use pico2peta_repo::Tracer;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

const REC_MAGIC: &[u8; 8] = b"P2PPROV1";
const LOG_MAGIC: &[u8; 8] = b"P2PPLOG1";
const MAX_FIELD: usize = 1 << 20;
const MAX_ITEMS: usize = 1 << 16;

/// Default ref holding the provenance log head.
pub const DEFAULT_LOG_REF: &str = "_prov/log";

/// One derivation: `outputs = op(params, inputs)`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Record {
    /// Operation identifier, e.g. `"train"`, `"build"`, `"quantize@v2"`.
    pub op: String,
    /// Hash of the parameters object (config, command line, ...), if any.
    pub params: Option<Hash>,
    /// Named inputs (role → object). Roles are sorted in the encoding.
    pub inputs: BTreeMap<String, Hash>,
    /// Named outputs (role → object).
    pub outputs: BTreeMap<String, Hash>,
    /// Free-form metadata (who, when, host, ...). Part of the identity.
    pub meta: BTreeMap<String, String>,
}

fn put_bytes(v: &mut Vec<u8>, b: &[u8]) {
    v.extend_from_slice(&(b.len() as u32).to_le_bytes());
    v.extend_from_slice(b);
}

struct Rd<'a>(&'a [u8]);
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::Corrupt("provenance object truncated".into()));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        if n > MAX_FIELD {
            return Err(Error::Corrupt("provenance field too large".into()));
        }
        self.take(n)
    }
    fn string(&mut self) -> Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| Error::Corrupt("provenance: non-UTF-8 string".into()))
    }
    fn hash(&mut self) -> Result<Hash> {
        let b = self.take(32)?;
        let mut a = [0u8; 32];
        a.copy_from_slice(b);
        Ok(Hash(a))
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > MAX_ITEMS {
            return Err(Error::Corrupt("provenance: too many items".into()));
        }
        Ok(n)
    }
}

impl Record {
    /// Canonical encoding (deterministic: maps are sorted).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = REC_MAGIC.to_vec();
        put_bytes(&mut v, self.op.as_bytes());
        match &self.params {
            Some(h) => {
                v.push(1);
                v.extend_from_slice(h.as_bytes());
            }
            None => v.push(0),
        }
        for map in [&self.inputs, &self.outputs] {
            v.extend_from_slice(&(map.len() as u32).to_le_bytes());
            for (role, h) in map {
                put_bytes(&mut v, role.as_bytes());
                v.extend_from_slice(h.as_bytes());
            }
        }
        v.extend_from_slice(&(self.meta.len() as u32).to_le_bytes());
        for (k, val) in &self.meta {
            put_bytes(&mut v, k.as_bytes());
            put_bytes(&mut v, val.as_bytes());
        }
        v
    }

    /// Decodes and re-checks canonicality (sorted, unique keys, no trailing bytes).
    pub fn decode(b: &[u8]) -> Result<Record> {
        if !is_record(b) {
            return Err(Error::Corrupt("not a provenance record".into()));
        }
        let mut r = Rd(&b[8..]);
        let op = r.string()?;
        let params = match r.take(1)?[0] {
            0 => None,
            1 => Some(r.hash()?),
            _ => return Err(Error::Corrupt("provenance: bad params tag".into())),
        };
        let mut maps = [BTreeMap::new(), BTreeMap::new()];
        for m in maps.iter_mut() {
            for _ in 0..r.count()? {
                let role = r.string()?;
                let h = r.hash()?;
                if m.insert(role, h).is_some() {
                    return Err(Error::Corrupt("provenance: duplicate role".into()));
                }
            }
        }
        let mut meta = BTreeMap::new();
        for _ in 0..r.count()? {
            let k = r.string()?;
            let v = r.string()?;
            if meta.insert(k, v).is_some() {
                return Err(Error::Corrupt("provenance: duplicate meta key".into()));
            }
        }
        if !r.0.is_empty() {
            return Err(Error::Corrupt("provenance: trailing bytes".into()));
        }
        let [inputs, outputs] = maps;
        let rec = Record { op, params, inputs, outputs, meta };
        if rec.encode() != b {
            return Err(Error::Corrupt("provenance record is not canonical".into()));
        }
        Ok(rec)
    }
}

/// True if `b` starts with the record magic.
pub fn is_record(b: &[u8]) -> bool {
    b.len() >= 8 && &b[..8] == REC_MAGIC
}

/// A node of the provenance log: a batch of records plus its predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogNode {
    /// The previous log node (`None` for the first).
    pub prev: Option<Hash>,
    /// Records appended by this node.
    pub records: Vec<Hash>,
}

impl LogNode {
    /// Canonical encoding (`P2PPLOG1`).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = LOG_MAGIC.to_vec();
        match &self.prev {
            Some(h) => {
                v.push(1);
                v.extend_from_slice(h.as_bytes());
            }
            None => v.push(0),
        }
        v.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        for h in &self.records {
            v.extend_from_slice(h.as_bytes());
        }
        v
    }

    /// Decodes a log node.
    pub fn decode(b: &[u8]) -> Result<LogNode> {
        if b.len() < 8 || &b[..8] != LOG_MAGIC {
            return Err(Error::Corrupt("not a provenance log node".into()));
        }
        let mut r = Rd(&b[8..]);
        let prev = match r.take(1)?[0] {
            0 => None,
            1 => Some(r.hash()?),
            _ => return Err(Error::Corrupt("provenance log: bad prev tag".into())),
        };
        let n = r.count()?;
        let mut records = Vec::with_capacity(n);
        for _ in 0..n {
            records.push(r.hash()?);
        }
        if !r.0.is_empty() {
            return Err(Error::Corrupt("provenance log: trailing bytes".into()));
        }
        Ok(LogNode { prev, records })
    }
}

/// Appends records to the provenance log of `repo` (OCC, retried on conflict).
/// Every input, output and params object must already exist (a dangling
/// provenance record is refused). Returns the record hashes.
pub fn record(repo: &dyn Repository, log_ref: &Name, recs: &[Record]) -> Result<Vec<Hash>> {
    let mut hashes = Vec::with_capacity(recs.len());
    for r in recs {
        for h in r.inputs.values().chain(r.outputs.values()).chain(r.params.iter()) {
            if !repo.contains(h)? {
                return Err(Error::MissingObject(*h));
            }
        }
        hashes.push(repo.put(&r.encode())?);
    }
    if hashes.is_empty() {
        return Ok(hashes);
    }
    for _ in 0..1000 {
        let cur = repo.resolve(log_ref)?;
        let node = LogNode { prev: cur.map(|v| v.hash), records: hashes.clone() };
        let nh = repo.put(&node.encode())?;
        match repo.replace(log_ref, &[(log_ref.clone(), RefValue::observed(cur))], nh)? {
            ReplaceOutcome::Ok { .. } => return Ok(hashes),
            ReplaceOutcome::Conflict { .. } => continue,
        }
    }
    Err(Error::Backend { transient: true, msg: "provenance log: too much contention".into() })
}

/// An in-memory view of the provenance log.
#[derive(Default, Debug)]
pub struct Provenance {
    records: HashMap<Hash, Record>,
    by_output: HashMap<Hash, Vec<Hash>>,
    by_input: HashMap<Hash, Vec<Hash>>,
}

impl Provenance {
    /// Loads every record reachable from the log head.
    pub fn load(repo: &dyn Repository, log_ref: &Name) -> Result<Provenance> {
        let mut p = Provenance::default();
        let mut next = repo.resolve(log_ref)?.map(|v| v.hash);
        let mut seen = HashSet::new();
        while let Some(h) = next {
            if !seen.insert(h) {
                return Err(Error::Corrupt("provenance log has a cycle".into()));
            }
            let node = LogNode::decode(&repo.get(&h, None)?)?;
            for rh in node.records {
                if p.records.contains_key(&rh) {
                    continue;
                }
                let rec = Record::decode(&repo.get(&rh, None)?)?;
                for o in rec.outputs.values() {
                    p.by_output.entry(*o).or_default().push(rh);
                }
                for i in rec.inputs.values() {
                    p.by_input.entry(*i).or_default().push(rh);
                }
                p.records.insert(rh, rec);
            }
            next = node.prev;
        }
        for v in p.by_output.values_mut().chain(p.by_input.values_mut()) {
            v.sort();
            v.dedup();
        }
        Ok(p)
    }

    /// Number of distinct records.
    pub fn len(&self) -> usize {
        self.records.len()
    }
    /// True if no records were loaded.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    /// The record with hash `h`.
    pub fn get(&self, h: &Hash) -> Option<&Record> {
        self.records.get(h)
    }

    /// Records that produced `h`.
    pub fn producers(&self, h: &Hash) -> Vec<(Hash, &Record)> {
        self.by_output.get(h).into_iter().flatten().filter_map(|r| self.records.get(r).map(|x| (*r, x))).collect()
    }

    /// Records that consumed `h`.
    pub fn consumers(&self, h: &Hash) -> Vec<(Hash, &Record)> {
        self.by_input.get(h).into_iter().flatten().filter_map(|r| self.records.get(r).map(|x| (*r, x))).collect()
    }

    /// Every object `h` was (transitively) derived from, params included.
    pub fn lineage(&self, h: &Hash) -> BTreeSet<Hash> {
        self.closure(h, |p, x| {
            p.producers(x).into_iter().flat_map(|(_, r)| r.inputs.values().copied().chain(r.params)).collect()
        })
    }

    /// Every object (transitively) derived from `h`, i.e. what must be re-verified if `h` is bad.
    pub fn impact(&self, h: &Hash) -> BTreeSet<Hash> {
        self.closure(h, |p, x| p.consumers(x).into_iter().flat_map(|(_, r)| r.outputs.values().copied()).collect())
    }

    fn closure(&self, start: &Hash, next: impl Fn(&Self, &Hash) -> Vec<Hash>) -> BTreeSet<Hash> {
        let mut out = BTreeSet::new();
        let mut q: VecDeque<Hash> = next(self, start).into();
        while let Some(x) = q.pop_front() {
            if x != *start && out.insert(x) {
                q.extend(next(self, &x));
            }
        }
        out
    }

    /// Can `h` be regenerated? That needs a producing record whose inputs
    /// (and params) are all present, or themselves regenerable (recursively).
    /// Returns a witness: a record that applies.
    pub fn regenerable(&self, store: &dyn ObjectStore, h: &Hash) -> Result<Option<Hash>> {
        self.regen(store, h, &mut HashSet::new())
    }

    fn regen(&self, store: &dyn ObjectStore, h: &Hash, visiting: &mut HashSet<Hash>) -> Result<Option<Hash>> {
        if !visiting.insert(*h) {
            return Ok(None); // cycle: not a valid derivation
        }
        'records: for (rh, r) in self.producers(h) {
            for i in r.inputs.values().chain(r.params.iter()) {
                if !store.contains(i)? && self.regen(store, i, visiting)?.is_none() {
                    continue 'records;
                }
            }
            visiting.remove(h);
            return Ok(Some(rh));
        }
        visiting.remove(h);
        Ok(None)
    }
}

/// GC tracer: provenance log nodes keep their records and predecessor alive,
/// and records keep their **inputs and params** alive, but not their
/// outputs. Other objects are delegated to `inner`.
pub struct ProvTracer<T: Tracer> {
    /// Tracer for every other object (e.g. Git/OCI adapters).
    pub inner: T,
}

impl<T: Tracer> Tracer for ProvTracer<T> {
    fn children(&self, hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        let size = store.size(hash)?;
        let head = store.get(hash, Some(0..size.min(8)))?;
        if head == LOG_MAGIC {
            let n = LogNode::decode(&store.get(hash, None)?)?;
            return Ok(n.prev.into_iter().chain(n.records).collect());
        }
        if head == REC_MAGIC {
            let r = Record::decode(&store.get(hash, None)?)?;
            return Ok(r.inputs.values().copied().chain(r.params).collect());
        }
        self.inner.children(hash, store)
    }
}
