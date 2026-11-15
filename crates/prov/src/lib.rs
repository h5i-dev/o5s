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
const LOG_MAGIC_V2: &[u8; 8] = b"P2PPLOG2";
const IDX_MAGIC: &[u8; 8] = b"P2PPIDX1";
const IDX_ENTRY: u64 = 64;
/// Build a new checkpoint index once the records since the last one reach
/// max(MIN_CHECKPOINT, total / 8): geometric growth keeps rebuilds amortized.
const MIN_CHECKPOINT: u64 = 1024;
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
    /// Checkpoint index covering every record in this node and all older
    /// nodes (`P2PPIDX1`), if this node carries one.
    pub checkpoint: Option<Hash>,
    /// Total number of records in this node and all older nodes.
    pub total: u64,
}

impl LogNode {
    /// Canonical encoding (`P2PPLOG2`).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = LOG_MAGIC_V2.to_vec();
        for h in [&self.prev, &self.checkpoint] {
            match h {
                Some(h) => {
                    v.push(1);
                    v.extend_from_slice(h.as_bytes());
                }
                None => v.push(0),
            }
        }
        v.extend_from_slice(&self.total.to_le_bytes());
        v.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        for h in &self.records {
            v.extend_from_slice(h.as_bytes());
        }
        v
    }

    /// Decodes a log node (`P2PPLOG2`, or the original `P2PPLOG1`).
    pub fn decode(b: &[u8]) -> Result<LogNode> {
        let v2 = b.len() >= 8 && &b[..8] == LOG_MAGIC_V2;
        if !v2 && !(b.len() >= 8 && &b[..8] == LOG_MAGIC) {
            return Err(Error::Corrupt("not a provenance log node".into()));
        }
        let mut r = Rd(&b[8..]);
        let opt = |r: &mut Rd| -> Result<Option<Hash>> {
            match r.take(1)?[0] {
                0 => Ok(None),
                1 => Ok(Some(r.hash()?)),
                _ => Err(Error::Corrupt("provenance log: bad tag".into())),
            }
        };
        let prev = opt(&mut r)?;
        let (checkpoint, total) = if v2 {
            let c = opt(&mut r)?;
            let t = r.take(8)?;
            let mut a = [0u8; 8];
            a.copy_from_slice(t);
            (c, Some(u64::from_le_bytes(a)))
        } else {
            (None, None)
        };
        let n = r.count()?;
        let mut records = Vec::with_capacity(n);
        for _ in 0..n {
            records.push(r.hash()?);
        }
        if !r.0.is_empty() {
            return Err(Error::Corrupt("provenance log: trailing bytes".into()));
        }
        Ok(LogNode { prev, total: total.unwrap_or(0), records, checkpoint })
    }
}

/// Builds a checkpoint index object: `P2PPIDX1 | n_fwd u64 | n_bwd u64 |
/// fwd (input, record)* | bwd (output, record)*`, both tables sorted.
fn encode_index(fwd: &mut Vec<(Hash, Hash)>, bwd: &mut Vec<(Hash, Hash)>) -> Vec<u8> {
    fwd.sort();
    fwd.dedup();
    bwd.sort();
    bwd.dedup();
    let mut v = Vec::with_capacity(24 + (fwd.len() + bwd.len()) * 64);
    v.extend_from_slice(IDX_MAGIC);
    v.extend_from_slice(&(fwd.len() as u64).to_le_bytes());
    v.extend_from_slice(&(bwd.len() as u64).to_le_bytes());
    for (k, r) in fwd.iter().chain(bwd.iter()) {
        v.extend_from_slice(k.as_bytes());
        v.extend_from_slice(r.as_bytes());
    }
    v
}

/// Reads all entries of a checkpoint index (used only when rebuilding one).
/// `(key, record)` pairs of one checkpoint table.
type Table = Vec<(Hash, Hash)>;

fn read_index(repo: &dyn Repository, idx: &Hash) -> Result<(Table, Table)> {
    let b = repo.get(idx, None)?;
    let (nf, nb) = index_header(&b)?;
    if nf.checked_add(nb).and_then(|n| n.checked_mul(IDX_ENTRY)).and_then(|n| n.checked_add(24)) != Some(b.len() as u64)
    {
        return Err(Error::Corrupt("provenance index length mismatch".into()));
    }
    let ent = |i: u64| -> (Hash, Hash) {
        let o = (24 + i * IDX_ENTRY) as usize;
        let mut k = [0u8; 32];
        let mut r = [0u8; 32];
        k.copy_from_slice(&b[o..o + 32]);
        r.copy_from_slice(&b[o + 32..o + 64]);
        (Hash(k), Hash(r))
    };
    Ok(((0..nf).map(ent).collect(), (nf..nf + nb).map(ent).collect()))
}

fn index_header(b: &[u8]) -> Result<(u64, u64)> {
    if b.len() < 24 || &b[..8] != IDX_MAGIC {
        return Err(Error::Corrupt("not a provenance index".into()));
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[8..16]);
    let nf = u64::from_le_bytes(a);
    a.copy_from_slice(&b[16..24]);
    let nb = u64::from_le_bytes(a);
    // Callers validate the total length (24 + 64·(nf+nb)) against the object size.
    Ok((nf, nb))
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
        let node = next_node(repo, cur.map(|v| v.hash), &hashes, recs)?;
        let nh = repo.put(&node.encode())?;
        match repo.replace(log_ref, &[(log_ref.clone(), RefValue::observed(cur))], nh)? {
            ReplaceOutcome::Ok { .. } => return Ok(hashes),
            ReplaceOutcome::Conflict { .. } => continue,
        }
    }
    Err(Error::Backend { transient: true, msg: "provenance log: too much contention".into() })
}

/// Builds the next log node. Attaches a new checkpoint index when the records
/// since the last checkpoint reach the geometric threshold.
fn next_node(repo: &dyn Repository, prev: Option<Hash>, hashes: &[Hash], recs: &[Record]) -> Result<LogNode> {
    // Walk back to the latest checkpoint, collecting the records since.
    let mut since: Vec<Hash> = hashes.to_vec();
    let mut base: Option<(Hash, u64)> = None; // (index, records covered)
    let mut total_prev = 0u64;
    let mut cur = prev;
    let mut first = true;
    while let Some(h) = cur {
        let n = LogNode::decode(&repo.get(&h, None)?)?;
        if first {
            total_prev = if n.total > 0 { n.total } else { u64::MAX }; // v1 log: unknown
            first = false;
        }
        if let Some(c) = n.checkpoint {
            base = Some((c, n.total));
            break;
        }
        since.extend_from_slice(&n.records);
        cur = n.prev;
    }
    let total = if total_prev == u64::MAX {
        // Legacy log without totals: count by walking (one time; the next node carries totals).
        let mut c = 0u64;
        let mut x = prev;
        while let Some(h) = x {
            let n = LogNode::decode(&repo.get(&h, None)?)?;
            c += n.records.len() as u64;
            x = n.prev;
        }
        c + hashes.len() as u64
    } else {
        total_prev + hashes.len() as u64
    };
    let covered = base.map_or(0, |b| b.1);
    let threshold = MIN_CHECKPOINT.max(covered / 8);
    let checkpoint = if since.len() as u64 >= threshold {
        let (mut fwd, mut bwd) = match &base {
            Some((idx, _)) => read_index(repo, idx)?,
            None => (Vec::new(), Vec::new()),
        };
        let fresh: HashMap<Hash, &Record> = hashes.iter().copied().zip(recs.iter()).collect();
        for rh in &since {
            let rec = match fresh.get(rh) {
                Some(r) => (*r).clone(),
                None => Record::decode(&repo.get(rh, None)?)?,
            };
            fwd.extend(rec.inputs.values().chain(rec.params.iter()).map(|i| (*i, *rh)));
            bwd.extend(rec.outputs.values().map(|o| (*o, *rh)));
        }
        Some(repo.put(&encode_index(&mut fwd, &mut bwd))?)
    } else {
        None
    };
    Ok(LogNode { prev, records: hashes.to_vec(), checkpoint, total })
}

/// A view of the provenance log. Records since the latest checkpoint are
/// held in memory. Older ones are found by binary search (ranged reads) in
/// the checkpoint index and fetched on demand, so loading and queries cost
/// O(recent + log n), whatever the history's size.
pub struct Provenance<'a> {
    repo: &'a dyn Repository,
    recent: HashMap<Hash, Record>,
    by_output: HashMap<Hash, Vec<Hash>>,
    by_input: HashMap<Hash, Vec<Hash>>,
    /// `(index object, n_fwd, n_bwd)`.
    checkpoint: Option<(Hash, u64, u64)>,
    total: u64,
}

impl<'a> Provenance<'a> {
    /// Opens the log at `log_ref`.
    pub fn load(repo: &'a dyn Repository, log_ref: &Name) -> Result<Provenance<'a>> {
        let mut p = Provenance {
            repo,
            recent: HashMap::new(),
            by_output: HashMap::new(),
            by_input: HashMap::new(),
            checkpoint: None,
            total: 0,
        };
        let mut next = repo.resolve(log_ref)?.map(|v| v.hash);
        let mut seen = HashSet::new();
        let mut first = true;
        while let Some(h) = next {
            if !seen.insert(h) {
                return Err(Error::Corrupt("provenance log has a cycle".into()));
            }
            let node = LogNode::decode(&repo.get(&h, None)?)?;
            if first {
                p.total = node.total;
                first = false;
            }
            for rh in &node.records {
                if p.recent.contains_key(rh) {
                    continue;
                }
                let rec = Record::decode(&repo.get(rh, None)?)?;
                for o in rec.outputs.values() {
                    p.by_output.entry(*o).or_default().push(*rh);
                }
                for i in rec.inputs.values().chain(rec.params.iter()) {
                    p.by_input.entry(*i).or_default().push(*rh);
                }
                p.recent.insert(*rh, rec);
            }
            if let Some(c) = node.checkpoint {
                let size = repo.size(&c)?;
                let (nf, nb) = index_header(&repo.get(&c, Some(0..24.min(size)))?)?;
                if nf.checked_add(nb).and_then(|n| n.checked_mul(IDX_ENTRY)).and_then(|n| n.checked_add(24))
                    != Some(size)
                {
                    return Err(Error::Corrupt("provenance index length mismatch".into()));
                }
                p.checkpoint = Some((c, nf, nb));
                break;
            }
            next = node.prev;
        }
        for v in p.by_output.values_mut().chain(p.by_input.values_mut()) {
            v.sort();
            v.dedup();
        }
        Ok(p)
    }

    /// Number of recordings in the log (from the head node's running total).
    /// A derivation recorded twice counts twice; queries deduplicate.
    pub fn len(&self) -> u64 {
        if self.total > 0 { self.total } else { self.recent.len() as u64 }
    }
    /// True if the log is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The record with hash `h` (fetched if it's older than the checkpoint).
    pub fn get(&self, h: &Hash) -> Result<Record> {
        match self.recent.get(h) {
            Some(r) => Ok(r.clone()),
            None => Record::decode(&self.repo.get(h, None)?),
        }
    }

    /// Binary search in one checkpoint table: all records with key `k`.
    fn lookup(&self, k: &Hash, backward: bool) -> Result<Vec<Hash>> {
        let Some((idx, nf, nb)) = self.checkpoint else { return Ok(Vec::new()) };
        let (base, n) = if backward { (nf, nb) } else { (0, nf) };
        let read = |i: u64| -> Result<(Hash, Hash)> {
            let o = 24 + (base + i) * IDX_ENTRY;
            let b = self.repo.get(&idx, Some(o..o + IDX_ENTRY))?;
            let mut kk = [0u8; 32];
            let mut rr = [0u8; 32];
            kk.copy_from_slice(&b[..32]);
            rr.copy_from_slice(&b[32..64]);
            Ok((Hash(kk), Hash(rr)))
        };
        let (mut lo, mut hi) = (0u64, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if read(mid)?.0 < *k { lo = mid + 1 } else { hi = mid }
        }
        let mut out = Vec::new();
        while lo < n {
            let (kk, r) = read(lo)?;
            if kk != *k {
                break;
            }
            out.push(r);
            lo += 1;
        }
        Ok(out)
    }

    fn records_for(&self, h: &Hash, backward: bool) -> Result<Vec<(Hash, Record)>> {
        let mut hs: Vec<Hash> =
            if backward { self.by_output.get(h) } else { self.by_input.get(h) }.cloned().unwrap_or_default();
        hs.extend(self.lookup(h, backward)?);
        hs.sort();
        hs.dedup();
        hs.into_iter().map(|rh| Ok((rh, self.get(&rh)?))).collect()
    }

    /// Records that produced `h`.
    pub fn producers(&self, h: &Hash) -> Result<Vec<(Hash, Record)>> {
        self.records_for(h, true)
    }

    /// Records that consumed `h` (as an input or as params).
    pub fn consumers(&self, h: &Hash) -> Result<Vec<(Hash, Record)>> {
        self.records_for(h, false)
    }

    /// Every object `h` was (transitively) derived from, params included.
    pub fn lineage(&self, h: &Hash) -> Result<BTreeSet<Hash>> {
        self.closure(h, |p, x| {
            Ok(p.producers(x)?
                .into_iter()
                .flat_map(|(_, r)| r.inputs.values().copied().chain(r.params).collect::<Vec<_>>())
                .collect())
        })
    }

    /// Every object (transitively) derived from `h`, i.e. what must be re-verified if `h` is bad.
    pub fn impact(&self, h: &Hash) -> Result<BTreeSet<Hash>> {
        self.closure(h, |p, x| {
            Ok(p.consumers(x)?.into_iter().flat_map(|(_, r)| r.outputs.values().copied().collect::<Vec<_>>()).collect())
        })
    }

    fn closure(&self, start: &Hash, next: impl Fn(&Self, &Hash) -> Result<Vec<Hash>>) -> Result<BTreeSet<Hash>> {
        let mut out = BTreeSet::new();
        let mut q: VecDeque<Hash> = next(self, start)?.into();
        while let Some(x) = q.pop_front() {
            if x != *start && out.insert(x) {
                q.extend(next(self, &x)?);
            }
        }
        Ok(out)
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
        'records: for (rh, r) in self.producers(h)? {
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
        if head == LOG_MAGIC || head == LOG_MAGIC_V2 {
            let n = LogNode::decode(&store.get(hash, None)?)?;
            return Ok(n.prev.into_iter().chain(n.records).chain(n.checkpoint).collect());
        }
        if head == REC_MAGIC {
            let r = Record::decode(&store.get(hash, None)?)?;
            return Ok(r.inputs.values().copied().chain(r.params).collect());
        }
        self.inner.children(hash, store)
    }
}
