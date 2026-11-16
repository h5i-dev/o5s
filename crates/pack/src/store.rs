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
use pico2peta_core::{Error, Hash, ObjectStore, ReadVerify, Result, check_range};
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

/// Content-defined chunker for large objects (see [`PackStore::put_stream_with`]).
///
/// The chunker decides chunk boundaries, i.e. dedup granularity and which
/// other content shares chunks. It never affects object identity (the BLAKE3
/// of the whole content) or reads: manifests just list chunks, so objects
/// written with any chunker read back the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChunkerKind {
    /// FastCDC v2020 with the store's `cdc_min/avg/max` (default 16/64/256 KiB).
    #[default]
    FastCdc,
    /// Byte-exact Hugging Face Xet chunking (Gear hash, 8/64/128 KiB), so
    /// chunks line up with Xet-hosted copies of the same content.
    Xet,
}

impl ChunkerKind {
    /// Parses a chunker id (`"fastcdc"`, `"xet"`); `None` = the default.
    pub fn from_id(id: Option<&str>) -> Result<ChunkerKind> {
        match id {
            None | Some("fastcdc") => Ok(ChunkerKind::FastCdc),
            Some("xet") => Ok(ChunkerKind::Xet),
            Some(other) => Err(Error::Parse(format!("unknown chunker {other:?} (fastcdc or xet)"))),
        }
    }

    /// The chunker's id (inverse of [`ChunkerKind::from_id`]).
    pub fn id(self) -> &'static str {
        match self {
            ChunkerKind::FastCdc => "fastcdc",
            ChunkerKind::Xet => "xet",
        }
    }
}

/// Per-put options for [`PackStore::put_stream_opts`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PutOpts {
    /// Content-defined chunker for large objects.
    pub chunker: ChunkerKind,
    /// Byte-group chunks with this element width (2 = bf16/fp16, 4 = fp32)
    /// before zstd (record codec `GroupedZstd`). A hint for fixed-width
    /// numeric data such as model weights; `None`, 0 or 1 = off. Changes only
    /// the stored encoding, never identity.
    pub group_width: Option<u8>,
}

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
    /// Before compressing a record of at least 8 KiB, try zstd-1 on its first
    /// 4 KiB. If that saves less than 3%, store the record raw and skip the full
    /// compression. Deterministic; affects only the codec, never identity.
    pub compress_probe: bool,
    /// zstd level for byte-grouped records (codec 2; model weights and their
    /// deltas). Grouped data keeps shrinking at higher levels where plain zstd
    /// doesn't (measured on bf16: L3 −1.6%, L6 −4.5%, L15 −9.4% vs plain L3;
    /// one core does ~290 / 90 / 12 MB/s). 0 = store grouped chunks raw.
    pub grouped_zstd_level: i32,
}

/// Default [`PackConfig::grouped_zstd_level`]: as fast as the disk on a
/// 4-core host, with about three times level 3's gain.
pub const DEFAULT_GROUPED_ZSTD_LEVEL: i32 = 6;

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
            compress_probe: true,
            grouped_zstd_level: DEFAULT_GROUPED_ZSTD_LEVEL,
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
            compress_probe: true,
            grouped_zstd_level: 1,
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
            && (-7..=22).contains(&self.grouped_zstd_level)
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
    /// Of which XOR-delta objects.
    pub deltas: usize,
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
    /// BLAKE3 of bytes `[0, len)`, updated on every append (appends are serialized
    /// and contiguous), so sealing needn't read the pack back.
    hasher: blake3::Hasher,
}

struct Inner {
    cfg: PackConfig,
    backend: Arc<dyn Backend>,
    staging_dir: PathBuf,
    faults: FaultHandle,
    index: RwLock<Index>,
    writer: Mutex<Option<OpenPack>>,
    /// Open packs whose seal failed after publication: frozen (never appended
    /// to), still readable, and retried on the next flush.
    pending: Mutex<Vec<OpenPack>>,
    /// Serializes refreshes, so an older listing can't overwrite a newer one.
    refresh_lock: Mutex<()>,
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
/// Smallest frame prefix decoded at once (see `frame_bodies`).
const FRAME_PREFIX_MIN: usize = 256 << 10;

impl Inner {
    fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.faults.hit(point)?)
    }

    /// Seals a fully written, locked staging file: fsync, upload pack, upload
    /// index, unlink. Idempotent and crash-safe at every step.
    fn seal_file(&self, path: &Path, f: &File, len: u64, entries: Vec<IdxEntry>) -> Result<String> {
        self.seal_file_named(path, f, len, entries, None)
    }

    /// Like `seal_file`. `name` is the pack's BLAKE3 if the caller already
    /// computed it incrementally while appending; otherwise the file is read back.
    fn seal_file_named(
        &self,
        path: &Path,
        f: &File,
        len: u64,
        mut entries: Vec<IdxEntry>,
        name: Option<String>,
    ) -> Result<String> {
        // Drop anything past the last complete record (e.g. a write that failed
        // halfway), so the published file is exactly the hashed bytes.
        if f.metadata()?.len() > len {
            f.set_len(len)?;
        }
        self.fault("seal.fsync")?;
        f.sync_all()?;
        let name = match name {
            Some(n) => n,
            None => {
                let mut hasher = blake3::Hasher::new();
                let mut buf = vec![0u8; 1 << 20];
                let mut off = 0u64;
                while off < len {
                    let n = buf.len().min((len - off) as usize);
                    f.read_exact_at(&mut buf[..n], off)?;
                    hasher.update(&buf[..n]);
                    off += n as u64;
                }
                hasher.finalize().to_hex().to_string()
            }
        };
        // Each step is skipped if an earlier attempt already did it, so a seal
        // that failed halfway can be retried (even after the staging file is gone).
        let pack_key = format!("packs/{name}.pack");
        if self.backend.head(&pack_key)?.is_none() {
            self.fault("seal.put-pack")?;
            self.backend.put_if_absent(&pack_key, PutSource::File(path))?;
        }
        let idx_key = format!("packs/{name}.idx");
        if self.backend.head(&idx_key)?.is_none() {
            let idx = encode_index(&mut entries);
            self.fault("seal.put-idx")?;
            let _: PutOutcome = self.backend.put_if_absent(&idx_key, PutSource::Bytes(&idx))?;
        }
        self.fault("seal.unlink")?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
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
                pending: Mutex::new(Vec::new()),
                refresh_lock: Mutex::new(()),
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
        let _serial = self.inner.refresh_lock.lock().map_err(poisoned)?;
        let listed = self.inner.backend.list("packs/")?;
        let keys: HashSet<&str> = listed.iter().map(|m| m.key.as_str()).collect();
        let live_idx: Vec<String> = listed
            .iter()
            .filter_map(|m| m.key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx")))
            .map(|s| s.to_string())
            .collect();
        // An index without its pack is a pack being deleted by GC (it deletes the
        // index first, but a non-atomic listing can still show it): skip it.
        // A permanently missing pack is reported by fsck.
        let live_idx: Vec<String> =
            live_idx.into_iter().filter(|name| keys.contains(format!("packs/{name}.pack").as_str())).collect();
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
                    // Another process may have a newer listing: delete only caches whose
                    // index is confirmed gone from the backend.
                    if let Some(name) = fname.strip_suffix(".idx3")
                        && !live.contains(&fname)
                        && matches!(self.inner.backend.head(&format!("packs/{name}.idx")), Ok(None))
                    {
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
        let f = match File::open(&path) {
            Ok(f) => f,
            // Removed by another process meanwhile: skip this round; a later refresh rebuilds it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
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
                // Other processes may use a different pack set's MIDX: only remove ones
                // unused for an hour (mapped files stay valid after unlink anyway).
                let old = ent
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(3600));
                if n.starts_with("midx-") && n.ends_with(".midx") && n != format!("midx-{tag}.midx") && old {
                    let _ = fs::remove_file(ent.path());
                }
            }
        }
        Ok(())
    }

    /// Fetches a (large) pack index into the local index cache, verifying it
    /// on the way, and memory-maps it. Non-v3 indexes are converted to v3.
    fn map_index(&self, name: &str, key: &str) -> Result<IdxV3View<memmap2::Mmap>> {
        for _ in 0..3 {
            match self.map_index_once(name, key) {
                // Another process removed the cache file between our check and open: refetch.
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                other => return other,
            }
        }
        self.map_index_once(name, key)
    }

    fn map_index_once(&self, name: &str, key: &str) -> Result<IdxV3View<memmap2::Mmap>> {
        let dir = self.inner.staging_dir.with_file_name("idx-cache");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.idx3"));
        // On a local-file backend a v3 index is hard-linked, not copied: pack
        // indexes are content-named and never modified, and the mapped view is
        // checksum-verified either way (saves the index's size on disk, ~66 B per object).
        if !path.exists()
            && let Some(src) = self.inner.backend.local_path(key)
            && File::open(&src)
                .and_then(|mut f| {
                    let mut head = [0u8; 8];
                    f.read_exact(&mut head).map(|_| head)
                })
                .is_ok_and(|h| &h == IDX_MAGIC_V3)
            && fs::hard_link(&src, &path).is_ok()
        {
            fsync_dir(&dir)?;
        }
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

    /// [`ObjectStore::put_many`]: small items are hashed, dedup-checked and
    /// compressed on up to 4 threads (off the writer lock), then appended in
    /// order in one writer-lock pass. Items at or above the chunking threshold
    /// go through [`ObjectStore::put`] (its own parallel pipeline). Duplicates
    /// within the batch are stored once.
    pub fn put_many_impl(&self, items: &[&[u8]]) -> Result<Vec<Hash>> {
        let cfg = &self.inner.cfg;
        let small: Vec<usize> = (0..items.len()).filter(|&i| items[i].len() < cfg.chunk_threshold).collect();
        let total: usize = small.iter().map(|&i| items[i].len()).sum();
        // One encoded record (codec, bytes) per small item that must be written.
        let encode = |i: usize, hash: Hash| -> (Codec, Vec<u8>) {
            let data = items[i];
            let compressed = compress_with_probe(cfg.zstd_level, cfg.compress_probe, data);
            let (codec, payload): (Codec, &[u8]) = match &compressed {
                Some(c) => (Codec::Zstd, c),
                None => (Codec::Raw, data),
            };
            (codec, encode_record(Kind::Blob, codec, &hash, data.len() as u64, payload))
        };
        // Dedup first (one staleness check, one read lock), so known objects
        // are neither compressed nor appended.
        let hashes_known: Vec<(Hash, bool)> = {
            let hs: Vec<Hash> = if total >= PUT_MANY_PARALLEL_BYTES {
                par_map(&small, |&i| Hash::of(items[i]))?
            } else {
                small.iter().map(|&i| Hash::of(items[i])).collect()
            };
            if !hs.is_empty() {
                let _ = self.dedup_hit(&hs[0])?; // refreshes the index if stale
            }
            let idx = self.inner.index.read().map_err(poisoned)?;
            hs.into_iter().map(|h| (h, idx.get(&h).is_some_and(|l| rank(&idx, &l) < 2))).collect()
        };
        let mut seen: HashSet<Hash> = HashSet::with_capacity(small.len());
        let todo: Vec<(usize, usize)> = small
            .iter()
            .enumerate()
            .filter(|(k, _)| !hashes_known[*k].1 && seen.insert(hashes_known[*k].0))
            .map(|(k, &i)| (k, i))
            .collect();
        let todo_bytes: usize = todo.iter().map(|&(_, i)| items[i].len()).sum();
        let encoded: Vec<(Codec, Vec<u8>)> = if todo_bytes >= PUT_MANY_PARALLEL_BYTES {
            par_map(&todo, |&(k, i)| encode(i, hashes_known[k].0))?
        } else {
            todo.iter().map(|&(k, i)| encode(i, hashes_known[k].0)).collect()
        };
        {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            for ((k, i), (codec, rec)) in todo.iter().zip(encoded) {
                self.append_record(&mut w, Kind::Blob, hashes_known[*k].0, items[*i].len() as u64, codec, &rec)?;
                self.maybe_seal(&mut w)?;
            }
        }
        let mut out = vec![Hash([0; 32]); items.len()];
        for (k, &i) in small.iter().enumerate() {
            out[i] = hashes_known[k].0;
        }
        for i in 0..items.len() {
            if items[i].len() >= cfg.chunk_threshold {
                out[i] = self.put(items[i])?;
            }
        }
        Ok(out)
    }

    /// [`ObjectStore::put_concat`]: the result is built from pieces in order.
    /// A range of a chunked (manifest) source contributes that source's leaf
    /// chunks, trimmed at the range ends; any other source contributes pieces
    /// of at most `cdc_max` bytes. Pieces are read (verified), shown to
    /// `observe`, and fed to the manifest-tree pipeline as chunks, so chunks
    /// already stored are dedup hits (no chunker, no recompression). Results
    /// under the chunking threshold are stored as one blob, like `put`.
    pub fn put_concat_impl(&self, parts: &[(Hash, Range<u64>)], observe: &mut dyn FnMut(&[u8])) -> Result<Hash> {
        let cfg = &self.inner.cfg;
        let piece_max = (cfg.cdc_max as u64).clamp(1, cfg.chunk_threshold.max(2) as u64 - 1);
        let mut plan: Vec<(Hash, Range<u64>)> = Vec::new();
        let mut total = 0u64;
        for (h, r) in parts {
            let loc = self.lookup(h)?.ok_or(Error::NotFound(*h))?;
            let r = check_range(Some(r.clone()), loc.raw_len)?;
            total = total.checked_add(r.end - r.start).ok_or_else(|| Error::Parse("put_concat: too long".into()))?;
            if r.start == r.end {
                continue;
            }
            if loc.kind == Kind::Manifest {
                for (c, off, len) in self.leaf_chunks(h)? {
                    let (a, b) = (off.max(r.start), (off + len).min(r.end));
                    if a < b {
                        plan.push((c, a - off..b - off));
                    }
                }
            } else {
                let mut a = r.start;
                while a < r.end {
                    let b = (a + piece_max).min(r.end);
                    plan.push((*h, a..b));
                    a = b;
                }
            }
        }
        if total > MAX_BLOB * 64 {
            return Err(Error::Parse("put_concat: result too large".into()));
        }
        let read = |(h, r): &(Hash, Range<u64>)| -> Result<Vec<u8>> {
            let d = self.get(h, Some(r.clone()))?;
            if d.len() as u64 != r.end - r.start {
                return Err(Error::Corrupt(format!("short read of {h}")));
            }
            Ok(d)
        };
        if total < cfg.chunk_threshold as u64 {
            let mut all = Vec::with_capacity(total as usize);
            for p in &plan {
                let d = read(p)?;
                observe(&d);
                all.extend_from_slice(&d);
            }
            return self.put(&all);
        }
        let chunks = plan.iter().map(|p| {
            let d = read(p)?;
            observe(&d);
            Ok(d)
        });
        self.put_chunks(chunks, None)
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
            let entries = match self.truncate_torn_tail(&f) {
                Ok(e) => e,
                Err(Error::Corrupt(why)) => {
                    // A CRC-valid but undecodable record: not a torn tail, and not
                    // something to guess about. Quarantine the file (it's never
                    // adopted again; `fsck` reports it) so the repository stays usable.
                    let q = p.with_extension(format!("corrupt-{}", unique_id()));
                    fs::rename(&p, &q)?;
                    fsync_dir(&self.inner.staging_dir)?;
                    let _ = why;
                    continue;
                }
                Err(e) => return Err(e),
            };
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

    /// Reads object `hash` during recovery: from an entry already recovered
    /// from `f` (the staging file being scanned), else from the store.
    fn read_from_file(&self, f: &File, entries: &[IdxEntry], hash: &Hash, depth: u32) -> Result<Vec<u8>> {
        if depth >= MAX_REF_DEPTH {
            return Err(Error::Corrupt(format!("frame reference chain too deep at {hash}")));
        }
        let Some(e) = entries.iter().rev().find(|e| e.hash == *hash) else {
            return self.get(hash, None);
        };
        let mut hdr = vec![0u8; REC_HEADER];
        f.read_exact_at(&mut hdr, e.offset)?;
        let h = parse_header(&hdr)?;
        let mut rec = vec![0u8; h.total_len() as usize];
        f.read_exact_at(&mut rec, e.offset)?;
        let (h, payload) = decode_record(&rec)?;
        let data = match e.frame {
            Some((inner, _)) => {
                let reference = match frame_ref(&h, payload)? {
                    Some(r) => Some(self.read_from_file(f, entries, &r, depth + 1)?),
                    None => None,
                };
                let bodies = decode_frame_bodies(&h, payload, reference.as_deref())?;
                bodies
                    .get(inner as usize..inner as usize + e.raw_len as usize)
                    .ok_or_else(|| Error::Corrupt("frame member out of range".into()))?
                    .to_vec()
            }
            None => unpack_payload(&h, payload)?,
        };
        if Hash::of(&data) != *hash {
            return Err(Error::Corrupt(format!("content hash mismatch for {hash} during recovery")));
        }
        Ok(data)
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
                    // A Frame3 reference may be an earlier record of this same file.
                    let resolve = |r: &Hash| self.read_from_file(f, &entries, r, 0);
                    let members = frame_members(&h, payload, &resolve)?;
                    let member_kind = if h.kind == Kind::Frame3 { Kind::Frame3 } else { Kind::Blob };
                    for (mh, inner, mlen) in members {
                        entries.push(IdxEntry {
                            hash: mh,
                            offset: good,
                            stored_len: h.stored_len,
                            raw_len: mlen as u64,
                            kind: member_kind,
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
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header);
        // Rename while holding the lock. Recovery only considers `*.pack`,
        // so a staging file is never adopted before its lock is held.
        fs::rename(&tmp, &path)?;
        fsync_dir(&self.inner.staging_dir)?;
        Ok(OpenPack {
            hasher,
            file: Arc::new(f),
            path,
            generation: self.inner.generation.fetch_add(1, Ordering::Relaxed),
            len: PACK_HEADER_V2 as u64,
            entries: Vec::new(),
        })
    }

    /// Compresses (if useful) and appends one record to the open pack.
    fn append(&self, w: &mut Option<OpenPack>, kind: Kind, hash: Hash, raw_len: u64, raw: &[u8]) -> Result<()> {
        let compressed = compress_with_probe(self.inner.cfg.zstd_level, self.inner.cfg.compress_probe, raw);
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
        self.append_record(w, kind, hash, raw_len, codec, &rec)
    }

    /// Appends a record already built by [`encode_record`] (so its encoding and
    /// CRC can be computed off the writer lock).
    fn append_record(
        &self,
        w: &mut Option<OpenPack>,
        kind: Kind,
        hash: Hash,
        raw_len: u64,
        codec: Codec,
        rec: &[u8],
    ) -> Result<()> {
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
        op.file.write_all_at(rec, offset)?;
        op.hasher.update(rec);
        op.len += rec.len() as u64;
        let stored_len = (rec.len() - REC_HEADER - REC_TRAILER) as u32;
        op.entries.push(IdxEntry { hash, offset, stored_len, raw_len, kind, codec, frame: None });
        let loc = Loc { pack: PackRef::Open(op.generation), offset, stored_len, raw_len, kind, frame: None };
        insert_loc(&mut *self.inner.index.write().map_err(poisoned)?, hash, loc);
        Ok(())
    }

    /// Seals the open pack if there is one.
    fn seal_locked(&self, w: &mut Option<OpenPack>) -> Result<()> {
        // First retry seals that failed after their pack was published.
        let pending: Vec<OpenPack> = std::mem::take(&mut *self.inner.pending.lock().map_err(poisoned)?);
        for op in pending {
            self.seal_one(op, w, true)?;
        }
        let Some(op) = w.take() else { return Ok(()) };
        self.seal_one(op, w, false)
    }

    fn seal_one(&self, op: OpenPack, w: &mut Option<OpenPack>, was_pending: bool) -> Result<()> {
        let OpenPack { file, path, generation, len, entries, hasher } = op;
        let known = hasher.finalize().to_hex().to_string();
        let name = match self.inner.seal_file_named(&path, &file, len, entries.clone(), Some(known)) {
            Ok(n) => n,
            Err(e) => {
                let op = OpenPack { file, path, generation, len, entries, hasher };
                // If the pack is already published (on local disk it's a hard link to this
                // very file), the file must never be appended to again: freeze it in the
                // pending list (still readable, retried on the next flush) and let new
                // writes start a fresh staging file. Otherwise it may stay open.
                let published = was_pending
                    || self
                        .published_name(&op)
                        .is_some_and(|n| self.inner.backend.head(&format!("packs/{n}.pack")).ok().flatten().is_some());
                if published {
                    self.inner.pending.lock().map_err(poisoned)?.push(op);
                } else {
                    *w = Some(op);
                }
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

    /// The content-derived name the open pack would be sealed under.
    fn published_name(&self, op: &OpenPack) -> Option<String> {
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut off = 0u64;
        while off < op.len {
            let n = buf.len().min((op.len - off) as usize);
            op.file.read_exact_at(&mut buf[..n], off).ok()?;
            hasher.update(&buf[..n]);
            off += n as u64;
        }
        Some(hasher.finalize().to_hex().to_string())
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
    /// `crc = false` skips the record CRC: only for reads whose content is then
    /// verified by its BLAKE3 hash (the CRC would check the same bytes again).
    fn read_raw(&self, hash: &Hash, mut loc: Loc, crc: bool) -> Result<(RecHeader, Vec<u8>, Loc)> {
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
                        match w.as_ref().filter(|op| op.generation == generation) {
                            Some(op) => Some(op.file.clone()),
                            None => self
                                .inner
                                .pending
                                .lock()
                                .map_err(poisoned)?
                                .iter()
                                .find(|op| op.generation == generation)
                                .map(|op| op.file.clone()),
                        }
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
            let (h, _) = if crc { decode_record(&raw)? } else { decode_record_unchecked(&raw)? };
            let framed = loc.frame.is_some();
            if framed != h.kind.is_frame() || (!framed && h.hash != *hash) {
                return Err(Error::Corrupt(format!("record at index location holds {} not {hash}", h.hash)));
            }
            return Ok((h, raw, loc));
        }
        Err(Error::Corrupt(format!("could not resolve location of {hash}")))
    }

    fn read_record(&self, hash: &Hash, loc: Loc, crc: bool) -> Result<(RecHeader, Vec<u8>)> {
        let (h, raw, loc) = self.read_raw(hash, loc, crc)?;
        if loc.frame.is_none() && h.raw_len != loc.raw_len {
            return Err(Error::Corrupt(format!("index length disagrees with record for {hash}")));
        }
        let (_, payload) = decode_record_unchecked(&raw)?; // checked (or skipped) by read_raw
        if let Some((inner, _)) = loc.frame {
            let (a, b) = (inner as usize, inner as usize + loc.raw_len as usize);
            let bodies = self.frame_bodies(&h, payload, &loc, b)?;
            let d = bodies.get(a..b).ok_or_else(|| Error::Corrupt(format!("frame member {hash} out of range")))?;
            return Ok((h, d.to_vec()));
        }
        if h.codec == Codec::Raw {
            // Reuse the record buffer: drop the header and trailer in place
            // (one memmove, no second allocation of the object's size).
            let end = REC_HEADER + payload.len();
            let (kind, raw_len) = (h.kind, h.raw_len);
            let mut raw = raw;
            raw.truncate(end);
            raw.drain(..REC_HEADER);
            if kind == Kind::Blob && raw.len() as u64 != raw_len {
                return Err(Error::Corrupt(format!("length mismatch for {}", h.hash)));
            }
            return Ok((h, raw));
        }
        Ok((h, unpack_payload(&h, payload)?))
    }

    /// The bytes of a frame's reference object, with a recursion guard (a
    /// reference may itself live in a frame with a reference; writers bound
    /// that depth, and this catches corrupt cycles).
    fn get_reference(&self, r: &Hash) -> Result<Vec<u8>> {
        struct Depth;
        impl Drop for Depth {
            fn drop(&mut self) {
                REF_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            }
        }
        if REF_DEPTH.with(|d| d.get()) >= MAX_REF_DEPTH {
            return Err(Error::Corrupt(format!("frame reference chain too deep at {r}")));
        }
        REF_DEPTH.with(|d| d.set(d.get() + 1));
        let _g = Depth;
        if self.lookup(r)?.is_none() {
            return Err(Error::Corrupt(format!("missing frame reference {r}")));
        }
        let mut out = Vec::new();
        self.open_depth(r, None, 0)?.read_to_end(&mut out).map_err(unwrap_io)?;
        Ok(out)
    }

    /// Decompressed frame bodies, through a small cache (recently used
    /// frames). Sequential reads of grouped objects then decompress each frame once.
    fn frame_bodies(&self, h: &RecHeader, payload: &[u8], loc: &Loc, need: usize) -> Result<Arc<Vec<u8>>> {
        let key = (h.hash, loc.offset);
        let cached = {
            let c = self.inner.frame_cache.lock().map_err(poisoned)?;
            c.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        };
        if let Some(v) = &cached
            && v.len() >= need
        {
            return Ok(v.clone());
        }
        let reference = match frame_ref(h, payload)? {
            Some(r) => Some(self.get_reference(&r)?),
            None => None,
        };
        // Decode only as far as needed (random reads of large frames), but at
        // least double what is cached (sequential reads stay linear), and
        // decode (and verify) the whole frame once past half of it.
        let raw = h.raw_len as usize;
        let want = need.max(cached.as_ref().map_or(0, |v| v.len() * 2)).max(FRAME_PREFIX_MIN);
        let v = if want * 2 >= raw {
            Arc::new(decode_frame_bodies(h, payload, reference.as_deref())?)
        } else {
            Arc::new(decode_frame_prefix(h, payload, reference.as_deref(), want)?)
        };
        let mut c = self.inner.frame_cache.lock().map_err(poisoned)?;
        c.retain(|(k, _)| *k != key);
        if c.len() >= FRAME_CACHE {
            c.remove(0);
        }
        c.push((key, v.clone()));
        Ok(v)
    }

    fn read_blob(&self, hash: &Hash, loc: Loc) -> Result<Vec<u8>> {
        // A plain blob skips the CRC: the BLAKE3 check below verifies the same
        // bytes. Frames keep it (one CRC covers all members, and decompression
        // dominates their cost).
        let (_, data) = self.read_record(hash, loc, loc.frame.is_some())?;
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
        let (_, data) = self.read_record(hash, loc, true)?;
        let m = decode_manifest(&data)?;
        if m.iter().try_fold(0u64, |a, c| a.checked_add(c.1)) != Some(loc.raw_len) {
            return Err(Error::Corrupt(format!("manifest length mismatch for {hash}")));
        }
        Ok(m)
    }

    fn put_chunks<I>(&self, chunks: I, group: Option<u8>) -> Result<Hash>
    where
        I: Iterator<Item = Result<Vec<u8>>>,
    {
        // A tree of manifests. Every node is the manifest of a contiguous range
        // of the object and is keyed by the BLAKE3 of that range, so every
        // index key is still the content hash of what `get` returns for it.
        //
        // Streaming pipeline: this thread runs the chunker; workers hash each
        // chunk, check dedup and compress; one committer takes results in
        // chunk order, hashes the object and its tree nodes, and appends. A
        // credit channel bounds how far the chunker can run ahead of the
        // committer (memory: at most `credits` chunks in flight).
        use std::sync::mpsc;
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(1).clamp(1, 8);
        let credits = PIPELINE_CREDITS_PER_WORKER * workers;
        let level = self.inner.cfg.zstd_level;
        let glevel = self.inner.cfg.grouped_zstd_level;
        let probe = self.inner.cfg.compress_probe;
        let fanout = self.inner.cfg.manifest_fanout;
        type Chunk = Arc<Vec<u8>>;
        let (work_tx, work_rx) = mpsc::sync_channel::<(u64, Chunk)>(credits);
        let work_rx = Mutex::new(work_rx);
        let (done_tx, done_rx) = mpsc::channel::<Result<(u64, Encoded)>>();
        let (whole_tx, whole_rx) = mpsc::sync_channel::<Chunk>(credits);
        let (snap_tx, snap_rx) = mpsc::channel::<Hash>();
        let (credit_tx, credit_rx) = mpsc::sync_channel::<()>(credits);
        for _ in 0..credits {
            let _ = credit_tx.send(());
        }
        let (fed, whole, tree) = std::thread::scope(|sc| {
            for _ in 0..workers {
                let done_tx = done_tx.clone();
                let work_rx = &work_rx;
                sc.spawn(move || {
                    loop {
                        let next = match work_rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok((seq, c)) = next else { return };
                        let hash = Hash::of(&c);
                        // Skip encoding chunks we already have (dedup); the
                        // committer re-checks before appending.
                        let r = self.lookup_local(&hash).map(|known| {
                            let rec = known.is_none().then(|| match group {
                                Some(w) => encode_grouped_or_raw(glevel, w, &hash, &c),
                                None => match compress_with_probe(level, probe, &c) {
                                    Some(z) => {
                                        (Codec::Zstd, encode_record(Kind::Blob, Codec::Zstd, &hash, c.len() as u64, &z))
                                    }
                                    None => {
                                        (Codec::Raw, encode_record(Kind::Blob, Codec::Raw, &hash, c.len() as u64, &c))
                                    }
                                },
                            });
                            (seq, Encoded { data: c, hash, rec })
                        });
                        let failed = r.is_err();
                        if done_tx.send(r).is_err() || failed {
                            return;
                        }
                    }
                });
            }
            drop(done_tx);
            // The whole-object hash runs on its own thread, in chunk order. The
            // tree needs it mid-stream only when the top level fills, i.e. after
            // exactly fanout^k chunks; it publishes those snapshots.
            let whole = sc.spawn(move || {
                let mut h = blake3::Hasher::new();
                let (mut n, mut next_snap) = (0u64, fanout as u64);
                for c in whole_rx {
                    h.update(&c);
                    n += 1;
                    if n == next_snap {
                        let _ = snap_tx.send(Hash(*h.finalize().as_bytes()));
                        next_snap = next_snap.saturating_mul(fanout as u64);
                    }
                }
                h
            });
            let committer = sc.spawn(move || -> Result<Vec<Level>> {
                let mut levels: Vec<Level> = vec![Level { hasher: blake3::Hasher::new(), entries: Vec::new(), len: 0 }];
                let mut top_hash =
                    || snap_rx.recv().map_err(|_| Error::Corrupt("put pipeline: hasher ended early".into()));
                let mut pending = std::collections::BTreeMap::new();
                let mut next = 0u64;
                for r in done_rx {
                    let (seq, e) = r?;
                    pending.insert(seq, e);
                    while let Some(e) = pending.remove(&next) {
                        self.add_chunk(e, &mut levels, fanout, &mut top_hash)?;
                        next += 1;
                        let _ = credit_tx.send(());
                    }
                }
                if !pending.is_empty() {
                    return Err(Error::Corrupt("put pipeline: chunk results out of sequence".into()));
                }
                Ok(levels)
            });
            // Feed the chunker's output. Stops early if the committer failed
            // (its credit sender is gone).
            let mut fed: Result<()> = Ok(());
            for (seq, c) in (0u64..).zip(chunks) {
                match c {
                    Ok(c) => {
                        let c = Arc::new(c);
                        if credit_rx.recv().is_err()
                            || whole_tx.send(c.clone()).is_err()
                            || work_tx.send((seq, c)).is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        fed = Err(e);
                        break;
                    }
                }
            }
            drop(work_tx);
            drop(whole_tx);
            drop(credit_rx);
            let panicked = || Error::Corrupt("put pipeline: thread panicked".into());
            let tree = committer.join().unwrap_or_else(|_| Err(panicked()));
            let whole = whole.join().map_err(|_| panicked());
            (fed, whole, tree)
        });
        // A chunker error wins: the committer may have seen a clean (truncated) end.
        fed?;
        let mut levels = tree?;
        let hash = Hash(*whole?.finalize().as_bytes());
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

    /// Adds one chunk (hashed, and encoded unless already known) to the tree.
    ///
    /// The top level's open node always starts at offset 0, so its hash is the
    /// whole-object hash so far, which `top_hash` supplies when the top fills.
    /// Only the levels below the top keep their own hashers (for objects under
    /// `fanout` chunks, none at all).
    fn add_chunk(
        &self,
        e: Encoded,
        levels: &mut Vec<Level>,
        fanout: usize,
        top_hash: &mut dyn FnMut() -> Result<Hash>,
    ) -> Result<()> {
        let c = &e.data[..];
        let top = levels.len() - 1;
        for l in levels[..top].iter_mut() {
            l.hasher.update(c);
        }
        if !self.dedup_hit(&e.hash)? {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            match &e.rec {
                Some((codec, rec)) => self.append_record(&mut w, Kind::Blob, e.hash, c.len() as u64, *codec, rec)?,
                None => self.append(&mut w, Kind::Blob, e.hash, c.len() as u64, c)?,
            }
            self.maybe_seal(&mut w)?;
        }
        levels[0].entries.push((e.hash, c.len() as u64));
        levels[0].len += c.len() as u64;
        // Carry full nodes upward.
        let mut i = 0;
        while levels[i].entries.len() == fanout {
            let node = if i + 1 == levels.len() {
                // Level i was the top: its node is the object so far. A new top
                // starts above it, and level i gets its own (fresh) hasher.
                levels.push(Level { hasher: blake3::Hasher::new(), entries: Vec::new(), len: 0 });
                let h = top_hash()?;
                let lv = &mut levels[i];
                self.write_node(h, &mut lv.entries, &mut lv.len)?
            } else {
                let lv = &mut levels[i];
                self.emit_node(&mut lv.hasher, &mut lv.entries, &mut lv.len)?
            };
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
        *hasher = blake3::Hasher::new();
        self.write_node(h, entries, len)
    }

    /// Writes one interior manifest node with a known hash and resets the level.
    fn write_node(&self, h: Hash, entries: &mut Vec<(Hash, u64)>, len: &mut u64) -> Result<(Hash, u64)> {
        let l = *len;
        if !self.dedup_hit(&h)? {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            self.append(&mut w, Kind::Manifest, h, l, &encode_manifest(entries))?;
            self.maybe_seal(&mut w)?;
        }
        entries.clear();
        *len = 0;
        Ok((h, l))
    }

    /// Leaf chunks of `hash` with their offsets: `(chunk hash, offset, len)`.
    /// A blob is one chunk at 0.
    fn leaf_chunks(&self, hash: &Hash) -> Result<Vec<(Hash, u64, u64)>> {
        let mut out = Vec::new();
        let mut stack: Vec<(Hash, u64)> = vec![(*hash, 0)];
        while let Some((h, off)) = stack.pop() {
            let loc = self.lookup(&h)?.ok_or(Error::NotFound(h))?;
            match loc.kind {
                Kind::Manifest => {
                    let m = self.read_manifest(&h, loc)?;
                    let mut o = off;
                    let mut kids = Vec::with_capacity(m.len());
                    for (c, l) in m {
                        kids.push((c, o));
                        o += l;
                    }
                    stack.extend(kids.into_iter().rev());
                }
                _ => out.push((h, off, loc.raw_len)),
            }
            if out.len() > MAX_DELTA_SEGS {
                return Err(Error::Parse("base has too many chunks for an anchored delta".into()));
            }
        }
        Ok(out)
    }

    /// Content-defined chunk hash → offset in `base`, for anchoring. A chunked
    /// base gives its stored chunks; any other base (a delta, a frame member)
    /// is chunked now with the store's FastCDC parameters, streaming.
    fn anchor_map(&self, base: &Hash) -> Result<HashMap<Hash, u64>> {
        let loc = self.lookup(base)?.ok_or(Error::NotFound(*base))?;
        if loc.kind == Kind::Manifest {
            return Ok(self.leaf_chunks(base)?.into_iter().map(|(h, o, _)| (h, o)).collect());
        }
        let cfg = &self.inner.cfg;
        let mut out = HashMap::new();
        let mut off = 0u64;
        for c in fastcdc::v2020::StreamCDC::new(self.open(base, None)?, cfg.cdc_min, cfg.cdc_avg, cfg.cdc_max) {
            let c = c.map_err(|e| Error::Corrupt(format!("chunking the base: {e:?}")))?;
            out.entry(Hash::of(&c.data)).or_insert(off);
            off += c.data.len() as u64;
        }
        Ok(out)
    }

    /// Stores `src` as a derived object of `base` with chunk-anchored XOR
    /// alignment (see [`pico2peta_core::PatchFormat::XorAnchored`]). Objects
    /// under the chunk threshold use an offset-aligned delta.
    pub fn put_delta_anchored(&self, base: &Hash, src: &mut dyn Read, opts: PutOpts) -> Result<Hash> {
        if !self.ensure_live_impl(base)? {
            return Err(Error::NotFound(*base));
        }
        let thr = self.inner.cfg.chunk_threshold;
        let mut head = Vec::with_capacity(thr.min(1 << 16));
        src.take(thr as u64).read_to_end(&mut head)?;
        if head.len() < thr {
            return self.put_delta_opts(base, None, &mut &head[..], opts);
        }
        let base_len = self.size(base)?;
        let anchors = self.anchor_map(base)?;
        let cfg = &self.inner.cfg;
        let reader = std::io::Cursor::new(head).chain(src);
        let mut cdc = fastcdc::v2020::StreamCDC::new(reader, cfg.cdc_min, cfg.cdc_avg, cfg.cdc_max);
        let mut whole = blake3::Hasher::new();
        let mut segs: Vec<DeltaSeg> = Vec::new();
        let mut shift: i64 = 0;
        let mut pos: u64 = 0;
        let mut failed: Option<Error> = None;
        let xor_chunks = std::iter::from_fn(|| {
            if failed.is_some() {
                return None;
            }
            let c = match cdc.next()? {
                Ok(c) => c.data,
                Err(fastcdc::v2020::Error::IoError(e)) => return Some(Err(Error::Io(e))),
                Err(e) => return Some(Err(Error::Corrupt(format!("chunker: {e:?}")))),
            };
            whole.update(&c);
            let o = pos;
            let l = c.len() as u64;
            pos += l;
            let anchored = anchors.get(&Hash::of(&c)).copied();
            if let Some(src_off) = anchored {
                shift = src_off as i64 - o as i64;
            }
            let src_off = o as i64 + shift;
            let mut x = c;
            if src_off >= 0 && (src_off as u64) < base_len {
                let s0 = src_off as u64;
                let n = l.min(base_len - s0);
                // Merge with the previous segment when contiguous on both sides.
                match segs.last_mut() {
                    Some(g) if g.dst + g.len == o && g.src + g.len == s0 => g.len += n,
                    _ => segs.push(DeltaSeg { dst: o, src: s0, len: n }),
                }
                if anchored.is_some() && n == l {
                    x.iter_mut().for_each(|b| *b = 0); // identical content
                } else {
                    match self.get(base, Some(s0..s0 + n)) {
                        Ok(b) => x.iter_mut().zip(&b).for_each(|(a, b)| *a ^= b),
                        Err(e) => {
                            failed = Some(Error::Corrupt(format!("reading the base: {e}")));
                            return Some(Err(Error::Corrupt("base read failed".into())));
                        }
                    }
                }
            }
            if segs.len() > MAX_DELTA_SEGS {
                return Some(Err(Error::Parse("too many delta segments".into())));
            }
            Some(Ok(x))
        });
        let xor = self.put_chunks(xor_chunks, opts.group_width.filter(|w| *w > 1))?;
        if let Some(e) = failed {
            return Err(e);
        }
        let hash = Hash(*whole.finalize().as_bytes());
        validate_segs(&segs, Some(base_len)).map_err(|e| Error::Corrupt(format!("anchored delta: {e}")))?;
        if self.dedup_hit(&hash)? {
            return Ok(hash);
        }
        // Always explicit segments (possibly none): `None` would mean offset alignment.
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        self.append(&mut w, Kind::Delta, hash, pos, &encode_delta(base, &xor, Some(&segs)))?;
        self.maybe_seal(&mut w)?;
        Ok(hash)
    }

    /// [`ObjectStore::put_stream`] with an explicit chunker. Objects under
    /// `chunk_threshold` are stored whole either way.
    pub fn put_stream_with(&self, chunker: ChunkerKind, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_opts(PutOpts { chunker, group_width: None }, src)
    }

    /// [`ObjectStore::put_stream`] with explicit options (see [`PutOpts`]).
    pub fn put_stream_opts(&self, opts: PutOpts, src: &mut dyn Read) -> Result<Hash> {
        let thr = self.inner.cfg.chunk_threshold;
        if opts.chunker == ChunkerKind::Xet && thr <= pico2peta_chunk::XET_MAX {
            // Chunks must stay below the threshold (they're stored as blobs).
            return Err(Error::Parse(format!(
                "Xet chunking needs pack.chunk_threshold > {} (have {thr})",
                pico2peta_chunk::XET_MAX
            )));
        }
        let group = match opts.group_width {
            None | Some(0 | 1) => None,
            Some(w) if (w as usize) <= crate::bytegroup::MAX_WIDTH => Some(w),
            Some(w) => return Err(Error::Parse(format!("byte-group width {w} out of range"))),
        };
        let mut head = Vec::with_capacity(thr.min(1 << 16));
        src.take(thr as u64).read_to_end(&mut head)?;
        if head.len() < thr {
            return self.put(&head);
        }
        let reader = std::io::Cursor::new(head).chain(src);
        match opts.chunker {
            ChunkerKind::FastCdc => {
                let cfg = &self.inner.cfg;
                let cdc = fastcdc::v2020::StreamCDC::new(reader, cfg.cdc_min, cfg.cdc_avg, cfg.cdc_max);
                let chunks = cdc.map(|r| {
                    r.map(|c| c.data).map_err(|e| match e {
                        fastcdc::v2020::Error::IoError(io) => Error::Io(io),
                        other => Error::Corrupt(format!("chunker: {other:?}")),
                    })
                });
                self.put_chunks(chunks, group)
            }
            ChunkerKind::Xet => {
                self.put_chunks(LengthChunks::new(reader, Box::new(pico2peta_chunk::XetChunker::new())), group)
            }
        }
    }

    /// Counters (local view, without a refresh).
    pub fn stats(&self) -> Result<PackStats> {
        // Lock order everywhere: writer, then index.
        let w = self.inner.writer.lock().map_err(poisoned)?;
        let idx = self.inner.index.read().map_err(poisoned)?;
        let mut s = PackStats {
            sealed_packs: idx.known.len(),
            objects: idx.map.len(),
            open_bytes: w.as_ref().map(|o| o.len).unwrap_or(0),
            ..Default::default()
        };
        let mut count = |kind: Kind, raw: u64| match kind {
            Kind::Frame | Kind::Frame2 => {}
            // Frame3 members are objects (indexed with their record's kind).
            Kind::Blob | Kind::Frame3 => {
                s.blobs += 1;
                s.logical_blob_bytes += raw;
            }
            Kind::Manifest => s.manifests += 1,
            Kind::Delta => s.deltas += 1,
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
            Kind::Delta => {
                let (b, x, _) = self.read_delta(hash, loc)?;
                Ok(Some(vec![b, x]))
            }
            Kind::Frame3 => {
                // A member of a frame with a reference prefix: the reference is a child.
                let (h, raw, _) = self.read_raw(hash, loc, true)?;
                let (_, payload) = decode_record(&raw)?;
                Ok(Some(frame_ref(&h, payload)?.into_iter().collect()))
            }
            Kind::Frame | Kind::Frame2 => Err(Error::Corrupt("frame record indexed as an object".into())),
        }
    }

    /// True for objects stored as plain data (a blob, or a frame member), as
    /// opposed to manifests and deltas.
    pub fn is_leaf(&self, hash: &Hash) -> Result<bool> {
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        Ok(matches!(loc.kind, Kind::Blob | Kind::Frame3))
    }

    fn read_delta(&self, hash: &Hash, loc: Loc) -> Result<(Hash, Hash, Option<Vec<DeltaSeg>>)> {
        let (_, data) = self.read_record(hash, loc, true)?;
        decode_delta(&data)
    }

    /// See [`ObjectStore::put_delta`].
    pub fn put_delta_impl(&self, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
        self.put_delta_mapped_impl(base, None, src)
    }

    /// The base as seen through an alignment: the bytes of `[start, end)` of the
    /// *new* object's coordinate space (`end = None` means unbounded, zero-filled).
    fn virtual_base(
        &self,
        base: &Hash,
        segs: Option<&[DeltaSeg]>,
        start: u64,
        end: Option<u64>,
        depth: usize,
    ) -> Result<Box<dyn Read + Send>> {
        let base_len = self.size(base)?;
        let ident = [DeltaSeg { dst: 0, src: 0, len: base_len }];
        let segs = segs.unwrap_or(&ident);
        let mut parts: Vec<Box<dyn Read + Send>> = Vec::new();
        let mut pos = start;
        for g in segs {
            let (gs, ge) = (g.dst, g.dst + g.len);
            if ge <= pos {
                continue;
            }
            if end.is_some_and(|e| gs >= e) {
                break;
            }
            if gs > pos {
                parts.push(Box::new(std::io::repeat(0).take(gs - pos)));
                pos = gs;
            }
            let pe = end.map_or(ge, |e| e.min(ge));
            let so = g.src + (pos - gs);
            parts.push(self.open_depth(base, Some(so..so + (pe - pos)), depth + 1)?);
            pos = pe;
        }
        match end {
            Some(e) if e > pos => parts.push(Box::new(std::io::repeat(0).take(e - pos))),
            Some(_) => {}
            None => parts.push(Box::new(std::io::repeat(0))),
        }
        Ok(Box::new(ChainAll { parts: parts.into_iter().collect(), cur: None }))
    }

    /// Like [`PackStore::put_delta_impl`] with an explicit alignment (for formats
    /// whose regions move between versions, such as reordered tensors).
    pub fn put_delta_mapped_impl(&self, base: &Hash, segs: Option<&[DeltaSeg]>, src: &mut dyn Read) -> Result<Hash> {
        self.put_delta_opts(base, segs, src, PutOpts::default())
    }

    /// Like [`PackStore::put_delta_mapped_impl`], storing the XOR stream with
    /// `opts` (chunker, byte grouping).
    pub fn put_delta_opts(
        &self,
        base: &Hash,
        segs: Option<&[DeltaSeg]>,
        src: &mut dyn Read,
        opts: PutOpts,
    ) -> Result<Hash> {
        if !self.ensure_live_impl(base)? {
            return Err(Error::NotFound(*base));
        }
        if let Some(sg) = segs {
            if sg.len() > MAX_DELTA_SEGS {
                return Err(Error::Parse("too many delta segments".into()));
            }
            validate_segs(sg, Some(self.size(base)?)).map_err(|e| Error::Parse(e.to_string()))?;
        }
        let mut xs = XorSource {
            src,
            base: self.virtual_base(base, segs, 0, None, 0)?,
            hasher: blake3::Hasher::new(),
            len: 0,
            tmp: Vec::new(),
        };
        let xor = self.put_stream_opts(opts, &mut xs)?;
        let hash = Hash(*xs.hasher.finalize().as_bytes());
        let len = xs.len;
        if self.dedup_hit(&hash)? {
            return Ok(hash);
        }
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        self.append(&mut w, Kind::Delta, hash, len, &encode_delta(base, &xor, segs))?;
        self.maybe_seal(&mut w)?;
        Ok(hash)
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
        let (h, raw, _) = self.read_raw(hash, loc, true)?;
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        if w.is_none() {
            *w = Some(self.new_open_pack()?);
        }
        let op = w.as_mut().ok_or_else(|| Error::Corrupt("no open pack".into()))?;
        let offset = op.len;
        self.inner.faults.hit("copy.write")?;
        op.file.write_all_at(&raw, offset)?;
        op.hasher.update(&raw);
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
        self.put_frame_ref(members, level, None)
    }

    /// [`PackStore::put_frame`] with an optional reference object: the frame's
    /// bodies are compressed against its bytes as zstd reference prefix (record
    /// kind `Frame3`). The reference must be a stored leaf object (blob or frame
    /// member) below the chunk threshold, and it stays a GC child of every member.
    pub fn put_frame_ref(&self, members: &[&[u8]], level: i32, reference: Option<&Hash>) -> Result<Vec<Hash>> {
        if members.iter().any(|m| m.len() >= self.inner.cfg.chunk_threshold) {
            return Err(Error::Parse("frame members must be smaller than the chunk threshold".into()));
        }
        let hashes: Vec<Hash> = members.iter().map(|m| Hash::of(m)).collect();
        let (kind, (codec, payload, fh, raw_len)) = match reference {
            None => (Kind::Frame2, encode_frame(members, level)?),
            Some(r) => {
                if !self.is_leaf(r)? || self.size(r)? >= self.inner.cfg.chunk_threshold as u64 {
                    return Err(Error::Parse(format!("frame reference {r} must be a small stored leaf object")));
                }
                let bytes = self.get(r, None)?;
                (Kind::Frame3, encode_frame_ref(members, level, r, &bytes)?)
            }
        };
        let member_kind = if kind == Kind::Frame3 { Kind::Frame3 } else { Kind::Blob };
        let rec = encode_record(kind, codec, &fh, raw_len, &payload);
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
        op.hasher.update(&rec);
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
                kind: member_kind,
                codec,
                frame,
            });
            let l = Loc {
                pack: PackRef::Open(op.generation),
                offset,
                stored_len,
                raw_len: mlen as u64,
                kind: member_kind,
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
                    // A broken object (a child is gone) is reported absent, so the
                    // caller uploads it again, and that put rewrites the missing chunks.
                    return Ok(false);
                }
            }
        }
        if loc.kind == Kind::Delta {
            let (b, x, _) = self.read_delta(hash, loc)?;
            for child in [b, x] {
                if !self.ensure_live_impl(&child)? {
                    return Ok(false);
                }
            }
        }
        if loc.kind == Kind::Frame3 {
            // A member of a frame with a reference prefix can't be read without it.
            for child in self.chunks(hash)?.unwrap_or_default() {
                if !self.ensure_live_impl(&child)? {
                    return Ok(false);
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

    /// Staging files quarantined by recovery because they held corrupt records.
    pub fn quarantined(&self) -> Result<Vec<PathBuf>> {
        let mut v: Vec<PathBuf> = fs::read_dir(&self.inner.staging_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.to_string_lossy().starts_with("corrupt-")))
            .collect();
        v.sort();
        Ok(v)
    }

    /// Index files whose pack is missing on the backend (for `fsck`).
    pub fn orphan_indexes(&self) -> Result<Vec<String>> {
        let listed = self.inner.backend.list("packs/")?;
        let keys: HashSet<&str> = listed.iter().map(|m| m.key.as_str()).collect();
        let mut v = Vec::new();
        for m in &listed {
            if let Some(name) = m.key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx"))
                && !keys.contains(format!("packs/{name}.pack").as_str())
                && self.inner.backend.head(&format!("packs/{name}.pack"))?.is_none()
            {
                v.push(name.to_string());
            }
        }
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
            return self.put_chunks(cdc.map(|c| Ok(data[c.offset..c.offset + c.length].to_vec())), None);
        }
        let hash = Hash::of(data);
        if self.dedup_hit(&hash)? {
            return Ok(hash);
        }
        // Compress and encode (CRC included) before taking the writer lock,
        // so concurrent writers only serialize on the append itself.
        let compressed = compress_with_probe(self.inner.cfg.zstd_level, self.inner.cfg.compress_probe, data);
        let (codec, payload): (Codec, &[u8]) = match &compressed {
            Some(c) => (Codec::Zstd, c),
            None => (Codec::Raw, data),
        };
        let rec = encode_record(Kind::Blob, codec, &hash, data.len() as u64, payload);
        let mut w = self.inner.writer.lock().map_err(poisoned)?;
        self.append_record(&mut w, Kind::Blob, hash, data.len() as u64, codec, &rec)?;
        self.maybe_seal(&mut w)?;
        Ok(hash)
    }

    fn put_many(&self, items: &[&[u8]]) -> Result<Vec<Hash>> {
        self.put_many_impl(items)
    }

    fn put_concat(&self, parts: &[(Hash, Range<u64>)], observe: &mut dyn FnMut(&[u8])) -> Result<Hash> {
        self.put_concat_impl(parts, observe)
    }

    fn put_chunked(&self, chunks: &[&[u8]]) -> Result<Hash> {
        // Chunks are stored as blobs, so each must stay below the threshold;
        // a larger one is split (still the same object, a finer layout).
        let thr = self.inner.cfg.chunk_threshold;
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        if total < thr {
            return self.put(&chunks.concat());
        }
        let piece = thr.max(2) - 1;
        let it = chunks.iter().filter(|c| !c.is_empty()).flat_map(|c| c.chunks(piece)).map(|c| Ok(c.to_vec()));
        self.put_chunks(it, None)
    }

    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_with(ChunkerKind::FastCdc, src)
    }

    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>> {
        // Whole-record fast path: the verified bytes are returned as they are
        // (no reader, no extra copies). Everything else streams into a buffer
        // sized up front.
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        if matches!(loc.kind, Kind::Blob | Kind::Frame3) {
            let r = check_range(range, loc.raw_len)?;
            let mut d = self.read_blob(hash, loc)?;
            if d.len() as u64 != loc.raw_len {
                return Err(Error::Corrupt(format!("index length disagrees with data for {hash}")));
            }
            if r.start == 0 && r.end == loc.raw_len {
                return Ok(d);
            }
            d.truncate(r.end as usize);
            return Ok(d.split_off(r.start as usize));
        }
        let len = check_range(range.clone(), loc.raw_len)?;
        let mut out = Vec::with_capacity((len.end - len.start).min(1 << 30) as usize);
        self.open(hash, range)?.read_to_end(&mut out).map_err(unwrap_io)?;
        Ok(out)
    }

    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>> {
        self.open_depth(hash, range, 0)
    }

    fn open_verified(
        &self,
        hash: &Hash,
        range: Option<Range<u64>>,
        verify: ReadVerify,
    ) -> Result<Box<dyn Read + Send>> {
        self.open_mode(hash, range, 0, verify)
    }

    fn size(&self, hash: &Hash) -> Result<u64> {
        self.lookup(hash)?.map(|l| l.raw_len).ok_or(Error::NotFound(*hash))
    }

    fn put_delta(&self, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
        self.put_delta_impl(base, src)
    }

    fn put_delta_mapped(&self, base: &Hash, segments: &[(u64, u64, u64)], src: &mut dyn Read) -> Result<Hash> {
        let segs: Vec<DeltaSeg> = segments.iter().map(|&(dst, src, len)| DeltaSeg { dst, src, len }).collect();
        self.put_delta_mapped_impl(base, Some(&segs), src)
    }

    fn put_stream_grouped(&self, src: &mut dyn Read, width: u8) -> Result<Hash> {
        self.put_stream_opts(PutOpts { chunker: ChunkerKind::FastCdc, group_width: Some(width) }, src)
    }

    fn put_delta_mapped_grouped(
        &self,
        base: &Hash,
        segments: &[(u64, u64, u64)],
        src: &mut dyn Read,
        width: u8,
    ) -> Result<Hash> {
        let segs: Vec<DeltaSeg> = segments.iter().map(|&(dst, src, len)| DeltaSeg { dst, src, len }).collect();
        self.put_delta_opts(base, Some(&segs), src, PutOpts { chunker: ChunkerKind::FastCdc, group_width: Some(width) })
    }

    fn put_encoded(&self, src: &mut dyn Read, enc: &pico2peta_core::Encoding) -> Result<Hash> {
        let opts = PutOpts { chunker: ChunkerKind::from_id(enc.chunker.as_deref())?, group_width: enc.group_width };
        match &enc.derive {
            None => self.put_stream_opts(opts, src),
            Some(d) => match d.patch {
                pico2peta_core::PatchFormat::Xor => {
                    let segs: Vec<DeltaSeg> =
                        d.segments.iter().map(|&(dst, src, len)| DeltaSeg { dst, src, len }).collect();
                    let segs = if segs.is_empty() { None } else { Some(&segs[..]) };
                    self.put_delta_opts(&d.base, segs, src, opts)
                }
                pico2peta_core::PatchFormat::XorAnchored => self.put_delta_anchored(&d.base, src, opts),
                _ => Err(Error::Unsupported("patch format not supported by this store")),
            },
        }
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

impl PackStore {
    fn open_depth(&self, hash: &Hash, range: Option<Range<u64>>, depth: usize) -> Result<Box<dyn Read + Send>> {
        self.open_mode(hash, range, depth, ReadVerify::Strict)
    }

    fn open_mode(
        &self,
        hash: &Hash,
        range: Option<Range<u64>>,
        depth: usize,
        mode: ReadVerify,
    ) -> Result<Box<dyn Read + Send>> {
        if depth > MAX_DELTA_DEPTH {
            return Err(Error::Corrupt(format!("delta chain too deep at {hash}")));
        }
        let loc = self.lookup(hash)?.ok_or(Error::NotFound(*hash))?;
        let whole = range.is_none();
        let r = check_range(range, loc.raw_len)?;
        match loc.kind {
            Kind::Delta => {
                let (base, xor, segs) = self.read_delta(hash, loc)?;
                let base_r = self.virtual_base(&base, segs.as_deref(), r.start, Some(r.end), depth)?;
                let xor_r = self.open_depth(&xor, Some(r.clone()), depth + 1)?;
                Ok(Box::new(XorReader {
                    base: base_r,
                    xor: xor_r,
                    tmp: Vec::new(),
                    verify: whole.then(|| (blake3::Hasher::new(), *hash)),
                }))
            }
            Kind::Frame | Kind::Frame2 => Err(Error::Corrupt("frame record indexed as an object".into())),
            Kind::Blob | Kind::Frame3 => {
                let mut d = self.read_blob(hash, loc)?;
                if d.len() as u64 != loc.raw_len {
                    return Err(Error::Corrupt(format!("index length disagrees with data for {hash}")));
                }
                if r.start == 0 && r.end == loc.raw_len {
                    return Ok(Box::new(std::io::Cursor::new(d))); // no copy for whole reads
                }
                d.truncate(r.end as usize);
                Ok(Box::new(std::io::Cursor::new(d.split_off(r.start as usize))))
            }
            Kind::Manifest => {
                let pieces = overlapping(self.read_manifest(hash, loc)?, r.start, r.end);
                Ok(Box::new(ChunkReader::new(self.clone(), pieces, whole.then_some(*hash), r.end - r.start, mode)))
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Best effort. If this fails, the next open adopts the staging file.
        if self.faults.crashed() {
            return; // simulate a dead process: leave the staging file behind
        }
        if let Ok(mut p) = self.pending.lock() {
            for op in p.drain(..) {
                let _ = self.seal_file(&op.path, &op.file, op.len, op.entries);
            }
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

/// Turns a length-reporting streaming [`pico2peta_chunk::Chunker`] over a
/// reader into an iterator of owned chunks.
struct LengthChunks<R> {
    r: R,
    c: Box<dyn pico2peta_chunk::Chunker>,
    buf: Vec<u8>,
    start: usize,
    lens: std::collections::VecDeque<usize>,
    scratch: Vec<usize>,
    done: bool,
}

/// Read size for [`LengthChunks`].
const LENGTH_CHUNKS_BLOCK: usize = 1 << 20;

impl<R: Read> LengthChunks<R> {
    fn new(r: R, c: Box<dyn pico2peta_chunk::Chunker>) -> Self {
        LengthChunks { r, c, buf: Vec::new(), start: 0, lens: Default::default(), scratch: Vec::new(), done: false }
    }
}

impl<R: Read> Iterator for LengthChunks<R> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Result<Vec<u8>>> {
        loop {
            if let Some(l) = self.lens.pop_front() {
                let end = self.start + l;
                if end > self.buf.len() {
                    return Some(Err(Error::Corrupt("chunker reported more bytes than it was given".into())));
                }
                let v = self.buf[self.start..end].to_vec();
                self.start = end;
                return Some(Ok(v));
            }
            if self.done {
                return None;
            }
            // Drop emitted bytes (what's left is at most one partial chunk).
            self.buf.drain(..self.start);
            self.start = 0;
            let old = self.buf.len();
            self.buf.resize(old + LENGTH_CHUNKS_BLOCK, 0);
            let n = match self.r.read(&mut self.buf[old..]) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    self.buf.truncate(old);
                    continue;
                }
                Err(e) => {
                    self.buf.truncate(old);
                    self.done = true;
                    return Some(Err(Error::Io(e)));
                }
            };
            self.buf.truncate(old + n);
            self.scratch.clear();
            if n == 0 {
                self.c.finish(&mut self.scratch);
                self.done = true;
            } else {
                self.c.update(&self.buf[old..], &mut self.scratch);
            }
            self.lens.extend(self.scratch.iter().copied());
        }
    }
}

/// One chunk out of a `put_chunks` worker: its bytes, hash and, unless it was
/// already stored, its encoded record.
struct Encoded {
    data: Arc<Vec<u8>>,
    hash: Hash,
    rec: Option<(Codec, Vec<u8>)>,
}

/// zstd-compresses `raw` at `level`, or returns `None` when that doesn't help (or level 0).
/// Order-0 entropy check for records too small to sample: at least 7.5
/// bits/byte (uniform random 1 KiB measures ~7.8; source code ~4.5-5.5) means
/// zstd won't help. Cheap next to a zstd attempt on small records.
fn looks_random(raw: &[u8]) -> bool {
    if raw.len() < 256 || raw.len() > PROBE_MIN {
        return false; // too few bytes to judge (let zstd try); larger records use the probe
    }
    // Four interleaved histograms avoid the store-to-load dependency of one.
    let mut c = [[0u32; 256]; 4];
    let (quads, rest) = raw.as_chunks::<4>();
    for q in quads {
        c[0][q[0] as usize] += 1;
        c[1][q[1] as usize] += 1;
        c[2][q[2] as usize] += 1;
        c[3][q[3] as usize] += 1;
    }
    for &b in rest {
        c[0][b as usize] += 1;
    }
    // H = log2(n) - sum(c log2 c) / n, with c log2 c from a table.
    let table = CLOGC
        .get_or_init(|| (0..=PROBE_MIN).map(|c| if c == 0 { 0.0 } else { c as f32 * (c as f32).log2() }).collect());
    let mut sum = 0f32;
    for i in 0..256 {
        sum += table[(c[0][i] + c[1][i] + c[2][i] + c[3][i]) as usize];
    }
    let n = raw.len() as f32;
    n.log2() - sum / n >= 7.5
}

/// `c * log2(c)` for `c` in `0..=PROBE_MIN` (see [`looks_random`]).
static CLOGC: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();

/// Minimum record size at which the compressibility probe runs.
const PROBE_MIN: usize = 8 << 10;
/// Probe sample size.
const PROBE_SAMPLE: usize = 4 << 10;

/// zstd at `level`, or `None` (store raw) if it doesn't help. With `probe`,
/// records of at least `PROBE_MIN` bytes whose first 4 KiB shrink by less than 3%
/// under zstd-1 are stored raw without running the full compression.
fn compress_with_probe(level: i32, probe: bool, raw: &[u8]) -> Option<Vec<u8>> {
    if level == 0 || raw.len() < 64 {
        return None;
    }
    if probe && raw.len() < PROBE_MIN && looks_random(raw) {
        return None;
    }
    if probe && raw.len() >= PROBE_MIN {
        let sample = &raw[..PROBE_SAMPLE];
        let small = zstd_compress(sample, 1)?;
        if small.len() * 100 >= sample.len() * 97 {
            return None;
        }
    }
    zstd_compress(raw, level).filter(|c| c.len() < raw.len())
}

/// Encodes a blob record as `GroupedZstd` (`width u8 | zstd(group(raw))`),
/// or raw if that doesn't make it smaller (or compression is off).
fn encode_grouped_or_raw(level: i32, width: u8, hash: &Hash, raw: &[u8]) -> (Codec, Vec<u8>) {
    let raw_rec = || (Codec::Raw, encode_record(Kind::Blob, Codec::Raw, hash, raw.len() as u64, raw));
    if level == 0 || raw.len() < 64 {
        return raw_rec();
    }
    let Ok(g) = crate::bytegroup::group(raw, width as usize) else {
        return raw_rec();
    };
    match zstd_compress(&g, level) {
        Some(z) if z.len() + 1 < raw.len() => {
            let mut payload = Vec::with_capacity(z.len() + 1);
            payload.push(width);
            payload.extend_from_slice(&z);
            (Codec::GroupedZstd, encode_record(Kind::Blob, Codec::GroupedZstd, hash, raw.len() as u64, &payload))
        }
        _ => raw_rec(),
    }
}

thread_local! {
    /// Per-thread zstd contexts, keyed by level (in practice two: the probe's
    /// level 1 and the configured level). Creating a context per chunk costs an
    /// allocation and a table reset each time.
    static ZSTD_CTX: std::cell::RefCell<Vec<(i32, zstd::bulk::Compressor<'static>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// `zstd::bulk::compress`, with a reused per-thread context. Output is the
/// same bytes: the context's parameters are just the level.
fn zstd_compress(raw: &[u8], level: i32) -> Option<Vec<u8>> {
    ZSTD_CTX.with(|cell| {
        let Ok(mut ctxs) = cell.try_borrow_mut() else {
            return zstd::bulk::compress(raw, level).ok();
        };
        let i = match ctxs.iter().position(|(l, _)| *l == level) {
            Some(i) => i,
            None => {
                ctxs.push((level, zstd::bulk::Compressor::new(level).ok()?));
                ctxs.len() - 1
            }
        };
        ctxs[i].1.compress(raw).ok()
    })
}

/// Below this many bytes a `put_many` batch is hashed and compressed on the
/// calling thread (thread start-up would cost more than it saves).
const PUT_MANY_PARALLEL_BYTES: usize = 256 << 10;

/// `items.map(f)` on up to 4 scoped threads, results in order. Workers take
/// items from a shared counter, so uneven item sizes still balance.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Result<Vec<R>> {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4).min(items.len());
    if workers <= 1 {
        return Ok(items.iter().map(&f).collect());
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut v = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= items.len() {
                            break v;
                        }
                        v.push((i, f(&items[i])));
                    }
                })
            })
            .collect();
        let mut all = Vec::with_capacity(items.len());
        for h in hs {
            all.extend(h.join().map_err(|_| Error::Corrupt("put_many worker panicked".into()))?);
        }
        Ok::<_, Error>(all)
    })?;
    out.sort_unstable_by_key(|(i, _)| *i);
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

/// Maximum length of a delta chain followed on reads.
const MAX_DELTA_DEPTH: usize = 32;

/// Reads the new object from `src`, hashes it, and yields `src XOR base`.
struct XorSource<'a> {
    src: &'a mut dyn Read,
    /// The (aligned, zero-extended, unbounded) base.
    base: Box<dyn Read + Send>,
    hasher: blake3::Hasher,
    len: u64,
    tmp: Vec<u8>,
}

impl Read for XorSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.src.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        if n > 0 {
            self.tmp.resize(n, 0);
            self.base.read_exact(&mut self.tmp[..n])?;
            for (a, b) in buf[..n].iter_mut().zip(&self.tmp[..n]) {
                *a ^= b;
            }
        }
        Ok(n)
    }
}

/// Concatenates readers.
struct ChainAll {
    parts: std::collections::VecDeque<Box<dyn Read + Send>>,
    cur: Option<Box<dyn Read + Send>>,
}

impl Read for ChainAll {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.cur.is_none() {
                match self.parts.pop_front() {
                    Some(p) => self.cur = Some(p),
                    None => return Ok(0),
                }
            }
            let Some(c) = self.cur.as_mut() else { return Ok(0) };
            let n = c.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            self.cur = None;
        }
    }
}

/// `base XOR xor`, streamed in lockstep (both cover the same range).
struct XorReader {
    base: Box<dyn Read + Send>,
    xor: Box<dyn Read + Send>,
    tmp: Vec<u8>,
    verify: Option<(blake3::Hasher, Hash)>,
}

impl Read for XorReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.xor.read(buf)?;
        if n == 0 {
            if let Some((h, want)) = self.verify.take()
                && Hash(*h.finalize().as_bytes()) != want
            {
                return Err(io_err(Error::Corrupt(format!("delta reassembly hash mismatch for {want}"))));
            }
            return Ok(0);
        }
        self.tmp.resize(n, 0);
        self.base.read_exact(&mut self.tmp[..n])?;
        for (a, b) in buf[..n].iter_mut().zip(&self.tmp[..n]) {
            *a ^= b;
        }
        if let Some((h, _)) = self.verify.as_mut() {
            h.update(&buf[..n]);
        }
        Ok(n)
    }
}

/// Maximum nesting of frame references followed by one read.
const MAX_REF_DEPTH: u32 = 16;

thread_local! {
    static REF_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Chunks the `put_chunks` pipeline may hold in flight, per worker.
const PIPELINE_CREDITS_PER_WORKER: usize = 8;

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
/// is bounded by tree depth times fanout (plus the read-ahead window),
/// whatever the object's size.
///
/// Large reads use read-ahead: a few worker threads fetch, decompress and
/// verify the next chunks in parallel, while the reader returns them strictly
/// in order and hashes the whole object.
struct ChunkReader {
    store: PackStore,
    /// One iterator per tree level: `(hash, start, end)` local to that entry.
    stack: Vec<std::vec::IntoIter<(Hash, u64, u64)>>,
    /// The chunk being returned and the read position in it.
    cur: (Arc<Vec<u8>>, usize),
    /// Whole-object reads also verify the reassembled hash at EOF.
    verify: Option<(blake3::Hasher, Hash)>,
    /// Large whole reads hash on a separate thread instead.
    hasher: Option<VerifyThread>,
    depth_limit: usize,
    /// Chunks requested but not yet returned, in order.
    pending: std::collections::VecDeque<std::sync::mpsc::Receiver<Result<Vec<u8>>>>,
    /// Read-ahead pool (started lazily for large reads) and its size.
    pool: Option<(std::sync::mpsc::Sender<ChunkJob>, usize)>,
    /// Bytes this reader will return (decides whether read-ahead pays off).
    span: u64,
    /// Check each chunk's BLAKE3 before returning it ([`ReadVerify::Strict`]).
    /// Off only for whole reads in [`ReadVerify::Eof`] mode, which still check
    /// record CRCs and the whole-object hash at EOF.
    verify_chunks: bool,
}

/// A whole-object hash running on its own thread: chunk sender, the thread,
/// and the expected hash.
type VerifyThread = (std::sync::mpsc::SyncSender<Arc<Vec<u8>>>, std::thread::JoinHandle<blake3::Hasher>, Hash);

/// One read-ahead job: fetch and verify chunk `hash`, return bytes `[a, b)`.
struct ChunkJob {
    hash: Hash,
    loc: Loc,
    a: u64,
    b: u64,
    verify: bool,
    reply: std::sync::mpsc::SyncSender<Result<Vec<u8>>>,
}

/// Reads at least this large use read-ahead workers.
const READAHEAD_MIN: u64 = 2 << 20;
/// Chunks in flight per read-ahead worker.
const READAHEAD_PER_WORKER: usize = 4;

impl PackStore {
    /// Fetches chunk `hash` (verifying it) and returns bytes `[a, b)`, moving
    /// the buffer when the whole chunk is wanted.
    fn chunk_slice(&self, hash: &Hash, loc: Loc, a: u64, b: u64, verify: bool) -> Result<Vec<u8>> {
        // Unverified (eof mode) still checks the record CRC (`read_record`).
        let mut d = if verify { self.read_blob(hash, loc)? } else { self.read_record(hash, loc, true)?.1 };
        if a == 0 && b as usize == d.len() {
            return Ok(d);
        }
        if a > b || b as usize > d.len() {
            return Err(Error::Corrupt("chunk shorter than manifest".into()));
        }
        d.truncate(b as usize);
        Ok(d.split_off(a as usize))
    }
}

impl ChunkReader {
    fn new(store: PackStore, pieces: Vec<(Hash, u64, u64)>, verify: Option<Hash>, span: u64, mode: ReadVerify) -> Self {
        // Per-chunk hashes may be skipped only when the whole object is verified at EOF.
        let verify_chunks = !(mode == ReadVerify::Eof && verify.is_some());
        // Large whole reads: hash on a separate thread, fed the same buffers.
        let hasher = verify.filter(|_| span >= READAHEAD_MIN).and_then(|want| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Arc<Vec<u8>>>(READAHEAD_PER_WORKER * 4);
            std::thread::Builder::new()
                .name("p2p-verify".into())
                .spawn(move || {
                    let mut h = blake3::Hasher::new();
                    for c in rx {
                        h.update(&c);
                    }
                    h
                })
                .ok()
                .map(|j| (tx, j, want))
        });
        ChunkReader {
            store,
            stack: vec![pieces.into_iter()],
            cur: (Arc::new(Vec::new()), 0),
            verify: if hasher.is_some() { None } else { verify.map(|h| (blake3::Hasher::new(), h)) },
            hasher,
            depth_limit: 16,
            pending: Default::default(),
            pool: None,
            span,
            verify_chunks,
        }
    }

    /// Next leaf chunk `(hash, loc, a, b)`, expanding manifests on the way.
    fn next_leaf(&mut self) -> Result<Option<(Hash, Loc, u64, u64)>> {
        loop {
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
            let Some((h, a, b)) = next else { return Ok(None) };
            let loc = self.store.lookup(&h)?.ok_or_else(|| Error::Corrupt(format!("missing chunk {h}")))?;
            match loc.kind {
                Kind::Frame | Kind::Frame2 | Kind::Delta => {
                    return Err(Error::Corrupt("unexpected record kind inside a manifest".into()));
                }
                Kind::Frame3 => return Ok(Some((h, loc, a, b))),
                Kind::Manifest => {
                    if self.stack.len() >= self.depth_limit {
                        return Err(Error::Corrupt("manifest tree too deep".into()));
                    }
                    let m = self.store.read_manifest(&h, loc)?;
                    self.stack.push(overlapping(m, a, b).into_iter());
                }
                Kind::Blob => return Ok(Some((h, loc, a, b))),
            }
        }
    }

    fn start_pool(&mut self) -> usize {
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(1).clamp(1, 4);
        let (tx, rx) = std::sync::mpsc::channel::<ChunkJob>();
        let rx = Arc::new(Mutex::new(rx));
        let mut spawned_n = 0;
        for _ in 0..workers {
            let rx = rx.clone();
            let store = self.store.clone();
            let spawned = std::thread::Builder::new().name("p2p-readahead".into()).spawn(move || {
                loop {
                    let job = match rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    let Ok(j) = job else { return };
                    let _ = j.reply.send(store.chunk_slice(&j.hash, j.loc, j.a, j.b, j.verify));
                }
            });
            if spawned.is_err() {
                break; // fewer workers (or none: the reader then works inline)
            }
            spawned_n += 1;
        }
        self.pool = Some((tx, spawned_n.max(1)));
        spawned_n.max(1)
    }

    /// The next chunk's bytes, in order (`None` at the end).
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if self.span < READAHEAD_MIN {
            return match self.next_leaf()? {
                Some((h, loc, a, b)) => self.store.chunk_slice(&h, loc, a, b, self.verify_chunks).map(Some),
                None => Ok(None),
            };
        }
        let window = READAHEAD_PER_WORKER
            * match &self.pool {
                Some((_, n)) => *n,
                None => self.start_pool(),
            };
        while self.pending.len() < window {
            let Some((hash, loc, a, b)) = self.next_leaf()? else { break };
            let (reply, rx) = std::sync::mpsc::sync_channel(1);
            let job = ChunkJob { hash, loc, a, b, verify: self.verify_chunks, reply };
            let job = match &self.pool {
                Some((p, _)) => match p.send(job) {
                    Ok(()) => {
                        self.pending.push_back(rx);
                        continue;
                    }
                    Err(std::sync::mpsc::SendError(j)) => j,
                },
                None => job,
            };
            // No workers: do it here (the reply channel has room for one).
            let _ = job.reply.send(self.store.chunk_slice(&job.hash, job.loc, job.a, job.b, job.verify));
            self.pending.push_back(rx);
        }
        match self.pending.pop_front() {
            None => Ok(None),
            Some(rx) => rx.recv().map_err(|_| Error::Corrupt("read-ahead worker died".into()))?.map(Some),
        }
    }
}

fn io_err(e: Error) -> std::io::Error {
    std::io::Error::other(e)
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let (data, pos) = &mut self.cur;
            let n = buf.len().min(data.len() - *pos);
            if n > 0 || buf.is_empty() {
                buf[..n].copy_from_slice(&data[*pos..*pos + n]);
                *pos += n;
                return Ok(n);
            }
            match self.next_chunk().map_err(io_err)? {
                None => {
                    if let Some((hs, want)) = self.verify.take()
                        && Hash(*hs.finalize().as_bytes()) != want
                    {
                        return Err(io_err(Error::Corrupt(format!("reassembled hash mismatch for {want}"))));
                    }
                    if let Some((tx, join, want)) = self.hasher.take() {
                        drop(tx);
                        let hs = join.join().map_err(|_| io_err(Error::Corrupt("verify thread panicked".into())))?;
                        if Hash(*hs.finalize().as_bytes()) != want {
                            return Err(io_err(Error::Corrupt(format!("reassembled hash mismatch for {want}"))));
                        }
                    }
                    return Ok(0);
                }
                Some(d) => {
                    let d = Arc::new(d);
                    if let Some((hs, _)) = self.verify.as_mut() {
                        hs.update(&d);
                    }
                    if let Some((tx, _, _)) = &self.hasher
                        && tx.send(d.clone()).is_err()
                    {
                        return Err(io_err(Error::Corrupt("verify thread died".into())));
                    }
                    self.cur = (d, 0);
                }
            }
        }
    }
}
