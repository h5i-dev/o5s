//! Content-addressed sorted segment maps: a scalable `key → (Hash, aux)` map
//! stored as core objects behind **one** ref.
//!
//! The OCI adapter uses them for the digest index and per-repo blob membership,
//! and the git adapter will use them for its oid map. A ref per entry would not
//! scale, because the core RefStore keeps every ref in memory.
//!
//! # Format (all integers little-endian)
//! * Segment: `b"P2PSEG01"`, `count: u64`, then `count` entries sorted by key
//!   with no duplicates. An entry is 72 bytes: `key: [u8; 32]`, `hash: [u8; 32]`,
//!   `aux: u64`.
//! * Root: `b"P2PMAP01"`, `n: u32`, then `n` items of `(segment: [u8; 32], count: u64)`,
//!   oldest first.
//!
//! Segments of one map are pairwise disjoint (an insert only writes keys that
//! are not present yet), so a lookup stops at the first hit, and merging two
//! segments is a plain sorted merge whose count is known up front.
//!
//! # Updates
//! An insert writes a new segment and a new root, then moves the ref with OCC
//! (`replace` with the observed version). On conflict it re-reads and retries.
//! The mapping is a pure function of content, so a concurrent insert of the
//! same key with a different value means corruption, and is reported as such.
//! In-process inserts to one map are serialized by a mutex, so OCC conflicts
//! only arise between server processes. Compaction is tiered: while the newest
//! segment is at least half the size of the one before it, the two are merged.
//! That keeps O(log n) segments with O(n log n) total merge work.
//!
//! # GC
//! Map values are **weak**: [`MapTracer`] keeps the root and the segments alive,
//! but not the mapped objects. Liveness of mapped objects comes from whatever
//! references them (OCI manifests, git commits and trees). A lookup whose
//! target has been collected is a miss (`get_live`), and re-inserting it heals
//! the entry, because the value is a pure function of the key's content.

use pico2peta_core::{Error, Hash, Name, ObjectStore, RefValue, ReplaceOutcome, Repository, Result};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

pub const SEG_MAGIC: &[u8; 8] = b"P2PSEG01";
pub const ROOT_MAGIC: &[u8; 8] = b"P2PMAP01";
const SEG_HEADER: u64 = 16;
pub const ENTRY: u64 = 72;
/// Segments up to this many entries are decoded whole and cached; larger
/// ones are binary-searched with ranged reads.
const CACHE_WHOLE: u64 = 1 << 15;
/// Total entries kept in the decoded-segment cache.
const CACHE_ENTRIES: u64 = 1 << 20;

pub type Key = [u8; 32];
/// A decoded segment.
type Segment = Arc<Vec<(Key, Value)>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value {
    pub hash: Hash,
    pub aux: u64,
}

/// A decoded root: segments oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Root {
    pub segments: Vec<(Hash, u64)>,
}

impl Root {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(12 + self.segments.len() * 40);
        v.extend_from_slice(ROOT_MAGIC);
        v.extend_from_slice(&(self.segments.len() as u32).to_le_bytes());
        for (h, c) in &self.segments {
            v.extend_from_slice(h.as_bytes());
            v.extend_from_slice(&c.to_le_bytes());
        }
        v
    }

    pub fn decode(b: &[u8]) -> Result<Root> {
        let bad = |m: &str| Error::Corrupt(format!("segmap root: {m}"));
        if b.len() < 12 || &b[..8] != ROOT_MAGIC {
            return Err(bad("bad magic"));
        }
        let n = u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize;
        if b.len() != 12 + n * 40 {
            return Err(bad("bad length"));
        }
        let mut segments = Vec::with_capacity(n);
        for i in 0..n {
            let o = 12 + i * 40;
            let mut h = [0u8; 32];
            h.copy_from_slice(&b[o..o + 32]);
            let mut c = [0u8; 8];
            c.copy_from_slice(&b[o + 32..o + 40]);
            segments.push((Hash(h), u64::from_le_bytes(c)));
        }
        Ok(Root { segments })
    }

    pub fn len(&self) -> u64 {
        self.segments.iter().map(|s| s.1).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn encode_entry(out: &mut Vec<u8>, k: &Key, v: &Value) {
    out.extend_from_slice(k);
    out.extend_from_slice(v.hash.as_bytes());
    out.extend_from_slice(&v.aux.to_le_bytes());
}

fn decode_entry(b: &[u8]) -> (Key, Value) {
    let mut k = [0u8; 32];
    k.copy_from_slice(&b[..32]);
    let mut h = [0u8; 32];
    h.copy_from_slice(&b[32..64]);
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[64..72]);
    (k, Value { hash: Hash(h), aux: u64::from_le_bytes(a) })
}

/// Encodes sorted, unique entries as a segment.
pub fn encode_segment(entries: &[(Key, Value)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(SEG_HEADER as usize + entries.len() * ENTRY as usize);
    v.extend_from_slice(SEG_MAGIC);
    v.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (k, val) in entries {
        encode_entry(&mut v, k, val);
    }
    v
}

/// Decodes and validates a whole segment.
pub fn decode_segment(b: &[u8]) -> Result<Vec<(Key, Value)>> {
    let bad = |m: &str| Error::Corrupt(format!("segmap segment: {m}"));
    if b.len() < SEG_HEADER as usize || &b[..8] != SEG_MAGIC {
        return Err(bad("bad magic"));
    }
    let mut c = [0u8; 8];
    c.copy_from_slice(&b[8..16]);
    let n = u64::from_le_bytes(c);
    if (b.len() as u64 - SEG_HEADER) != n.checked_mul(ENTRY).ok_or_else(|| bad("count overflow"))? {
        return Err(bad("bad length"));
    }
    let out: Vec<(Key, Value)> =
        b[SEG_HEADER as usize..].as_chunks::<{ ENTRY as usize }>().0.iter().map(|e| decode_entry(e)).collect();
    if out.windows(2).any(|w| w[0].0 >= w[1].0) {
        return Err(bad("keys not strictly sorted"));
    }
    Ok(out)
}

/// A handle to one map (one ref).
pub struct SegMap {
    repo: Arc<dyn Repository>,
    name: Name,
    cache: Arc<SegCache>,
    write_lock: tokio::sync::Mutex<()>,
}

/// Cache of decoded small segments, keyed by content hash (immutable, so
/// never stale). Shared by all maps.
#[derive(Default)]
pub struct SegCache {
    inner: Mutex<(HashMap<Hash, Segment>, u64)>,
}

impl SegCache {
    fn get(&self, h: &Hash) -> Option<Arc<Vec<(Key, Value)>>> {
        self.inner.lock().ok()?.0.get(h).cloned()
    }
    fn put(&self, h: Hash, v: Arc<Vec<(Key, Value)>>) {
        if let Ok(mut g) = self.inner.lock() {
            let n = v.len() as u64;
            if g.1 + n > CACHE_ENTRIES {
                // Simple bounded policy: drop everything. Hot segments come back quickly.
                g.0.clear();
                g.1 = 0;
            }
            if g.0.insert(h, v).is_none() {
                g.1 += n;
            }
        }
    }
}

impl SegMap {
    pub fn new(repo: Arc<dyn Repository>, name: Name, cache: Arc<SegCache>) -> SegMap {
        SegMap { repo, name, cache, write_lock: tokio::sync::Mutex::new(()) }
    }

    pub fn name(&self) -> &Name {
        &self.name
    }

    /// Current root and its ref value (blocking).
    pub fn root_blocking(&self) -> Result<(Option<RefValue>, Root)> {
        match self.repo.resolve(&self.name)? {
            None => Ok((None, Root::default())),
            Some(v) => Ok((Some(v), Root::decode(&self.repo.get(&v.hash, None)?)?)),
        }
    }

    fn segment_lookup(&self, seg: &Hash, count: u64, key: &Key) -> Result<Option<Value>> {
        if count <= CACHE_WHOLE {
            let entries = match self.cache.get(seg) {
                Some(e) => e,
                None => {
                    let e = Arc::new(decode_segment(&self.repo.get(seg, None)?)?);
                    if e.len() as u64 != count {
                        return Err(Error::Corrupt(format!(
                            "segment {seg} has {} entries, root says {count}",
                            e.len()
                        )));
                    }
                    self.cache.put(*seg, e.clone());
                    e
                }
            };
            return Ok(entries.binary_search_by(|(k, _)| k.cmp(key)).ok().map(|i| entries[i].1));
        }
        // Binary search with ranged reads.
        let (mut lo, mut hi) = (0u64, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = SEG_HEADER + mid * ENTRY;
            let b = self.repo.get(seg, Some(off..off + ENTRY))?;
            if b.len() as u64 != ENTRY {
                return Err(Error::Corrupt(format!("short segment read in {seg}")));
            }
            let (k, v) = decode_entry(&b);
            match k.cmp(key) {
                std::cmp::Ordering::Equal => return Ok(Some(v)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    fn lookup_in(&self, root: &Root, key: &Key) -> Result<Option<Value>> {
        for (seg, count) in root.segments.iter().rev() {
            if let Some(v) = self.segment_lookup(seg, *count, key)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Looks `key` up in a root loaded earlier (for many lookups against one
    /// consistent version, e.g. a GC mark or a git walk).
    pub fn get_in(&self, root: &Root, key: &Key) -> Result<Option<Value>> {
        self.lookup_in(root, key)
    }

    /// Looks `key` up (blocking).
    pub fn get_blocking(&self, key: &Key) -> Result<Option<Value>> {
        let (_, root) = self.root_blocking()?;
        self.lookup_in(&root, key)
    }

    /// Like [`Self::get_blocking`], but a value whose object no longer exists
    /// (weak entry collected by GC) is a miss.
    pub fn get_live_blocking(&self, key: &Key) -> Result<Option<Value>> {
        match self.get_blocking(key)? {
            Some(v) if self.repo.contains(&v.hash)? => Ok(Some(v)),
            _ => Ok(None),
        }
    }

    /// Inserts entries (blocking). Existing keys must map to the same hash,
    /// otherwise this fails with `Corrupt`. Returns how many were new.
    pub fn insert_blocking(&self, mut entries: Vec<(Key, Value)>) -> Result<usize> {
        entries.sort_by_key(|e| e.0);
        entries.dedup_by(|a, b| a.0 == b.0);
        for _attempt in 0..16 {
            let (cur, root) = self.root_blocking()?;
            let mut fresh = Vec::with_capacity(entries.len());
            for (k, v) in &entries {
                match self.lookup_in(&root, k)? {
                    Some(old) if old.hash == v.hash => {}
                    Some(old) => {
                        return Err(Error::Corrupt(format!(
                            "{}: key already maps to {}, refusing to remap to {}",
                            self.name, old.hash, v.hash
                        )));
                    }
                    None => fresh.push((*k, *v)),
                }
            }
            if fresh.is_empty() {
                return Ok(0);
            }
            let n = fresh.len();
            let seg = self.repo.put(&encode_segment(&fresh))?;
            let mut new_root = root.clone();
            new_root.segments.push((seg, n as u64));
            self.compact(&mut new_root)?;
            let root_hash = self.repo.put(&new_root.encode())?;
            let observed = RefValue::observed(cur);
            match self.repo.replace(&self.name, &[(self.name.clone(), observed)], root_hash)? {
                ReplaceOutcome::Ok { .. } => return Ok(n),
                ReplaceOutcome::Conflict { .. } => {
                    std::thread::sleep(std::time::Duration::from_millis(fastrand::u64(1..20)));
                }
            }
        }
        Err(Error::Backend { transient: true, msg: format!("{}: too much update contention", self.name) })
    }

    /// Async wrapper: serializes in-process writers and runs on the blocking pool.
    pub async fn insert(self: &Arc<Self>, entries: Vec<(Key, Value)>) -> Result<usize> {
        let _g = self.write_lock.lock().await;
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.insert_blocking(entries))
            .await
            .map_err(|e| Error::Backend { transient: false, msg: format!("segmap task: {e}") })?
    }

    pub async fn get_live(self: &Arc<Self>, key: Key) -> Result<Option<Value>> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.get_live_blocking(&key))
            .await
            .map_err(|e| Error::Backend { transient: false, msg: format!("segmap task: {e}") })?
    }

    /// Tiered compaction of the newest segments.
    fn compact(&self, root: &mut Root) -> Result<()> {
        while root.segments.len() >= 2 {
            let n = root.segments.len();
            let (a, ca) = root.segments[n - 2];
            let (b, cb) = root.segments[n - 1];
            if cb * 2 < ca {
                break;
            }
            let merged = self.merge(&a, ca, &b, cb)?;
            root.segments.truncate(n - 2);
            root.segments.push((merged, ca + cb));
        }
        Ok(())
    }

    /// Streams a sorted merge of two disjoint segments into a new segment.
    fn merge(&self, a: &Hash, ca: u64, b: &Hash, cb: u64) -> Result<Hash> {
        let ra = SegReader::open(self.repo.as_ref(), a, ca)?;
        let rb = SegReader::open(self.repo.as_ref(), b, cb)?;
        let mut m = MergeReader::new(ra, rb, ca + cb);
        let h = self.repo.put_stream(&mut m)?;
        if let Some(e) = m.error.take() {
            return Err(e);
        }
        Ok(h)
    }
}

/// Sequential entry reader over a segment object.
struct SegReader {
    r: std::io::BufReader<Box<dyn Read + Send>>,
    left: u64,
}

impl SegReader {
    fn open(store: &dyn ObjectStore, h: &Hash, count: u64) -> Result<SegReader> {
        let mut r = std::io::BufReader::with_capacity(1 << 16, store.open(h, None)?);
        let mut hdr = [0u8; 16];
        r.read_exact(&mut hdr)?;
        if &hdr[..8] != SEG_MAGIC
            || u64::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15]]) != count
        {
            return Err(Error::Corrupt(format!("segment {h}: bad header")));
        }
        Ok(SegReader { r, left: count })
    }

    fn next(&mut self) -> Result<Option<[u8; 72]>> {
        if self.left == 0 {
            return Ok(None);
        }
        let mut e = [0u8; 72];
        self.r.read_exact(&mut e)?;
        self.left -= 1;
        Ok(Some(e))
    }
}

/// `Read` producing the merged segment's bytes.
struct MergeReader {
    a: SegReader,
    b: SegReader,
    pa: Option<[u8; 72]>,
    pb: Option<[u8; 72]>,
    started: bool,
    header: Vec<u8>,
    buf: Vec<u8>,
    pos: usize,
    last: Option<Key>,
    error: Option<Error>,
}

impl MergeReader {
    fn new(a: SegReader, b: SegReader, total: u64) -> Self {
        let mut header = SEG_MAGIC.to_vec();
        header.extend_from_slice(&total.to_le_bytes());
        MergeReader {
            a,
            b,
            pa: None,
            pb: None,
            started: false,
            header,
            buf: Vec::new(),
            pos: 0,
            last: None,
            error: None,
        }
    }

    fn fill(&mut self) -> Result<()> {
        self.buf.clear();
        self.pos = 0;
        if !self.started {
            self.started = true;
            self.buf.extend_from_slice(&self.header);
            self.pa = self.a.next()?;
            self.pb = self.b.next()?;
        }
        while self.buf.len() < 1 << 16 {
            let take_a = match (&self.pa, &self.pb) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(x), Some(y)) => x[..32] < y[..32],
            };
            let e = if take_a {
                let e = self.pa.take();
                self.pa = self.a.next()?;
                e
            } else {
                let e = self.pb.take();
                self.pb = self.b.next()?;
                e
            };
            let Some(e) = e else { break };
            let mut k = [0u8; 32];
            k.copy_from_slice(&e[..32]);
            if self.last.is_some_and(|l| l >= k) {
                return Err(Error::Corrupt("segments are not disjoint and sorted".into()));
            }
            self.last = Some(k);
            self.buf.extend_from_slice(&e);
        }
        Ok(())
    }
}

impl Read for MergeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            if let Err(e) = self.fill() {
                let msg = e.to_string();
                self.error = Some(e);
                return Err(std::io::Error::other(msg));
            }
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Children of map objects for GC: a root keeps its segments alive. Segments
/// have no children (map values are weak).
pub fn map_children(head: &[u8], h: &Hash, store: &dyn ObjectStore) -> Result<Option<Vec<Hash>>> {
    if head.starts_with(ROOT_MAGIC) {
        let root = Root::decode(&store.get(h, None)?)?;
        return Ok(Some(root.segments.into_iter().map(|(s, _)| s).collect()));
    }
    if head.starts_with(SEG_MAGIC) {
        return Ok(Some(Vec::new()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pico2peta_repo::LocalRepo;

    fn key(i: u64) -> Key {
        let mut k = [0u8; 32];
        k[..8].copy_from_slice(&i.to_be_bytes());
        k[31] = 7;
        k
    }

    fn val(i: u64) -> Value {
        Value { hash: Hash::of(&i.to_le_bytes()), aux: i }
    }

    fn repo() -> (tempfile::TempDir, Arc<dyn Repository>) {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path().join("r"), &Default::default()).unwrap();
        (d, Arc::new(r))
    }

    #[test]
    fn codec_roundtrip_and_rejects_garbage() {
        let e: Vec<_> = (0..10).map(|i| (key(i), val(i))).collect();
        assert_eq!(decode_segment(&encode_segment(&e)).unwrap(), e);
        let r = Root { segments: vec![(Hash::of(b"a"), 3), (Hash::of(b"b"), 9)] };
        assert_eq!(Root::decode(&r.encode()).unwrap(), r);
        let mut unsorted = e.clone();
        unsorted.swap(0, 1);
        assert!(decode_segment(&encode_segment(&unsorted)).is_err());
        for bad in [&b""[..], b"P2PSEG01", b"P2PSEG01\x01\0\0\0\0\0\0\0", b"P2PMAP01\x05\0\0\0", b"xxxxxxxxxxxxxxxx"] {
            assert!(decode_segment(bad).is_err());
            assert!(Root::decode(bad).is_err());
        }
    }

    #[test]
    fn insert_lookup_compact_and_conflicts() {
        let (_d, repo) = repo();
        let m = SegMap::new(repo.clone(), Name::new("t/map").unwrap(), Arc::default());
        let mut next = 0u64;
        // 40 inserts of growing batches exercise tiered compaction.
        for round in 1..=40u64 {
            let batch: Vec<_> = (next..next + round).map(|i| (key(i), val(i))).collect();
            next += round;
            assert_eq!(m.insert_blocking(batch).unwrap(), round as usize);
        }
        let (_, root) = m.root_blocking().unwrap();
        assert_eq!(root.len(), next);
        assert!(root.segments.len() <= 12, "tiered compaction keeps few segments: {}", root.segments.len());
        for i in (0..next).step_by(7) {
            assert_eq!(m.get_blocking(&key(i)).unwrap(), Some(val(i)));
        }
        assert_eq!(m.get_blocking(&key(next + 1)).unwrap(), None);
        // Re-inserting is a no-op; remapping is refused.
        assert_eq!(m.insert_blocking(vec![(key(3), val(3))]).unwrap(), 0);
        assert!(matches!(m.insert_blocking(vec![(key(3), val(4))]), Err(Error::Corrupt(_))));
        // A second handle (another "process") sees everything and can extend it.
        let m2 = SegMap::new(repo, Name::new("t/map").unwrap(), Arc::default());
        assert_eq!(m2.insert_blocking(vec![(key(next), val(next))]).unwrap(), 1);
        assert_eq!(m.get_blocking(&key(next)).unwrap(), Some(val(next)));
    }

    #[test]
    fn large_segments_use_ranged_search() {
        let (_d, repo) = repo();
        let m = SegMap::new(repo, Name::new("t/big").unwrap(), Arc::default());
        let n = CACHE_WHOLE + 1000;
        m.insert_blocking((0..n).map(|i| (key(i * 2), val(i))).collect()).unwrap();
        assert_eq!(m.get_blocking(&key(2 * 777)).unwrap(), Some(val(777)));
        assert_eq!(m.get_blocking(&key(2 * (n - 1))).unwrap(), Some(val(n - 1)));
        assert_eq!(m.get_blocking(&key(3)).unwrap(), None);
    }

    #[test]
    fn concurrent_writers_converge() {
        let (_d, repo) = repo();
        std::thread::scope(|s| {
            for t in 0..4u64 {
                let repo = repo.clone();
                s.spawn(move || {
                    let m = SegMap::new(repo, Name::new("t/conc").unwrap(), Arc::default());
                    for i in 0..25u64 {
                        let k = t * 1000 + i;
                        m.insert_blocking(vec![(key(k), val(k))]).unwrap();
                    }
                });
            }
        });
        let m = SegMap::new(repo, Name::new("t/conc").unwrap(), Arc::default());
        assert_eq!(m.root_blocking().unwrap().1.len(), 100);
        for t in 0..4u64 {
            for i in 0..25u64 {
                assert!(m.get_blocking(&key(t * 1000 + i)).unwrap().is_some());
            }
        }
    }
}
