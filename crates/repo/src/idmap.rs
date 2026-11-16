//! External-id index for fronts: content-addressed sorted segment maps, a
//! scalable `key → (Hash, aux)` map stored as core objects behind **one** ref.
//!
//! Fronts use it to map external ids (Git SHA-1 oids, OCI sha256 digests, Xet
//! hashes, ...) to core object hashes. Written by the net session for the OCI
//! and Git adapters (`crates/server/src/segmap.rs`), moved into the core as a
//! generic Front API primitive; the on-disk format is unchanged.
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
//! Map values are **weak**: [`map_children`] keeps the root and the segments alive,
//! but not the mapped objects. Liveness of mapped objects comes from whatever
//! references them (OCI manifests, git commits and trees). A lookup whose
//! target has been collected is a miss (`get_live`), and re-inserting it heals
//! the entry, because the value is a pure function of the key's content.

use pico2peta_core::{Error, Hash, Name, ObjectStore, RefValue, ReplaceOutcome, Repository, Result};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

/// A pseudo-random backoff in `1..=cap` ms (no RNG dependency in the core).
fn jitter(cap: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    1 + h.finish() % cap
}

/// The generic name for [`SegMap`] in the Front API.
pub type IdMap = SegMap;

/// Segment magic.
pub const SEG_MAGIC: &[u8; 8] = b"P2PSEG01";
/// Root magic.
pub const ROOT_MAGIC: &[u8; 8] = b"P2PMAP01";
const SEG_HEADER: u64 = 16;
/// Bytes per segment entry.
pub const ENTRY: u64 = 72;
/// Segments up to this many entries are decoded whole and cached; larger
/// ones are binary-searched with ranged reads.
const CACHE_WHOLE: u64 = 1 << 15;
/// Total entries kept in the decoded-segment cache.
const CACHE_ENTRIES: u64 = 1 << 20;
/// Large segments are read in aligned pages of this size. A ranged read of a chunked object
/// decodes (and verifies) the whole chunk it touches, which averages 64 KiB, so caching pages
/// turns ~log2(n) chunk decodes per lookup into hits.
const PAGE: u64 = 64 << 10;
/// Bytes of large-segment pages kept (shared by all maps).
const PAGE_CACHE_BYTES: u64 = 128 << 20;
/// `get_many` scans fetch runs of missing pages with ranged reads of at most this size.
const SCAN_WINDOW: u64 = 4 << 20;

/// A map key (external ids are hashed or padded to 32 bytes by the front).
pub type Key = [u8; 32];
/// A decoded segment.
type Segment = Arc<Vec<(Key, Value)>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A map value.
pub struct Value {
    /// The core object the key maps to.
    pub hash: Hash,
    /// Front-defined extra data (e.g. object type and size).
    pub aux: u64,
}

/// A decoded root: segments oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Root {
    /// Segments, oldest first: `(segment object, entry count)`.
    pub segments: Vec<(Hash, u64)>,
}

impl Root {
    /// Encodes the root object.
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

    /// Decodes a root object.
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

    /// Total entries.
    pub fn len(&self) -> u64 {
        self.segments.iter().map(|s| s.1).sum()
    }

    /// Whether the map is empty.
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
    /// Serializes in-process writers (OCC handles other processes).
    write_lock: Mutex<()>,
}

/// Cache of decoded small segments, keyed by content hash (immutable, so
/// never stale). Shared by all maps.
#[derive(Default)]
pub struct SegCache {
    inner: Mutex<(HashMap<Hash, Segment>, u64)>,
    pages: Mutex<Pages>,
}

/// Pages of large segments: (segment, page index) → bytes, bounded in bytes, FIFO eviction.
#[derive(Default)]
struct Pages {
    map: HashMap<(Hash, u64), Arc<Vec<u8>>>,
    order: std::collections::VecDeque<(Hash, u64)>,
    bytes: u64,
}

impl SegCache {
    fn page(&self, k: &(Hash, u64)) -> Option<Arc<Vec<u8>>> {
        self.pages.lock().ok()?.map.get(k).cloned()
    }
    fn put_page(&self, k: (Hash, u64), v: Arc<Vec<u8>>) {
        if let Ok(mut g) = self.pages.lock() {
            let n = v.len() as u64;
            while g.bytes + n > PAGE_CACHE_BYTES
                && let Some(old) = g.order.pop_front()
            {
                if let Some(o) = g.map.remove(&old) {
                    g.bytes -= o.len() as u64;
                }
            }
            if g.map.insert(k, v).is_none() {
                g.bytes += n;
                g.order.push_back(k);
            }
        }
    }
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
    /// A handle to the map behind ref `name`.
    pub fn new(repo: Arc<dyn Repository>, name: Name, cache: Arc<SegCache>) -> SegMap {
        SegMap { repo, name, cache, write_lock: Mutex::new(()) }
    }

    /// The ref holding the map root.
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
            let entries = self.small_segment(seg, count)?;
            return Ok(entries.binary_search_by(|(k, _)| k.cmp(key)).ok().map(|i| entries[i].1));
        }
        // Binary search over cached pages of the segment.
        let (mut lo, mut hi) = (0u64, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let b = self.entry_bytes(seg, count, mid)?;
            let (k, v) = decode_entry(&b);
            match k.cmp(key) {
                std::cmp::Ordering::Equal => return Ok(Some(v)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Entry `i` of a large segment, read through the page cache.
    fn entry_bytes(&self, seg: &Hash, count: u64, i: u64) -> Result<[u8; ENTRY as usize]> {
        let size = SEG_HEADER + count * ENTRY;
        let off = SEG_HEADER + i * ENTRY;
        let mut out = [0u8; ENTRY as usize];
        let mut done = 0u64;
        while done < ENTRY {
            let at = off + done;
            let pi = at / PAGE;
            let page = match self.cache.page(&(*seg, pi)) {
                Some(p) => p,
                None => {
                    let r = pi * PAGE..((pi + 1) * PAGE).min(size);
                    let want = r.end - r.start;
                    let p = Arc::new(self.repo.get(seg, Some(r))?);
                    if p.len() as u64 != want {
                        return Err(Error::Corrupt(format!("short segment read in {seg}")));
                    }
                    self.cache.put_page((*seg, pi), p.clone());
                    p
                }
            };
            let s = (at - pi * PAGE) as usize;
            let n = (ENTRY - done).min(page.len() as u64 - s as u64) as usize;
            if n == 0 {
                return Err(Error::Corrupt(format!("short segment read in {seg}")));
            }
            out[done as usize..done as usize + n].copy_from_slice(&page[s..s + n]);
            done += n as u64;
        }
        Ok(out)
    }

    /// Looks up many keys against one root (e.g. every object of a push or one wave of a git
    /// walk). Same results as one [`Self::get_in`] per key, in input order.
    ///
    /// A sorted merge-join: the keys are sorted and deduplicated once, then each segment, newest
    /// first, is matched against the keys not found yet. A large segment is split recursively
    /// around probe entries until a sub-range holds about one key per page or fits one window;
    /// such a range's pages are taken from the page cache (runs of missing pages fetched with one
    /// ranged read each) and merged with its keys. So a dense batch reads each page of the
    /// segment at most once per cache lifetime, and a sparse one only the pages its keys can be
    /// in.
    pub fn get_many(&self, root: &Root, keys: &[Key]) -> Result<Vec<Option<Value>>> {
        let mut uniq: Vec<Key> = keys.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        let mut found: Vec<Option<Value>> = vec![None; uniq.len()];
        // Indices (into `uniq`, ascending) still missing.
        let mut pending: Vec<usize> = (0..uniq.len()).collect();
        for (seg, count) in root.segments.iter().rev() {
            if pending.is_empty() {
                break;
            }
            let want: Vec<(Key, usize)> = pending.iter().map(|&i| (uniq[i], i)).collect();
            self.segment_many(seg, *count, &want, &mut found)?;
            pending.retain(|&i| found[i].is_none());
        }
        Ok(keys.iter().map(|k| uniq.binary_search(k).ok().and_then(|i| found[i])).collect())
    }

    /// Matches sorted `keys` (`(key, slot)`) against one segment, filling `found[slot]`.
    fn segment_many(&self, seg: &Hash, count: u64, keys: &[(Key, usize)], found: &mut [Option<Value>]) -> Result<()> {
        if count <= CACHE_WHOLE {
            let entries = self.small_segment(seg, count)?;
            merge_join(entries.iter().map(|e| Ok((e.0, e.1))), keys, found)?;
            return Ok(());
        }
        self.range_many(seg, count, 0, count, keys, found)
    }

    /// Entries `lo..hi` of a large segment against the sorted keys that can only be there.
    fn range_many(
        &self,
        seg: &Hash,
        count: u64,
        lo: u64,
        hi: u64,
        keys: &[(Key, usize)],
        found: &mut [Option<Value>],
    ) -> Result<()> {
        if keys.is_empty() || lo >= hi {
            return Ok(());
        }
        let span = (hi - lo) * ENTRY;
        // Scan the span when the keys are dense in it (about a page per key or more), or it is
        // a single page; otherwise split. (Scanning every span up to `SCAN_WINDOW` made a lookup
        // of a few keys, such as one tree's children in a reachability closure, decode up to
        // ~58k entries.)
        if span <= PAGE || (keys.len() as u64).saturating_mul(PAGE) >= span {
            let (first, pages) = self.span_pages(seg, count, lo, hi)?;
            let entries = (lo..hi).map(|i| span_entry(&pages, first, i).map(|b| decode_entry(&b)));
            return merge_join(entries, keys, found);
        }
        let mid = lo + (hi - lo) / 2;
        let (k, v) = decode_entry(&self.entry_bytes(seg, count, mid)?);
        let split = keys.partition_point(|(x, _)| *x < k);
        let (left, rest) = keys.split_at(split);
        let right = match rest.first() {
            Some((x, slot)) if *x == k => {
                found[*slot] = Some(v);
                &rest[1..]
            }
            _ => rest,
        };
        self.range_many(seg, count, lo, mid, left, found)?;
        self.range_many(seg, count, mid + 1, hi, right, found)
    }

    /// The pages covering entries `lo..hi` of a large segment, through the page cache: runs of
    /// missing pages are fetched with one ranged read each (at most [`SCAN_WINDOW`] bytes) and
    /// cached. Returns the first page index and the pages.
    fn span_pages(&self, seg: &Hash, count: u64, lo: u64, hi: u64) -> Result<(u64, Vec<Arc<Vec<u8>>>)> {
        let size = SEG_HEADER + count * ENTRY;
        let (pa, pb) = ((SEG_HEADER + lo * ENTRY) / PAGE, (SEG_HEADER + hi * ENTRY - 1) / PAGE);
        let mut pages: Vec<Option<Arc<Vec<u8>>>> = (pa..=pb).map(|pi| self.cache.page(&(*seg, pi))).collect();
        let mut k = 0usize;
        while k < pages.len() {
            if pages[k].is_some() {
                k += 1;
                continue;
            }
            // A run of missing pages, bounded by the window.
            let mut e = k;
            while e < pages.len() && pages[e].is_none() && ((e - k + 1) as u64) * PAGE <= SCAN_WINDOW {
                e += 1;
            }
            let start = (pa + k as u64) * PAGE;
            let end = ((pa + e as u64) * PAGE).min(size);
            let b = self.repo.get(seg, Some(start..end))?;
            if b.len() as u64 != end - start {
                return Err(Error::Corrupt(format!("short segment read in {seg}")));
            }
            for (j, chunk) in b.chunks(PAGE as usize).enumerate() {
                let p = Arc::new(chunk.to_vec());
                self.cache.put_page((*seg, pa + (k + j) as u64), p.clone());
                pages[k + j] = Some(p);
            }
            k = e;
        }
        Ok((pa, pages.into_iter().map(|p| p.unwrap_or_default()).collect()))
    }

    /// A small segment, decoded whole through the segment cache.
    fn small_segment(&self, seg: &Hash, count: u64) -> Result<Segment> {
        if let Some(e) = self.cache.get(seg) {
            return Ok(e);
        }
        let e = Arc::new(decode_segment(&self.repo.get(seg, None)?)?);
        if e.len() as u64 != count {
            return Err(Error::Corrupt(format!("segment {seg} has {} entries, root says {count}", e.len())));
        }
        self.cache.put(*seg, e.clone());
        Ok(e)
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
    /// (weak entry collected by GC) is a miss, and a hit is made safe to
    /// reference: `ensure_live` rewrites the object if GC has condemned its
    /// only copy, so callers may point new names at it without re-putting it.
    pub fn get_live_blocking(&self, key: &Key) -> Result<Option<Value>> {
        match self.get_blocking(key)? {
            Some(v) if self.repo.ensure_live(&v.hash)? => Ok(Some(v)),
            _ => Ok(None),
        }
    }

    /// Inserts entries (blocking). Existing keys must map to the same hash,
    /// otherwise this fails with `Corrupt`. Returns how many were new.
    pub fn insert_blocking(&self, mut entries: Vec<(Key, Value)>) -> Result<usize> {
        let _g = self.write_lock.lock().map_err(|_| Error::Corrupt("id map writer lock poisoned".into()))?;
        entries.sort_by_key(|e| e.0);
        entries.dedup_by(|a, b| a.0 == b.0);
        for attempt in 0..64u32 {
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
                    // Another process won; back off (jittered, capped) and rebase.
                    let cap = 1u64 << attempt.min(6);
                    std::thread::sleep(std::time::Duration::from_millis(jitter(cap.max(2))));
                }
            }
        }
        Err(Error::Backend { transient: true, msg: format!("{}: too much update contention", self.name) })
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
/// Merges sorted segment entries with sorted, unique keys, filling `found[slot]` for each match.
fn merge_join(
    entries: impl Iterator<Item = Result<(Key, Value)>>,
    keys: &[(Key, usize)],
    found: &mut [Option<Value>],
) -> Result<()> {
    let mut ki = 0;
    for e in entries {
        if ki == keys.len() {
            break;
        }
        let (k, v) = e?;
        while ki < keys.len() && keys[ki].0 < k {
            ki += 1;
        }
        if ki < keys.len() && keys[ki].0 == k {
            found[keys[ki].1] = Some(v);
            ki += 1;
        }
    }
    Ok(())
}

/// Entry `i` from the pages of a span (`pages[0]` is page `first`); entries may straddle pages.
fn span_entry(pages: &[Arc<Vec<u8>>], first: u64, i: u64) -> Result<[u8; ENTRY as usize]> {
    let mut out = [0u8; ENTRY as usize];
    let mut at = SEG_HEADER + i * ENTRY;
    let mut done = 0usize;
    while done < ENTRY as usize {
        let page = pages
            .get((at / PAGE - first) as usize)
            .ok_or_else(|| Error::Corrupt("segment entry outside its span".into()))?;
        let s = (at % PAGE) as usize;
        let n = (ENTRY as usize - done).min(page.len().saturating_sub(s));
        if n == 0 {
            return Err(Error::Corrupt("short segment page".into()));
        }
        out[done..done + n].copy_from_slice(&page[s..s + n]);
        done += n;
        at += n as u64;
    }
    Ok(out)
}

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
    use crate::LocalRepo;

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
    fn large_segment_pages_find_every_entry() {
        let (_d, repo) = repo();
        let m = SegMap::new(repo, Name::new("t/pages").unwrap(), Arc::default());
        let n = CACHE_WHOLE + 3000;
        m.insert_blocking((0..n).map(|i| (key(i * 2), val(i))).collect()).unwrap();
        let (_, root) = m.root_blocking().unwrap();
        // 72-byte entries straddle 64 KiB pages regularly; every one must be found.
        let keys: Vec<Key> = (0..n).map(|i| key(i * 2)).collect();
        let got = m.get_many(&root, &keys).unwrap();
        for (i, g) in got.iter().enumerate() {
            assert_eq!(*g, Some(val(i as u64)), "entry {i}");
        }
        let misses: Vec<Key> = (0..200).map(|i| key(i * 2 + 1)).collect();
        assert!(m.get_many(&root, &misses).unwrap().iter().all(Option::is_none));
    }

    /// Builds a root directly from segments (oldest first), so newer segments can override keys
    /// (which `insert_blocking` refuses) and sizes straddle `CACHE_WHOLE`.
    fn root_of(repo: &Arc<dyn Repository>, segs: &[Vec<(Key, Value)>]) -> Root {
        Root {
            segments: segs
                .iter()
                .map(|e| {
                    let mut e = e.clone();
                    e.sort_by_key(|x| x.0);
                    e.dedup_by_key(|x| x.0);
                    (repo.put(&encode_segment(&e)).unwrap(), e.len() as u64)
                })
                .collect(),
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 24, ..Default::default() })]
        /// `get_many` = one `get_in` per key: several segments (small and large), keys
        /// overridden by newer segments, misses, duplicates and arbitrary order.
        #[test]
        fn get_many_matches_per_key_lookups(
            seed in proptest::prelude::any::<u64>(),
            sizes in proptest::collection::vec(
                proptest::prop_oneof![0u64..200, 1000u64..5000, (CACHE_WHOLE + 1)..(CACHE_WHOLE * 3)], 1..5),
            nq in 0usize..3000,
            dense in proptest::prelude::any::<bool>(),
        ) {
            let (_d, repo) = repo();
            let mut rng = seed | 1;
            let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
            let universe = sizes.iter().sum::<u64>() * 2 + 10;
            let segs: Vec<Vec<(Key, Value)>> = sizes
                .iter()
                .enumerate()
                .map(|(si, &n)| (0..n).map(|_| { let i = next() % universe; (key(i), Value { hash: Hash::of(&i.to_le_bytes()), aux: si as u64 * 1_000_000 + i }) }).collect())
                .collect();
            let root = root_of(&repo, &segs);
            let m = SegMap::new(repo, Name::new("t/prop").unwrap(), Arc::default());
            // Dense batches take the scan path; sparse ones the probe path.
            let nq = if dense { nq * 20 } else { nq };
            let qs: Vec<Key> = (0..nq).map(|_| key(next() % universe)).collect();
            let want: Vec<Option<Value>> = qs.iter().map(|k| m.get_in(&root, k).unwrap()).collect();
            proptest::prop_assert_eq!(m.get_many(&root, &qs).unwrap(), want);
        }
    }

    #[test]
    fn get_many_overrides_and_edges() {
        let (_d, repo) = repo();
        let big = CACHE_WHOLE + 5000;
        let old: Vec<_> = (0..big).map(|i| (key(i * 2), val(i))).collect();
        let new: Vec<_> = (0..big).step_by(3).map(|i| (key(i * 2), Value { hash: Hash::of(b"new"), aux: i })).collect();
        let root = root_of(&repo, &[old, new, vec![(key(1), val(1))]]);
        let m = SegMap::new(repo, Name::new("t/edges").unwrap(), Arc::default());
        let mut qs: Vec<Key> =
            vec![key(0), key(2 * (big - 1)), key(1), key(1), key(2 * big + 1), [0u8; 32], [0xff; 32]];
        qs.extend((0..big).step_by(997).map(|i| key(i * 2)));
        let want: Vec<_> = qs.iter().map(|k| m.get_in(&root, k).unwrap()).collect();
        assert_eq!(m.get_many(&root, &qs).unwrap(), want);
        assert_eq!(want[0].unwrap().hash, Hash::of(b"new"));
        assert_eq!(want[2], Some(val(1)));
        assert_eq!(want[4], None);
        assert!(m.get_many(&root, &[]).unwrap().is_empty());
        assert!(m.get_many(&Root::default(), &qs).unwrap().iter().all(Option::is_none));
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
