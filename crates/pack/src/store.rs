//! `PackStore`, an [`ObjectStore`] on top of append-only packs.
//!
//! # Layout
//! ```text
//! <staging>/<id>.pack        open packs, one per writer handle, each flock'ed by its owner
//! backend: packs/<P>.pack    sealed packs, named by the BLAKE3 of their bytes
//! backend: packs/<P>.idx     sorted index for P. Its existence is the commit point of a seal
//! ```
//!
//! # Write path
//! Objects under `chunk_threshold` bytes are appended as a single blob record
//! (bundling). Larger objects are cut with FastCDC. Each chunk becomes a blob
//! record, deduplicated by chunk hash, and a manifest record is stored under
//! the object hash. Records accumulate in this handle's staging file. The
//! open pack is sealed when it reaches `pack_cap` or on [`ObjectStore::flush`]:
//! fsync, hash, `put_if_absent(.pack)`, `put_if_absent(.idx)`, then unlink
//! the staging file. Names are content-derived, so repeating or racing a
//! seal is harmless.
//!
//! # Multi-process safety
//! Every handle writes only its own staging file, under an exclusive `flock`.
//! Sealed blobs are create-only. Readers learn about other writers' packs by
//! re-listing `packs/` when a lookup misses.
//!
//! # Crash recovery
//! When a handle opens, it adopts every staging file whose lock it can take
//! (the owner died). It truncates the torn tail at the first incomplete or
//! corrupt record and seals what's left. A seal that crashed after
//! uploading is completed idempotently.

use crate::format::*;
use pico2peta_core::backend::{Backend, PutOutcome, PutSource, fsync_dir};
use pico2peta_core::fault::{FaultHandle, Faults};
use pico2peta_core::{Error, Hash, ObjectStore, Result, check_range};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Default [`PackConfig::manifest_fanout`]: 4096 entries (~160 KiB per node).
/// Two levels cover 2^24 chunks (~1 TiB at 64 KiB average chunks).
pub const DEFAULT_MANIFEST_FANOUT: usize = 4096;

/// Tunables for the pack store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackConfig {
    /// Objects at least this large are chunked.
    pub chunk_threshold: usize,
    /// FastCDC minimum chunk size.
    pub cdc_min: u32,
    /// FastCDC average chunk size.
    pub cdc_avg: u32,
    /// FastCDC maximum chunk size (must be below `chunk_threshold`).
    pub cdc_max: u32,
    /// The open pack is sealed once it reaches this many bytes.
    pub pack_cap: u64,
    /// zstd level for record payloads. 0 disables compression.
    pub zstd_level: i32,
    /// Maximum entries per manifest node. Larger objects get a tree of
    /// manifests, so no manifest grows without bound.
    pub manifest_fanout: usize,
}

impl Default for PackConfig {
    fn default() -> Self {
        PackConfig {
            chunk_threshold: 1 << 20,
            cdc_min: 16 << 10,
            cdc_avg: 64 << 10,
            cdc_max: 256 << 10,
            pack_cap: 256 << 20,
            zstd_level: 3,
            manifest_fanout: DEFAULT_MANIFEST_FANOUT,
        }
    }
}

impl PackConfig {
    /// Tiny sizes, so that tests exercise chunking and sealing.
    pub fn tiny() -> Self {
        PackConfig {
            chunk_threshold: 2048,
            cdc_min: 64,
            cdc_avg: 256,
            cdc_max: 1024,
            pack_cap: 16 << 10,
            zstd_level: 1,
            manifest_fanout: 4,
        }
    }

    /// Checks invariants: valid FastCDC bounds, and chunks always smaller than the threshold.
    pub fn validate(&self) -> Result<()> {
        let ok = self.cdc_min >= fastcdc::v2020::MINIMUM_MIN
            && self.cdc_avg >= fastcdc::v2020::AVERAGE_MIN
            && self.cdc_max >= fastcdc::v2020::MAXIMUM_MIN
            && self.cdc_min <= fastcdc::v2020::MINIMUM_MAX
            && self.cdc_avg <= fastcdc::v2020::AVERAGE_MAX
            && self.cdc_max <= fastcdc::v2020::MAXIMUM_MAX
            && self.cdc_min <= self.cdc_avg
            && self.cdc_avg <= self.cdc_max
            && (self.cdc_max as usize) < self.chunk_threshold
            && self.chunk_threshold <= (u32::MAX / 2) as usize
            && self.pack_cap >= 1024
            && (-7..=22).contains(&self.zstd_level)
            && (2..=(1 << 20)).contains(&self.manifest_fanout);
        if ok { Ok(()) } else { Err(Error::Parse(format!("invalid pack config {self:?}"))) }
    }
}

/// Where a record lives.
#[derive(Clone, Copy, Debug)]
enum PackRef {
    Sealed(u32),
    /// The open pack of this handle, identified by generation.
    Open(u64),
}

#[derive(Clone, Copy, Debug)]
struct Loc {
    pack: PackRef,
    offset: u64,
    stored_len: u32,
    raw_len: u64,
    kind: Kind,
    /// `(inner offset, frame length)` when the object lives in a frame record.
    frame: Option<(u32, u32)>,
}

/// Summary counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackStats {
    /// Sealed packs known to this handle.
    pub sealed_packs: usize,
    /// Addressable hashes (objects, chunks, manifest nodes).
    pub objects: usize,
    /// Of which blobs (including chunks and frame members).
    pub blobs: usize,
    /// Of which manifest nodes.
    pub manifests: usize,
    /// Bytes in this handle's open (unsealed) pack.
    pub open_bytes: u64,
    /// Sum of blob lengths.
    pub logical_blob_bytes: u64,
}

/// A large sealed pack whose index is memory-mapped instead of loaded.
struct BigPack {
    pid: u32,
    name: String,
    view: IdxV3View<memmap2::Mmap>,
}

/// An installed multi-pack index and its mapping onto `Index::big`.
struct MidxState {
    view: MidxView<memmap2::Mmap>,
    /// For each MIDX pack ordinal, the position in `Index::big` (None if gone).
    ord_to_big: Vec<Option<usize>>,
    /// Per `Index::big` position: covered by the MIDX (so not searched individually).
    covered: Vec<bool>,
}

/// Build a multi-pack index once there are at least this many large packs...
const MIDX_MIN_PACKS: usize = 8;

#[derive(Default)]
struct Index {
    /// Locations from small sealed packs and from this handle's open pack.
    map: HashMap<Hash, Loc>,
    /// Large sealed packs, searched through their mapped v3 indexes.
    big: Vec<Arc<BigPack>>,
    /// Multi-pack index over (most of) `big`.
    midx: Option<MidxState>,
    /// Sealed pack names (hex), indexed by `PackRef::Sealed`.
    sealed: Vec<String>,
    known: HashSet<String>,
    /// Packs condemned by GC (still readable, scheduled for deletion).
    condemned: HashSet<String>,
    /// Parsed `gc/*.condemned` records, by key.
    condemned_records: HashMap<String, Vec<String>>,
    last_refresh: Option<std::time::Instant>,
}

fn loc_of(pid: u32, e: &IdxEntry) -> Loc {
    Loc {
        pack: PackRef::Sealed(pid),
        offset: e.offset,
        stored_len: e.stored_len,
        raw_len: e.raw_len,
        kind: e.kind,
        frame: e.frame,
    }
}

impl Index {
    /// The best location of `hash` (see [`rank`]) across the map and the
    /// mapped large-pack indexes.
    fn get(&self, hash: &Hash) -> Option<Loc> {
        let mut best = self.map.get(hash).copied();
        if best.as_ref().is_some_and(|l| rank(self, l) == 0) {
            return best;
        }
        let consider = |l: Loc, best: &mut Option<Loc>| -> bool {
            if best.as_ref().is_none_or(|old| rank(self, &l) < rank(self, old)) {
                *best = Some(l);
            }
            best.as_ref().is_some_and(|b| rank(self, b) == 0)
        };
        if let Some(m) = &self.midx {
            for (ord, ent) in m.view.find_all(hash) {
                if let Some(Some(bi)) = m.ord_to_big.get(ord as usize)
                    && let Some(b) = self.big.get(*bi)
                    && let Ok(e) = b.view.entry(ent as usize)
                    && e.hash == *hash
                    && consider(loc_of(b.pid, &e), &mut best)
                {
                    return best;
                }
            }
        }
        for (bi, b) in self.big.iter().enumerate() {
            if self.midx.as_ref().is_some_and(|m| m.covered.get(bi).copied().unwrap_or(false)) {
                continue;
            }
            if let Ok(Some(e)) = b.view.find(hash)
                && consider(loc_of(b.pid, &e), &mut best)
            {
                break;
            }
        }
        best
    }
}

/// Location preference: live sealed < own open pack < condemned sealed.
fn rank(idx: &Index, l: &Loc) -> u8 {
    match l.pack {
        PackRef::Sealed(pid) if idx.sealed.get(pid as usize).is_none_or(|n| idx.condemned.contains(n)) => 2,
        PackRef::Sealed(_) => 0,
        PackRef::Open(_) => 1,
    }
}

fn insert_loc(idx: &mut Index, hash: Hash, loc: Loc) {
    let better = match idx.get(&hash) {
        Some(old) => rank(idx, &loc) < rank(idx, &old),
        None => true,
    };
    if better {
        idx.map.insert(hash, loc);
    }
}

/// Interval after which writers re-list before trusting a dedup hit, so that
/// they learn about GC condemnations promptly. GC grace periods must be much
/// longer than this (see `pico2peta-repo` GC).
pub const DEDUP_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Prefix of GC records on the backend.
pub const GC_PREFIX: &str = "gc/";

struct OpenPack {
    file: Arc<File>,
    path: PathBuf,
    generation: u64,
    len: u64,
    entries: Vec<IdxEntry>,
}

struct Inner {
    cfg: PackConfig,
    backend: Arc<dyn Backend>,
    staging_dir: PathBuf,
    faults: FaultHandle,
    index: RwLock<Index>,
    writer: Mutex<Option<OpenPack>>,
    generation: AtomicU64,
    dedup_refresh_ms: AtomicU64,
    /// See [`PackStore::set_mmap_index_threshold`].
    mmap_index_bytes: AtomicU64,
    /// Recently decompressed frames: `((frame hash, record offset), bodies)`.
    frame_cache: Mutex<Vec<FrameCacheEntry>>,
    /// Recently used manifest nodes (immutable, so they can be cached freely).
    manifest_cache: Mutex<Vec<ManifestCacheEntry>>,
}

type FrameCacheEntry = ((Hash, u64), Arc<Vec<u8>>);
type ManifestCacheEntry = (Hash, Arc<Vec<(Hash, u64)>>);

/// Manifest nodes cached per handle (each up to `manifest_fanout` × 40 B).
const MANIFEST_CACHE: usize = 16;

/// Frames kept decompressed per handle (at most ~4 MiB each by default).
const FRAME_CACHE: usize = 8;

impl Inner {
    fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.faults.hit(point)?)
    }

    /// Seals a fully written, locked staging file: fsync, upload pack, upload
    /// index, unlink. Idempotent and crash-safe at every step.
    fn seal_file(&self, path: &Path, f: &File, len: u64, mut entries: Vec<IdxEntry>) -> Result<String> {
        self.fault("seal.fsync")?;
        f.sync_all()?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut off = 0u64;
        while off < len {
            let n = buf.len().min((len - off) as usize);
            f.read_exact_at(&mut buf[..n], off)?;
            hasher.update(&buf[..n]);
            off += n as u64;
        }
        let name = hasher.finalize().to_hex().to_string();
        self.fault("seal.put-pack")?;
        self.backend.put_if_absent(&format!("packs/{name}.pack"), PutSource::File(path))?;
        let idx = encode_index(&mut entries);
        self.fault("seal.put-idx")?;
        let _: PutOutcome = self.backend.put_if_absent(&format!("packs/{name}.idx"), PutSource::Bytes(&idx))?;
        self.fault("seal.unlink")?;
        fs::remove_file(path)?;
        fsync_dir(&self.staging_dir)?;
        Ok(name)
    }
}

/// See the module docs. Cheap to clone (shares state).
#[derive(Clone)]
pub struct PackStore {
    inner: Arc<Inner>,
}

fn poisoned<T>(_: T) -> Error {
    Error::Corrupt("internal lock poisoned".into())
}

fn unique_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&t.to_le_bytes());
    h.update(&SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    h.finalize().to_hex()[..20].to_string()
}

impl PackStore {
    /// Opens a store with staging under `staging_dir` and sealed packs on
    /// `backend`, and recovers orphaned staging files.
    pub fn open(staging_dir: impl AsRef<Path>, backend: Arc<dyn Backend>, cfg: PackConfig) -> Result<Self> {
        Self::open_with_faults(staging_dir, backend, cfg, Faults::disarmed())
    }

    /// Like [`PackStore::open`], with fault injection (for tests).
    pub fn open_with_faults(
        staging_dir: impl AsRef<Path>,
        backend: Arc<dyn Backend>,
        cfg: PackConfig,
        faults: FaultHandle,
    ) -> Result<Self> {
        cfg.validate()?;
        let staging_dir = staging_dir.as_ref().to_path_buf();
        fs::create_dir_all(&staging_dir)?;
        let s = PackStore {
            inner: Arc::new(Inner {
                cfg,
                backend,
                staging_dir,
                faults,
                index: RwLock::new(Index::default()),
                writer: Mutex::new(None),
                generation: AtomicU64::new(1),
                dedup_refresh_ms: AtomicU64::new(DEDUP_REFRESH_INTERVAL.as_millis() as u64),
                // P2P_MMAP_INDEX_BYTES overrides the default (tests use 0 to map every index).
                mmap_index_bytes: AtomicU64::new(
                    std::env::var("P2P_MMAP_INDEX_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(4 << 20),
                ),
                frame_cache: Mutex::new(Vec::new()),
                manifest_cache: Mutex::new(Vec::new()),
            }),
        };
        s.refresh()?;
        s.recover_orphans()?;
        Ok(s)
    }

    /// The configuration this store was opened with.
    pub fn config(&self) -> &PackConfig {
        &self.inner.cfg
    }

    fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.inner.faults.hit(point)?)
    }

    /// Re-lists the backend: loads indexes of new sealed packs, forgets
    /// deleted ones, and picks up GC condemnations. Returns the number of
    /// newly loaded packs.
    pub fn refresh(&self) -> Result<usize> {
        let listed = self.inner.backend.list("packs/")?;
        let keys: HashSet<&str> = listed.iter().map(|m| m.key.as_str()).collect();
        let live_idx: Vec<String> = listed
            .iter()
            .filter_map(|m| m.key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx")))
            .map(|s| s.to_string())
            .collect();
        for name in &live_idx {
            if !keys.contains(format!("packs/{name}.pack").as_str()) {
                return Err(Error::Corrupt(format!("index without pack: {name}")));
            }
        }
        // GC condemnation records (small, create-only, cached by key).
        let gc_keys: Vec<String> = self
            .inner
            .backend
            .list(GC_PREFIX)?
            .into_iter()
            .map(|m| m.key)
            .filter(|k| k.ends_with(".condemned"))
            .collect();
        let (removed, mut records) = {
            let idx = self.inner.index.read().map_err(poisoned)?;
            let listed_set: HashSet<&String> = live_idx.iter().collect();
            let removed = idx.known.iter().any(|k| !listed_set.contains(k));
            let records: HashMap<String, Vec<String>> =
                gc_keys.iter().filter_map(|k| idx.condemned_records.get(k).map(|v| (k.clone(), v.clone()))).collect();
            (removed, records)
        };
        for k in &gc_keys {
            if !records.contains_key(k) {
                match read_name_list(self.inner.backend.as_ref(), k) {
                    Ok(v) => {
                        records.insert(k.clone(), v);
                    }
                    Err(Error::KeyNotFound(_)) => {} // deleted concurrently
                    Err(e) => return Err(e),
                }
            }
        }
        let condemned: HashSet<String> = records.values().flatten().cloned().collect();
        let rebuild = {
            let idx = self.inner.index.read().map_err(poisoned)?;
            removed || idx.condemned != condemned
        };
        let to_load: Vec<String> = if rebuild {
            live_idx.clone()
        } else {
            let idx = self.inner.index.read().map_err(poisoned)?;
            live_idx.iter().filter(|n| !idx.known.contains(*n)).cloned().collect()
        };
        let mut loaded = Vec::new();
        let mut big_loaded = Vec::new();
        let threshold = self.inner.mmap_index_bytes.load(Ordering::Relaxed);
        for name in to_load {
            let key = format!("packs/{name}.idx");
            let size = match self.inner.backend.head(&key)? {
                Some(m) => m.len,
                None => continue, // deleted by GC between list and read
            };
            if size >= threshold {
                match self.map_index(&name, &key) {
                    Ok(view) => {
                        big_loaded.push((name, view));
                        continue;
                    }
                    Err(Error::KeyNotFound(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            let mut buf = Vec::new();
            match self.inner.backend.open(&key) {
                Ok(mut r) => {
                    r.read_to_end(&mut buf)?;
                }
                Err(Error::KeyNotFound(_)) => continue,
                Err(e) => return Err(e),
            }
            loaded.push((name, decode_index(&buf)?));
        }
        let mut idx = self.inner.index.write().map_err(poisoned)?;
        if rebuild {
            let open: Vec<(Hash, Loc)> =
                idx.map.iter().filter(|(_, l)| matches!(l.pack, PackRef::Open(_))).map(|(h, l)| (*h, *l)).collect();
            // `sealed` is append-only, so `PackRef::Sealed(pid)` held by concurrent
            // readers stays valid: positions are never reused or reordered.
            let sealed = std::mem::take(&mut idx.sealed);
            *idx = Index { condemned, condemned_records: records, sealed, ..Index::default() };
            // Drop cached indexes of packs that no longer exist (deleted by GC).
            let live: HashSet<String> = live_idx.iter().map(|n| format!("{n}.idx3")).collect();
            if let Ok(rd) = fs::read_dir(self.inner.staging_dir.with_file_name("idx-cache")) {
                for ent in rd.flatten() {
                    let fname = ent.file_name().to_string_lossy().to_string();
                    if fname.ends_with(".idx3") && !live.contains(&fname) {
                        let _ = fs::remove_file(ent.path());
                    }
                }
            }
            for (h, l) in open {
                idx.map.insert(h, l);
            }
        } else {
            idx.condemned_records = records;
        }
        let mut n = 0;
        for (name, view) in big_loaded {
            if !idx.known.insert(name.clone()) {
                continue;
            }
            n += 1;
            let pid = match idx.sealed.iter().position(|x| *x == name) {
                Some(p) => p as u32,
                None => {
                    idx.sealed.push(name.clone());
                    (idx.sealed.len() - 1) as u32
                }
            };
            idx.big.push(Arc::new(BigPack { pid, name, view }));
        }
        for (name, entries) in loaded {
            if !idx.known.insert(name.clone()) {
                continue;
            }
            n += 1;
            let pid = match idx.sealed.iter().position(|x| *x == name) {
                Some(p) => p as u32,
                None => {
                    idx.sealed.push(name);
                    (idx.sealed.len() - 1) as u32
                }
            };
            for e in entries {
                let loc = Loc {
                    pack: PackRef::Sealed(pid),
                    offset: e.offset,
                    stored_len: e.stored_len,
                    raw_len: e.raw_len,
                    kind: e.kind,
                    frame: e.frame,
                };
                insert_loc(&mut idx, e.hash, loc);
            }
        }
        idx.last_refresh = Some(std::time::Instant::now());
        let nbig = idx.big.len();
        if let Some(m) = idx.midx.as_mut() {
            // New big packs were appended: they're not covered.
            m.covered.resize(nbig, false);
        }
        drop(idx);
        self.maybe_build_midx()?;
        Ok(n)
    }

    /// Builds (or maps a cached) multi-pack index once enough large packs are
    /// uncovered, outside the index lock (the views are shared through `Arc`).
    fn maybe_build_midx(&self) -> Result<()> {
        let packs: Vec<Arc<BigPack>> = {
            let idx = self.inner.index.read().map_err(poisoned)?;
            let uncovered = match &idx.midx {
                Some(m) => m.covered.iter().filter(|c| !**c).count(),
                None => idx.big.len(),
            };
            if idx.big.len() < MIDX_MIN_PACKS || uncovered * 10 <= idx.big.len() {
                return Ok(());
            }
            idx.big.clone()
        };
        let mut names: Vec<&str> = packs.iter().map(|b| b.name.as_str()).collect();
        names.sort();
        let tag = blake3::hash(names.join("\n").as_bytes()).to_hex()[..24].to_string();
        let dir = self.inner.staging_dir.with_file_name("idx-cache");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("midx-{tag}.midx"));
        if !path.exists() {
            let mut sorted: Vec<&Arc<BigPack>> = packs.iter().collect();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            let views: Vec<(&str, &IdxV3View<memmap2::Mmap>)> =
                sorted.iter().map(|b| (b.name.as_str(), &b.view)).collect();
            let tmp = dir.join(format!(".midx-{tag}.{}.tmp", unique_id()));
            let f = File::create(&tmp)?;
            write_midx(&views, &f)?;
            f.sync_all()?;
            fs::rename(&tmp, &path)?;
            fsync_dir(&dir)?;
        }
        let f = File::open(&path)?;
        // SAFETY: as for pack index caches, MIDX files are written once
        // (temp file + rename) and never modified while mapped.
        let map = unsafe { memmap2::Mmap::map(&f)? };
        let view = match MidxView::new(map) {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_file(&path);
                return Err(e);
            }
        };
        let mut idx = self.inner.index.write().map_err(poisoned)?;
        let pos: HashMap<&str, usize> = idx.big.iter().enumerate().map(|(i, b)| (b.name.as_str(), i)).collect();
        let ord_to_big: Vec<Option<usize>> = view.names().iter().map(|n| pos.get(n.as_str()).copied()).collect();
        let mut covered = vec![false; idx.big.len()];
        for bi in ord_to_big.iter().flatten() {
            covered[*bi] = true;
        }
        drop(pos);
        idx.midx = Some(MidxState { view, ord_to_big, covered });
        drop(idx);
        // Remove superseded MIDX files.
        if let Ok(rd) = fs::read_dir(&dir) {
            for ent in rd.flatten() {
                let n = ent.file_name().to_string_lossy().to_string();
                if n.starts_with("midx-") && n.ends_with(".midx") && n != format!("midx-{tag}.midx") {
                    let _ = fs::remove_file(ent.path());
                }
            }
        }
        Ok(())
    }

    /// Fetches a (large) pack index into the local index cache, verifying it
    /// on the way, and memory-maps it. Non-v3 indexes are converted to v3.
    fn map_index(&self, name: &str, key: &str) -> Result<IdxV3View<memmap2::Mmap>> {
        let dir = self.inner.staging_dir.with_file_name("idx-cache");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.idx3"));
        if !path.exists() {
            let tmp = dir.join(format!(".{name}.{}.tmp", unique_id()));
            {
                let mut src = self.inner.backend.open(key)?;
                let mut head = [0u8; 8];
                src.read_exact(&mut head)?;
                let mut f = File::create(&tmp)?;
                if &head == IDX_MAGIC_V3 {
                    f.write_all(&head)?;
                    std::io::copy(&mut src, &mut f)?;
                } else {
                    // Old format: decode in memory once, rewrite as v3.
                    let mut buf = head.to_vec();
                    src.read_to_end(&mut buf)?;
                    let mut entries = decode_index(&buf)?;
                    f.write_all(&encode_index(&mut entries))?;
                }
                f.sync_all()?;
            }
            fs::rename(&tmp, &path)?;
            fsync_dir(&dir)?;
        }
        let f = File::open(&path)?;
        // SAFETY: cache files are written once (temp file + rename) and never
        // modified or truncated while mapped. A file damaged by external means
        // fails the checksum in `IdxV3View::new`.
        let map = unsafe { memmap2::Mmap::map(&f)? };
        match IdxV3View::new(map) {
            Ok(v) => Ok(v),
            Err(e) => {
                // A corrupt cache entry: drop it so the next refresh refetches.
                let _ = fs::remove_file(&path);
                Err(e)
            }
        }
    }

    /// Indexes at least this large (bytes) are memory-mapped rather than
    /// loaded into the heap (default 4 MiB, about 95k entries). Mainly for tests.
    pub fn set_mmap_index_threshold(&self, bytes: u64) {
        self.inner.mmap_index_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Lookup for write-side deduplication: refreshes if the local view is
    /// older than [`DEDUP_REFRESH_INTERVAL`], and ignores copies that live
    /// only in condemned packs (those are about to be deleted).
    fn dedup_hit(&self, hash: &Hash) -> Result<bool> {
        let stale = {
            let idx = self.inner.index.read().map_err(poisoned)?;
            let max = std::time::Duration::from_millis(self.inner.dedup_refresh_ms.load(Ordering::Relaxed));
            idx.last_refresh.is_none_or(|t| t.elapsed() > max)
        };
        if stale {
            self.refresh()?;
        }
        let idx = self.inner.index.read().map_err(poisoned)?;
        Ok(idx.get(hash).is_some_and(|l| rank(&idx, &l) < 2))
    }

    /// Seals every staging file whose owner is gone.
    fn recover_orphans(&self) -> Result<()> {
        let mut paths: Vec<PathBuf> = fs::read_dir(&self.inner.staging_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "pack"))
            .collect();
        paths.sort();
        for p in paths {
            let f = match OpenOptions::new().read(true).write(true).open(&p) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            match f.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => continue, // live owner
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            // We own it now. The file may have been unlinked between listing
            // and locking (another recoverer sealed it), so check it's still there.
            if !p.exists() {
                continue;
            }
            let entries = self.truncate_torn_tail(&f)?;
            if entries.is_empty() {
                self.fault("recover.unlink-empty")?;
                fs::remove_file(&p)?;
                fsync_dir(&self.inner.staging_dir)?;
                continue;
            }
            let len = f.metadata()?.len();
            self.inner.seal_file(&p, &f, len, entries)?;
        }
        self.refresh()?;
        Ok(())
    }

    /// Scans a staging file, truncates it after the last valid record and
    /// returns the index entries of the valid records.
    fn truncate_torn_tail(&self, f: &File) -> Result<Vec<IdxEntry>> {
        let len = f.metadata()?.len();
        let mut entries = Vec::new();
        let mut magic = [0u8; 8];
        let mut good = 0u64;
        let hl = if len >= 8 && f.read_exact_at(&mut magic, 0).is_ok() { pack_header_len(&magic) } else { None };
        if let Some(hl) = hl.filter(|hl| len >= *hl) {
            good = hl;
            let mut hdr = vec![0u8; REC_HEADER];
            while good + REC_HEADER as u64 <= len {
                if f.read_exact_at(&mut hdr, good).is_err() {
                    break;
                }
                let Ok(h) = parse_header(&hdr) else { break };
                if good + h.total_len() > len {
                    break;
                }
                let mut rec = vec![0u8; h.total_len() as usize];
                f.read_exact_at(&mut rec, good)?;
                let Ok((_, payload)) = decode_record(&rec) else { break };
                if h.kind.is_frame() {
                    // A CRC-valid frame with a bad directory is corruption, not a torn tail.
                    let members = frame_members(&h, payload)?;
                    for (mh, inner, mlen) in members {
                        entries.push(IdxEntry {
                            hash: mh,
                            offset: good,
                            stored_len: h.stored_len,
                            raw_len: mlen as u64,
                            kind: Kind::Blob,
                            codec: h.codec,
                            frame: Some((inner, h.raw_len as u32)),
                        });
                    }
                } else {
                    entries.push(IdxEntry {
                        hash: h.hash,
                        offset: good,
                        stored_len: h.stored_len,
                        raw_len: h.raw_len,
                        kind: h.kind,
                        codec: h.codec,
                        frame: None,
                    });
                }
                good += h.total_len();
            }
        }
        if good < len {
            self.fault("recover.truncate")?;
            f.set_len(good)?;
            f.sync_all()?;
        }
        Ok(entries)
    }

    /// Creates a fresh, locked staging file.
    fn new_open_pack(&self) -> Result<OpenPack> {
        let id = unique_id();
        let tmp = self.inner.staging_dir.join(format!("{id}.creating"));
        let path = self.inner.staging_dir.join(format!("{id}.pack"));
        self.fault("open.create")?;
        let mut f = OpenOptions::new().read(true).write(true).create_new(true).open(&tmp)?;
        f.lock()?;
        let mut header = PACK_MAGIC.to_vec();
        let nonce = blake3::hash(format!("{id}-{}-{:?}", std::process::id(), std::time::SystemTime::now()).as_bytes());
        header.extend_from_slice(&nonce.as_bytes()[..16]);
        f.write_all(&header)?;
        // Rename while holding the lock. Recovery only considers `*.pack`,
        // so a staging file is never adopted before its lock is held.
        fs::rename(&tmp, &path)?;
        fsync_dir(&self.inner.staging_dir)?;
        Ok(OpenPack {
            file: Arc::new(f),
            path,
            generation: self.inner.generation.fetch_add(1, Ordering::Relaxed),
            len: PACK_HEADER_V2 as u64,
            entries: Vec::new(),
        })
    }

    /// Compresses (if useful) and appends one record to the open pack.
    fn append(&self, w: &mut Option<OpenPack>, kind: Kind, hash: Hash, raw_len: u64, raw: &[u8]) -> Result<()> {
        let compressed = compress(self.inner.cfg.zstd_level, raw);
        let (codec, payload): (Codec, &[u8]) = match &compressed {
            Some(c) => (Codec::Zstd, c),
            None => (Codec::Raw, raw),
        };
        self.append_encoded(w, kind, hash, raw_len, codec, payload)
    }

    /// Appends an already-encoded payload (see [`compress`]).
    fn append_encoded(
        &self,
        w: &mut Option<OpenPack>,
        kind: Kind,
        hash: Hash,
        raw_len: u64,
        codec: Codec,
        payload: &[u8],
    ) -> Result<()> {
        let rec = encode_record(kind, codec, &hash, raw_len, payload);
        if w.is_none() {
            *w = Some(self.new_open_pack()?);
        }
        let op = w.as_mut().ok_or_else(|| Error::Corrupt("no open pack".into()))?;
        let offset = op.len;
        if let Err(e) = self.inner.faults.hit("append.write") {
            // Simulate a torn write: half the record reaches the file.
            let _ = op.file.write_all_at(&rec[..rec.len() / 2], offset);
            return Err(e.into());
        }
        op.file.write_all_at(&rec, offset)?;
        op.len += rec.len() as u64;
        let stored_len = payload.len() as u32;
        op.entries.push(IdxEntry { hash, offset, stored_len, raw_len, kind, codec, frame: None });
        let loc = Loc { pack: PackRef::Open(op.generation), offset, stored_len, raw_len, kind, frame: None };
        insert_loc(&mut *self.inner.index.write().map_err(poisoned)?, hash, loc);
        Ok(())
    }

    /// Seals the open pack if there is one.
    fn seal_locked(&self, w: &mut Option<OpenPack>) -> Result<()> {
        let Some(op) = w.take() else { return Ok(()) };
        let OpenPack { file, path, generation, len, entries } = op;
        let name = match self.inner.seal_file(&path, &file, len, entries.clone()) {
            Ok(n) => n,
            Err(e) => {
                // Keep the open pack so a later flush can retry. Our records stay readable.
                *w = Some(OpenPack { file, path, generation, len, entries });
                return Err(e);
            }
        };
        drop(file); // releases the flock
        // Re-point our records at the sealed pack directly. This doesn't
        // depend on a refresh, which may race with GC condemning the pack.
        let mut idx = self.inner.index.write().map_err(poisoned)?;
        idx.known.insert(name.clone());
        let pid = match idx.sealed.iter().position(|n| *n == name) {
            Some(p) => p as u32,
            None => {
                idx.sealed.push(name);
                (idx.sealed.len() - 1) as u32
            }
        };
        for e in entries {
            let loc = Loc {
                pack: PackRef::Sealed(pid),
                offset: e.offset,
                stored_len: e.stored_len,
                raw_len: e.raw_len,
                kind: e.kind,
                frame: e.frame,
            };
            match idx.map.get(&e.hash) {
                Some(Loc { pack: PackRef::Open(g), .. }) if *g == generation => {
                    idx.map.insert(e.hash, loc);
                }
                _ => insert_loc(&mut idx, e.hash, loc),
            }
        }
        Ok(())
    }

    /// Seals the open pack. Afterwards every object written by this handle is
    /// durable and visible to other processes.
    pub fn seal(&self) -> Result<()> {
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        self.seal_locked(&mut w)
    }

    fn maybe_seal(&self, w: &mut Option<OpenPack>) -> Result<()> {
        if w.as_ref().is_some_and(|op| op.len >= self.inner.cfg.pack_cap) { self.seal_locked(w) } else { Ok(()) }
    }

    fn lookup_local(&self, hash: &Hash) -> Result<Option<Loc>> {
        Ok(self.inner.index.read().map_err(poisoned)?.get(hash))
    }

    /// Looks `hash` up, re-listing the backend once on a miss.
    fn lookup(&self, hash: &Hash) -> Result<Option<Loc>> {
        if let Some(l) = self.lookup_local(hash)? {
            return Ok(Some(l));
        }
        if self.refresh()? > 0 { self.lookup_local(hash) } else { Ok(None) }
    }

    /// Reads the raw bytes of `hash`'s record (CRC verified), following
    /// relocations caused by seals and GC.
    fn read_raw(&self, hash: &Hash, mut loc: Loc) -> Result<(RecHeader, Vec<u8>, Loc)> {
        for _ in 0..4 {
            let total = (REC_HEADER + loc.stored_len as usize + REC_TRAILER) as u64;
            let raw = match loc.pack {
                PackRef::Sealed(pid) => {
                    let name = self.inner.index.read().map_err(poisoned)?.sealed.get(pid as usize).cloned();
                    let Some(name) = name else { return Err(Error::Corrupt(format!("unknown pack id {pid}"))) };
                    match self.inner.backend.get_range(&format!("packs/{name}.pack"), loc.offset..loc.offset + total) {
                        Ok(b) => b,
                        Err(Error::KeyNotFound(_)) => {
                            // Deleted by GC: the live copy was rescued elsewhere.
                            self.refresh()?;
                            loc = self.lookup_local(hash)?.ok_or(Error::NotFound(*hash))?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                PackRef::Open(generation) => {
                    let file = {
                        let w = self.inner.writer.lock().map_err(poisoned)?;
                        w.as_ref().filter(|op| op.generation == generation).map(|op| op.file.clone())
                    };
                    match file {
                        Some(f) => {
                            let mut buf = vec![0u8; total as usize];
                            f.read_exact_at(&mut buf, loc.offset)?;
                            buf
                        }
                        None => {
                            // Sealed in the meantime. Re-resolve.
                            loc = self.lookup_local(hash)?.ok_or(Error::NotFound(*hash))?;
                            continue;
                        }
                    }
                }
            };
            let (h, _) = decode_record(&raw)?;
            let framed = loc.frame.is_some();
            if framed != h.kind.is_frame() || (!framed && h.hash != *hash) {
                return Err(Error::Corrupt(format!("record at index location holds {} not {hash}", h.hash)));
            }
            return Ok((h, raw, loc));
        }
        Err(Error::Corrupt(format!("could not resolve location of {hash}")))
    }

    fn read_record(&self, hash: &Hash, loc: Loc) -> Result<(RecHeader, Vec<u8>)> {
        let (h, raw, loc) = self.read_raw(hash, loc)?;
        let (_, payload) = decode_record(&raw)?;
        if let Some((inner, _)) = loc.frame {
            let bodies = self.frame_bodies(&h, payload, &loc)?;
            let (a, b) = (inner as usize, inner as usize + loc.raw_len as usize);
            let d = bodies.get(a..b).ok_or_else(|| Error::Corrupt(format!("frame member {hash} out of range")))?;
            return Ok((h, d.to_vec()));
        }
        Ok((h, unpack_payload(&h, payload)?))
    }

    /// Decompressed frame bodies, through a small cache (recently used
    /// frames). Sequential reads of grouped objects then decompress each frame once.
    fn frame_bodies(&self, h: &RecHeader, payload: &[u8], loc: &Loc) -> Result<Arc<Vec<u8>>> {
        let key = (h.hash, loc.offset);
        {
            let c = self.inner.frame_cache.lock().map_err(poisoned)?;
            if let Some((_, v)) = c.iter().find(|(k, _)| *k == key) {
                return Ok(v.clone());
            }
        }
        let v = Arc::new(decode_frame_bodies(h, payload)?);
        let mut c = self.inner.frame_cache.lock().map_err(poisoned)?;
        if c.len() >= FRAME_CACHE {
            c.remove(0);
        }
        c.push((key, v.clone()));
        Ok(v)
    }

    fn read_blob(&self, hash: &Hash, loc: Loc) -> Result<Vec<u8>> {
        let (_, data) = self.read_record(hash, loc)?;
        if Hash::of(&data) != *hash {
            return Err(Error::Corrupt(format!("content hash mismatch for {hash}")));
        }
        Ok(data)
    }

    fn read_manifest(&self, hash: &Hash, loc: Loc) -> Result<Vec<(Hash, u64)>> {
        {
            let c = self.inner.manifest_cache.lock().map_err(poisoned)?;
            if let Some((_, m)) = c.iter().find(|(k, _)| k == hash) {
                return Ok(m.as_ref().clone());
            }
        }
        let m = self.read_manifest_uncached(hash, loc)?;
        let mut c = self.inner.manifest_cache.lock().map_err(poisoned)?;
        if c.len() >= MANIFEST_CACHE {
            c.remove(0);
        }
        c.push((*hash, Arc::new(m.clone())));
        Ok(m)
    }

    fn read_manifest_uncached(&self, hash: &Hash, loc: Loc) -> Result<Vec<(Hash, u64)>> {
        let (_, data) = self.read_record(hash, loc)?;
        let m = decode_manifest(&data)?;
        if m.iter().try_fold(0u64, |a, c| a.checked_add(c.1)) != Some(loc.raw_len) {
            return Err(Error::Corrupt(format!("manifest length mismatch for {hash}")));
        }
        Ok(m)
    }

    fn put_chunks<I>(&self, chunks: I) -> Result<Hash>
    where
        I: Iterator<Item = Result<Vec<u8>>>,
    {
        // A tree of manifests. Every node is the manifest of a contiguous range
        // of the object and is keyed by the BLAKE3 of that range, so every
        // index key is still the content hash of what `get` returns for it.
        let fanout = self.inner.cfg.manifest_fanout;
        let mut whole = blake3::Hasher::new();
        let mut levels: Vec<Level> = vec![Level { hasher: blake3::Hasher::new(), entries: Vec::new(), len: 0 }];
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
        let level = self.inner.cfg.zstd_level;
        let mut chunks = chunks.peekable();
        while chunks.peek().is_some() {
            // Read a batch, then hash and compress its chunks in parallel.
            let mut batch: Vec<Vec<u8>> = Vec::with_capacity(CHUNK_BATCH);
            while batch.len() < CHUNK_BATCH {
                match chunks.next() {
                    Some(c) => batch.push(c?),
                    None => break,
                }
            }
            let mut enc: Vec<(Hash, Option<Vec<u8>>)> = vec![(Hash([0; 32]), None); batch.len()];
            let per = batch.len().div_ceil(workers);
            std::thread::scope(|sc| {
                for (bs, es) in batch.chunks(per.max(1)).zip(enc.chunks_mut(per.max(1))) {
                    sc.spawn(move || {
                        for (b, e) in bs.iter().zip(es.iter_mut()) {
                            let h = Hash::of(b);
                            // Skip compressing chunks we already have (dedup).
                            let known = matches!(self.lookup_local(&h), Ok(Some(_)));
                            *e = (h, if known { None } else { compress(level, b) });
                        }
                    });
                }
            });
            for (c, (ch, z)) in batch.iter().zip(enc) {
                self.add_chunk(c, ch, z, &mut whole, &mut levels, fanout)?;
            }
        }
        let hash = Hash(*whole.finalize().as_bytes());
        // Close partial nodes below the top.
        let top = levels.len() - 1;
        for i in 0..top {
            if !levels[i].entries.is_empty() {
                let lv = &mut levels[i];
                let node = self.emit_node(&mut lv.hasher, &mut lv.entries, &mut lv.len)?;
                levels[i + 1].entries.push(node);
                levels[i + 1].len += node.1;
            }
        }
        let root = &levels[top];
        if root.entries.len() == 1 && root.entries[0].0 == hash {
            return Ok(hash); // the single child already is the whole object
        }
        if !self.dedup_hit(&hash)? {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            self.append(&mut w, Kind::Manifest, hash, root.len, &encode_manifest(&root.entries))?;
            self.maybe_seal(&mut w)?;
        }
        Ok(hash)
    }

    /// Adds one chunk (already hashed and maybe compressed) to the tree.
    fn add_chunk(
        &self,
        c: &[u8],
        ch: Hash,
        z: Option<Vec<u8>>,
        whole: &mut blake3::Hasher,
        levels: &mut Vec<Level>,
        fanout: usize,
    ) -> Result<()> {
        whole.update(c);
        for l in levels.iter_mut() {
            l.hasher.update(c);
        }
        if !self.dedup_hit(&ch)? {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            match &z {
                Some(z) => self.append_encoded(&mut w, Kind::Blob, ch, c.len() as u64, Codec::Zstd, z)?,
                None => self.append_encoded(&mut w, Kind::Blob, ch, c.len() as u64, Codec::Raw, c)?,
            }
            self.maybe_seal(&mut w)?;
        }
        levels[0].entries.push((ch, c.len() as u64));
        levels[0].len += c.len() as u64;
        // Carry full nodes upward.
        let mut i = 0;
        while levels[i].entries.len() == fanout {
            if i + 1 == levels.len() {
                // The new parent's first node so far is exactly this node's bytes.
                let h = levels[i].hasher.clone();
                levels.push(Level { hasher: h, entries: Vec::new(), len: 0 });
            }
            let lv = &mut levels[i];
            let node = self.emit_node(&mut lv.hasher, &mut lv.entries, &mut lv.len)?;
            levels[i + 1].entries.push(node);
            levels[i + 1].len += node.1;
            i += 1;
        }
        Ok(())
    }

    /// Writes one interior manifest node and resets the level.
    fn emit_node(
        &self,
        hasher: &mut blake3::Hasher,
        entries: &mut Vec<(Hash, u64)>,
        len: &mut u64,
    ) -> Result<(Hash, u64)> {
        let h = Hash(*hasher.finalize().as_bytes());
        let l = *len;
        if !self.dedup_hit(&h)? {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            self.append(&mut w, Kind::Manifest, h, l, &encode_manifest(entries))?;
            self.maybe_seal(&mut w)?;
        }
        *hasher = blake3::Hasher::new();
        entries.clear();
        *len = 0;
        Ok((h, l))
    }

    /// Counters (local view, without a refresh).
    pub fn stats(&self) -> Result<PackStats> {
        let idx = self.inner.index.read().map_err(poisoned)?;
        let w = self.inner.writer.lock().map_err(poisoned)?;
        let mut s = PackStats {
            sealed_packs: idx.known.len(),
            objects: idx.map.len(),
            open_bytes: w.as_ref().map(|o| o.len).unwrap_or(0),
            ..Default::default()
        };
        let mut count = |kind: Kind, raw: u64| match kind {
            Kind::Frame | Kind::Frame2 => {}
            Kind::Blob => {
                s.blobs += 1;
                s.logical_blob_bytes += raw;
            }
            Kind::Manifest => s.manifests += 1,
        };
        for l in idx.map.values() {
            count(l.kind, l.raw_len);
        }
        // Mapped packs: counted per entry (an object present in several packs counts more than once).
        let mut big_entries = 0;
        for b in &idx.big {
            for i in 0..b.view.len() {
                let e = b.view.entry(i)?;
                count(e.kind, e.raw_len);
                big_entries += 1;
            }
        }
        s.objects += big_entries;
        Ok(s)
    }

    // -- GC / repack support ------------------------------------------------

    /// Overrides [`DEDUP_REFRESH_INTERVAL`] for this handle. GC grace
    /// periods must stay much longer than this.
    pub fn set_dedup_refresh_interval(&self, d: std::time::Duration) {
        self.inner.dedup_refresh_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }

    /// The backend sealed packs live on.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.backend
    }

    /// Sealed packs that are not condemned, after a refresh.
    pub fn live_packs(&self) -> Result<Vec<String>> {
        self.refresh()?;
        let idx = self.inner.index.read().map_err(poisoned)?;
        let mut v: Vec<String> =
            idx.sealed.iter().filter(|n| idx.known.contains(*n) && !idx.condemned.contains(*n)).cloned().collect();
        v.sort();
        Ok(v)
    }

    /// Packs currently condemned by GC records (as of the last refresh).
    pub fn condemned_packs(&self) -> Result<HashSet<String>> {
        Ok(self.inner.index.read().map_err(poisoned)?.condemned.clone())
    }

    /// Index entries of sealed pack `name`, read from the backend.
    pub fn pack_entries(&self, name: &str) -> Result<Vec<IdxEntry>> {
        let mut buf = Vec::new();
        self.inner.backend.open(&format!("packs/{name}.idx"))?.read_to_end(&mut buf)?;
        decode_index(&buf)
    }

    /// Byte size of sealed pack `name` (pack plus index).
    pub fn pack_bytes(&self, name: &str) -> Result<u64> {
        let b = &self.inner.backend;
        let p = b.head(&format!("packs/{name}.pack"))?.map_or(0, |m| m.len);
        let i = b.head(&format!("packs/{name}.idx"))?.map_or(0, |m| m.len);
        Ok(p + i)
    }

    /// For a chunked object, the hashes of its chunks. `None` for a plain blob.
    pub fn chunks(&self, hash: &Hash) -> Result<Option<Vec<Hash>>> {
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        match loc.kind {
            Kind::Blob => Ok(None),
            Kind::Manifest => Ok(Some(self.read_manifest(hash, loc)?.into_iter().map(|c| c.0).collect())),
            Kind::Frame | Kind::Frame2 => Err(Error::Corrupt("frame record indexed as an object".into())),
        }
    }

    /// Copies `hash`'s record verbatim (no recompression) into this
    /// handle's open pack. The copy becomes durable at the next flush. Chunks
    /// of a manifest aren't copied, so callers copy every live hash themselves.
    pub fn copy_record(&self, hash: &Hash) -> Result<()> {
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        if loc.frame.is_some() {
            // A framed member is copied out as a plain blob record.
            let data = self.read_blob(hash, loc)?;
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            self.inner.faults.hit("copy.write")?;
            self.append(&mut w, Kind::Blob, *hash, data.len() as u64, &data)?;
            return self.maybe_seal(&mut w);
        }
        let (h, raw, _) = self.read_raw(hash, loc)?;
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        if w.is_none() {
            *w = Some(self.new_open_pack()?);
        }
        let op = w.as_mut().ok_or_else(|| Error::Corrupt("no open pack".into()))?;
        let offset = op.len;
        self.inner.faults.hit("copy.write")?;
        op.file.write_all_at(&raw, offset)?;
        op.len += raw.len() as u64;
        op.entries.push(IdxEntry {
            hash: h.hash,
            offset,
            stored_len: h.stored_len,
            raw_len: h.raw_len,
            kind: h.kind,
            codec: h.codec,
            frame: None,
        });
        let l = Loc {
            pack: PackRef::Open(op.generation),
            offset,
            stored_len: h.stored_len,
            raw_len: h.raw_len,
            kind: h.kind,
            frame: None,
        };
        insert_loc(&mut *self.inner.index.write().map_err(poisoned)?, h.hash, l);
        self.maybe_seal(&mut w)
    }

    /// Writes `members` as one frame record compressed together at `level`.
    /// This is the similarity-grouped layout of Experiment 1 (V2): callers pass
    /// related objects in a meaningful order. Members are always written, even
    /// if a copy already exists elsewhere (repack relies on this). Durable on flush.
    pub fn put_frame(&self, members: &[&[u8]], level: i32) -> Result<Vec<Hash>> {
        if members.iter().any(|m| m.len() >= self.inner.cfg.chunk_threshold) {
            return Err(Error::Parse("frame members must be smaller than the chunk threshold".into()));
        }
        let hashes: Vec<Hash> = members.iter().map(|m| Hash::of(m)).collect();
        let (codec, payload, fh, raw_len) = encode_frame(members, level)?;
        let rec = encode_record(Kind::Frame2, codec, &fh, raw_len, &payload);
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        if w.is_none() {
            *w = Some(self.new_open_pack()?);
        }
        let op = w.as_mut().ok_or_else(|| Error::Corrupt("no open pack".into()))?;
        let offset = op.len;
        if let Err(e) = self.inner.faults.hit("frame.write") {
            let _ = op.file.write_all_at(&rec[..rec.len() / 2], offset);
            return Err(e.into());
        }
        op.file.write_all_at(&rec, offset)?;
        op.len += rec.len() as u64;
        let stored_len = payload.len() as u32;
        let mut dir = Vec::with_capacity(members.len());
        let mut off = 0u32;
        for (h, m) in hashes.iter().zip(members) {
            dir.push((*h, off, m.len() as u32));
            off += m.len() as u32;
        }
        let mut idx = self.inner.index.write().map_err(poisoned)?;
        for (mh, inner, mlen) in dir {
            let frame = Some((inner, raw_len as u32));
            op.entries.push(IdxEntry {
                hash: mh,
                offset,
                stored_len,
                raw_len: mlen as u64,
                kind: Kind::Blob,
                codec,
                frame,
            });
            let l = Loc {
                pack: PackRef::Open(op.generation),
                offset,
                stored_len,
                raw_len: mlen as u64,
                kind: Kind::Blob,
                frame,
            };
            insert_loc(&mut idx, mh, l);
        }
        drop(idx);
        self.maybe_seal(&mut w)?;
        Ok(hashes)
    }

    /// See [`ObjectStore::ensure_live`]. Walks manifest trees, so cost is
    /// proportional to the object's manifest size, not its data.
    pub fn ensure_live_impl(&self, hash: &Hash) -> Result<bool> {
        // Refresh first if our view of condemnations may be stale.
        let _ = self.dedup_hit(hash)?;
        let Some(loc) = self.lookup(hash)? else { return Ok(false) };
        if loc.kind == Kind::Manifest {
            for (child, _) in self.read_manifest(hash, loc)? {
                if !self.ensure_live_impl(&child)? {
                    return Err(Error::Corrupt(format!("{hash} references missing chunk {child}")));
                }
            }
        }
        let condemned_only = {
            let idx = self.inner.index.read().map_err(poisoned)?;
            idx.get(hash).is_some_and(|l| rank(&idx, &l) == 2)
        };
        if condemned_only {
            self.copy_record(hash)?;
        }
        Ok(true)
    }

    /// Name of the sealed pack `hash` is currently served from (`None` if it
    /// comes from this handle's open pack, or isn't present).
    pub fn serving_pack(&self, hash: &Hash) -> Result<Option<String>> {
        let idx = self.inner.index.read().map_err(poisoned)?;
        Ok(idx.get(hash).and_then(|l| match l.pack {
            PackRef::Sealed(pid) => idx.sealed.get(pid as usize).cloned(),
            PackRef::Open(_) => None,
        }))
    }

    /// Every hash addressable through `get` (objects and chunks), sorted.
    pub fn hashes(&self) -> Result<Vec<Hash>> {
        let idx = self.inner.index.read().map_err(poisoned)?;
        let mut v: Vec<Hash> = idx.map.keys().copied().collect();
        for b in &idx.big {
            for i in 0..b.view.len() {
                v.push(b.view.entry(i)?.hash);
            }
        }
        v.sort();
        v.dedup();
        Ok(v)
    }

    /// Path of this handle's open staging file, if any (test hook).
    pub fn staging_file(&self) -> Option<PathBuf> {
        self.inner.writer.lock().ok().and_then(|w| w.as_ref().map(|o| o.path.clone()))
    }
}

impl ObjectStore for PackStore {
    fn put(&self, data: &[u8]) -> Result<Hash> {
        if data.len() >= self.inner.cfg.chunk_threshold {
            let cfg = &self.inner.cfg;
            let cdc = fastcdc::v2020::FastCDC::new(data, cfg.cdc_min, cfg.cdc_avg, cfg.cdc_max);
            return self.put_chunks(cdc.map(|c| Ok(data[c.offset..c.offset + c.length].to_vec())));
        }
        let hash = Hash::of(data);
        if self.dedup_hit(&hash)? {
            return Ok(hash);
        }
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        self.append(&mut w, Kind::Blob, hash, data.len() as u64, data)?;
        self.maybe_seal(&mut w)?;
        Ok(hash)
    }

    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash> {
        let thr = self.inner.cfg.chunk_threshold;
        let mut head = Vec::with_capacity(thr.min(1 << 16));
        src.take(thr as u64).read_to_end(&mut head)?;
        if head.len() < thr {
            return self.put(&head);
        }
        let cfg = &self.inner.cfg;
        let reader = std::io::Cursor::new(head).chain(src);
        let cdc = fastcdc::v2020::StreamCDC::new(reader, cfg.cdc_min, cfg.cdc_avg, cfg.cdc_max);
        self.put_chunks(cdc.map(|r| {
            r.map(|c| c.data).map_err(|e| match e {
                fastcdc::v2020::Error::IoError(io) => Error::Io(io),
                other => Error::Corrupt(format!("chunker: {other:?}")),
            })
        }))
    }

    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.open(hash, range)?.read_to_end(&mut out).map_err(unwrap_io)?;
        Ok(out)
    }

    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>> {
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        let whole = range.is_none();
        let r = check_range(range, loc.raw_len)?;
        match loc.kind {
            Kind::Frame | Kind::Frame2 => Err(Error::Corrupt("frame record indexed as an object".into())),
            Kind::Blob => {
                let d = self.read_blob(hash, loc)?;
                Ok(Box::new(std::io::Cursor::new(d[r.start as usize..r.end as usize].to_vec())))
            }
            Kind::Manifest => {
                let pieces = overlapping(self.read_manifest(hash, loc)?, r.start, r.end);
                Ok(Box::new(ChunkReader {
                    store: self.clone(),
                    stack: vec![pieces.into_iter()],
                    cur: std::io::Cursor::new(Vec::new()),
                    verify: whole.then(|| (blake3::Hasher::new(), *hash)),
                    depth_limit: 16,
                }))
            }
        }
    }

    fn size(&self, hash: &Hash) -> Result<u64> {
        self.lookup(hash)?.map(|l| l.raw_len).ok_or(Error::NotFound(*hash))
    }

    fn contains(&self, hash: &Hash) -> Result<bool> {
        Ok(self.lookup(hash)?.is_some())
    }

    fn ensure_live(&self, hash: &Hash) -> Result<bool> {
        self.ensure_live_impl(hash)
    }

    fn flush(&self) -> Result<()> {
        self.seal()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Best effort. If this fails, the next open adopts the staging file.
        if self.faults.crashed() {
            return; // simulate a dead process: leave the staging file behind
        }
        if let Ok(mut w) = self.writer.lock()
            && let Some(op) = w.take()
        {
            let _ = self.seal_file(&op.path, &op.file, op.len, op.entries);
        }
    }
}

/// Reads a newline-separated list of names (a GC record).
pub fn read_name_list(b: &dyn Backend, key: &str) -> Result<Vec<String>> {
    let mut buf = String::new();
    b.open(key)?.read_to_string(&mut buf)?;
    let v: Vec<String> = buf.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect();
    if v.iter().any(|n| n.len() != 64 || !n.bytes().all(|c| c.is_ascii_hexdigit())) {
        return Err(Error::Corrupt(format!("bad GC record {key}")));
    }
    Ok(v)
}

/// One level of a manifest tree under construction.
struct Level {
    hasher: blake3::Hasher,
    entries: Vec<(Hash, u64)>,
    len: u64,
}

/// zstd-compresses `raw` at `level`, or returns `None` when that doesn't help (or level 0).
fn compress(level: i32, raw: &[u8]) -> Option<Vec<u8>> {
    if level != 0 && raw.len() >= 64 {
        zstd::bulk::compress(raw, level).ok().filter(|c| c.len() < raw.len())
    } else {
        None
    }
}

/// Chunks hashed and compressed in parallel per batch in `put_chunks`.
const CHUNK_BATCH: usize = 64;

/// Converts the `io::Error` wrapping of a core error back into it.
fn unwrap_io(e: std::io::Error) -> Error {
    match e.downcast::<Error>() {
        Ok(inner) => inner,
        Err(e) => Error::Io(e),
    }
}

/// Entries of `m` that overlap `[start, end)`, with their local sub-range.
fn overlapping(m: Vec<(Hash, u64)>, start: u64, end: u64) -> Vec<(Hash, u64, u64)> {
    let mut out = Vec::new();
    let mut pos = 0u64;
    for (h, len) in m {
        let (cs, ce) = (pos, pos + len);
        pos = ce;
        if ce > start && cs < end {
            out.push((h, start.max(cs) - cs, end.min(ce) - cs));
        }
    }
    out
}

/// Streams a chunked object, expanding the manifest tree lazily, so memory
/// is bounded by tree depth times fanout, whatever the object's size.
struct ChunkReader {
    store: PackStore,
    /// One iterator per tree level: `(hash, start, end)` local to that entry.
    stack: Vec<std::vec::IntoIter<(Hash, u64, u64)>>,
    cur: std::io::Cursor<Vec<u8>>,
    /// Whole-object reads also verify the reassembled hash at EOF.
    verify: Option<(blake3::Hasher, Hash)>,
    depth_limit: usize,
}

fn io_err(e: Error) -> std::io::Error {
    std::io::Error::other(e)
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.cur.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let next = loop {
                match self.stack.last_mut() {
                    None => break None,
                    Some(it) => match it.next() {
                        Some(p) => break Some(p),
                        None => {
                            self.stack.pop();
                        }
                    },
                }
            };
            let Some((h, a, b)) = next else {
                if let Some((hs, want)) = self.verify.take()
                    && Hash(*hs.finalize().as_bytes()) != want
                {
                    return Err(io_err(Error::Corrupt(format!("reassembled hash mismatch for {want}"))));
                }
                return Ok(0);
            };
            let loc = self
                .store
                .lookup(&h)
                .map_err(io_err)?
                .ok_or_else(|| io_err(Error::Corrupt(format!("missing chunk {h}"))))?;
            match loc.kind {
                Kind::Frame | Kind::Frame2 => {
                    return Err(io_err(Error::Corrupt("frame record indexed as an object".into())));
                }
                Kind::Manifest => {
                    if self.stack.len() >= self.depth_limit {
                        return Err(io_err(Error::Corrupt("manifest tree too deep".into())));
                    }
                    let m = self.store.read_manifest(&h, loc).map_err(io_err)?;
                    self.stack.push(overlapping(m, a, b).into_iter());
                }
                Kind::Blob => {
                    let d = self.store.read_blob(&h, loc).map_err(io_err)?;
                    if let Some((hs, _)) = self.verify.as_mut() {
                        hs.update(&d);
                    }
                    let slice = d
                        .get(a as usize..b as usize)
                        .ok_or_else(|| io_err(Error::Corrupt("chunk shorter than manifest".into())))?;
                    self.cur = std::io::Cursor::new(slice.to_vec());
                }
            }
        }
    }
}
