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

/// Tunables for the pack store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackConfig {
    /// Objects at least this large are chunked.
    pub chunk_threshold: usize,
    /// FastCDC minimum, average and maximum chunk sizes.
    pub cdc_min: u32,
    pub cdc_avg: u32,
    pub cdc_max: u32,
    /// The open pack is sealed once it reaches this many bytes.
    pub pack_cap: u64,
    /// zstd level for record payloads. 0 disables compression.
    pub zstd_level: i32,
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
            && (-7..=22).contains(&self.zstd_level);
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
}

/// Summary counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackStats {
    pub sealed_packs: usize,
    pub objects: usize,
    pub blobs: usize,
    pub manifests: usize,
    pub open_bytes: u64,
    pub logical_blob_bytes: u64,
}

#[derive(Default)]
struct Index {
    map: HashMap<Hash, Loc>,
    /// Sealed pack names (hex), indexed by `PackRef::Sealed`.
    sealed: Vec<String>,
    known: HashSet<String>,
}

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
}

impl Inner {
    fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.faults.hit(point)?)
    }

    /// Seals a fully written, locked staging file: fsync, upload pack, upload
    /// index, unlink. Idempotent and crash-safe at every step.
    fn seal_file(&self, path: &Path, f: &File, len: u64, mut entries: Vec<IdxEntry>) -> Result<()> {
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
        Ok(())
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
            }),
        };
        s.refresh()?;
        s.recover_orphans()?;
        Ok(s)
    }

    pub fn config(&self) -> &PackConfig {
        &self.inner.cfg
    }

    fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.inner.faults.hit(point)?)
    }

    /// Loads indexes of sealed packs this handle hasn't seen yet. Returns
    /// the number of new packs.
    pub fn refresh(&self) -> Result<usize> {
        let listed = self.inner.backend.list("packs/")?;
        let names: HashSet<&str> = listed.iter().map(|m| m.key.as_str()).collect();
        let mut new = Vec::new();
        {
            let idx = self.inner.index.read().map_err(poisoned)?;
            for m in &listed {
                let Some(name) = m.key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx")) else { continue };
                if !idx.known.contains(name) {
                    new.push(name.to_string());
                }
            }
        }
        let mut loaded = Vec::new();
        for name in new {
            if !names.contains(format!("packs/{name}.pack").as_str()) {
                return Err(Error::Corrupt(format!("index without pack: {name}")));
            }
            let mut buf = Vec::new();
            self.inner.backend.open(&format!("packs/{name}.idx"))?.read_to_end(&mut buf)?;
            loaded.push((name, decode_index(&buf)?));
        }
        let n = loaded.len();
        let mut idx = self.inner.index.write().map_err(poisoned)?;
        for (name, entries) in loaded {
            if !idx.known.insert(name.clone()) {
                continue;
            }
            let pid = idx.sealed.len() as u32;
            idx.sealed.push(name);
            for e in entries {
                let loc = Loc {
                    pack: PackRef::Sealed(pid),
                    offset: e.offset,
                    stored_len: e.stored_len,
                    raw_len: e.raw_len,
                    kind: e.kind,
                };
                // Prefer sealed locations over our own open pack.
                match idx.map.get(&e.hash) {
                    Some(Loc { pack: PackRef::Sealed(_), .. }) => {}
                    _ => {
                        idx.map.insert(e.hash, loc);
                    }
                }
            }
        }
        Ok(n)
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
        if len >= 8 && f.read_exact_at(&mut magic, 0).is_ok() && &magic == PACK_MAGIC {
            good = 8;
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
                if decode_record(&rec).is_err() {
                    break;
                }
                entries.push(IdxEntry {
                    hash: h.hash,
                    offset: good,
                    stored_len: h.stored_len,
                    raw_len: h.raw_len,
                    kind: h.kind,
                    codec: h.codec,
                });
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
        f.write_all(PACK_MAGIC)?;
        // Rename while holding the lock. Recovery only considers `*.pack`,
        // so a staging file is never adopted before its lock is held.
        fs::rename(&tmp, &path)?;
        fsync_dir(&self.inner.staging_dir)?;
        Ok(OpenPack {
            file: Arc::new(f),
            path,
            generation: self.inner.generation.fetch_add(1, Ordering::Relaxed),
            len: PACK_MAGIC.len() as u64,
            entries: Vec::new(),
        })
    }

    /// Compresses (if useful) and appends one record to the open pack.
    fn append(&self, w: &mut Option<OpenPack>, kind: Kind, hash: Hash, raw_len: u64, raw: &[u8]) -> Result<()> {
        let compressed = if self.inner.cfg.zstd_level != 0 && raw.len() >= 64 {
            zstd::bulk::compress(raw, self.inner.cfg.zstd_level).ok().filter(|c| c.len() < raw.len())
        } else {
            None
        };
        let (codec, payload): (Codec, &[u8]) = match &compressed {
            Some(c) => (Codec::Zstd, c),
            None => (Codec::Raw, raw),
        };
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
        op.entries.push(IdxEntry { hash, offset, stored_len, raw_len, kind, codec });
        let loc = Loc { pack: PackRef::Open(op.generation), offset, stored_len, raw_len, kind };
        self.inner.index.write().map_err(poisoned)?.map.entry(hash).or_insert(loc);
        Ok(())
    }

    /// Seals the open pack if there is one.
    fn seal_locked(&self, w: &mut Option<OpenPack>) -> Result<()> {
        let Some(op) = w.take() else { return Ok(()) };
        let OpenPack { file, path, generation, len, entries } = op;
        let names: Vec<Hash> = entries.iter().map(|e| e.hash).collect();
        if let Err(e) = self.inner.seal_file(&path, &file, len, entries.clone()) {
            // Keep the open pack so a later flush can retry. Our records stay readable.
            *w = Some(OpenPack { file, path, generation, len, entries });
            return Err(e);
        }
        drop(file); // releases the flock
        self.refresh()?;
        // Any record still pointing at this generation now lives in the sealed pack.
        let idx = self.inner.index.read().map_err(poisoned)?;
        for h in names {
            if let Some(Loc { pack: PackRef::Open(g), .. }) = idx.map.get(&h)
                && *g == generation
            {
                return Err(Error::Corrupt(format!("sealed pack not visible after refresh for {h}")));
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
        Ok(self.inner.index.read().map_err(poisoned)?.map.get(hash).copied())
    }

    /// Looks `hash` up, re-listing the backend once on a miss.
    fn lookup(&self, hash: &Hash) -> Result<Option<Loc>> {
        if let Some(l) = self.lookup_local(hash)? {
            return Ok(Some(l));
        }
        if self.refresh()? > 0 { self.lookup_local(hash) } else { Ok(None) }
    }

    fn read_record(&self, hash: &Hash, mut loc: Loc) -> Result<(RecHeader, Vec<u8>)> {
        for _ in 0..3 {
            let total = (REC_HEADER + loc.stored_len as usize + REC_TRAILER) as u64;
            let raw = match loc.pack {
                PackRef::Sealed(pid) => {
                    let name = self.inner.index.read().map_err(poisoned)?.sealed[pid as usize].clone();
                    self.inner.backend.get_range(&format!("packs/{name}.pack"), loc.offset..loc.offset + total)?
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
            let (h, payload) = decode_record(&raw)?;
            if h.hash != *hash {
                return Err(Error::Corrupt(format!("record at index location holds {} not {hash}", h.hash)));
            }
            return Ok((h, unpack_payload(&h, payload)?));
        }
        Err(Error::Corrupt(format!("could not resolve location of {hash}")))
    }

    fn read_blob(&self, hash: &Hash, loc: Loc) -> Result<Vec<u8>> {
        let (_, data) = self.read_record(hash, loc)?;
        if Hash::of(&data) != *hash {
            return Err(Error::Corrupt(format!("content hash mismatch for {hash}")));
        }
        Ok(data)
    }

    fn read_manifest(&self, hash: &Hash, loc: Loc) -> Result<Vec<(Hash, u64)>> {
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
        let mut hasher = blake3::Hasher::new();
        let mut manifest = Vec::new();
        let mut total = 0u64;
        for c in chunks {
            let c = c?;
            hasher.update(&c);
            total += c.len() as u64;
            let ch = Hash::of(&c);
            if self.lookup_local(&ch)?.is_none() {
                let mut w = self.inner.writer.lock().map_err(poisoned)?;
                self.append(&mut w, Kind::Blob, ch, c.len() as u64, &c)?;
                self.maybe_seal(&mut w)?;
            }
            manifest.push((ch, c.len() as u64));
        }
        let hash = Hash(*hasher.finalize().as_bytes());
        if self.lookup_local(&hash)?.is_none() {
            let mut w = self.inner.writer.lock().map_err(poisoned)?;
            self.append(&mut w, Kind::Manifest, hash, total, &encode_manifest(&manifest))?;
            self.maybe_seal(&mut w)?;
        }
        Ok(hash)
    }

    /// Counters (local view, without a refresh).
    pub fn stats(&self) -> Result<PackStats> {
        let idx = self.inner.index.read().map_err(poisoned)?;
        let w = self.inner.writer.lock().map_err(poisoned)?;
        let mut s = PackStats {
            sealed_packs: idx.sealed.len(),
            objects: idx.map.len(),
            open_bytes: w.as_ref().map(|o| o.len).unwrap_or(0),
            ..Default::default()
        };
        for l in idx.map.values() {
            match l.kind {
                Kind::Blob => {
                    s.blobs += 1;
                    s.logical_blob_bytes += l.raw_len;
                }
                Kind::Manifest => s.manifests += 1,
            }
        }
        Ok(s)
    }

    /// Every hash addressable through `get` (objects and chunks), sorted.
    pub fn hashes(&self) -> Result<Vec<Hash>> {
        let mut v: Vec<Hash> = self.inner.index.read().map_err(poisoned)?.map.keys().copied().collect();
        v.sort();
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
        if self.lookup_local(&hash)?.is_some() {
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
            Kind::Blob => {
                let d = self.read_blob(hash, loc)?;
                Ok(Box::new(std::io::Cursor::new(d[r.start as usize..r.end as usize].to_vec())))
            }
            Kind::Manifest => {
                let m = self.read_manifest(hash, loc)?;
                let mut pieces = Vec::new();
                let mut pos = 0u64;
                for (ch, len) in m {
                    let (cs, ce) = (pos, pos + len);
                    pos = ce;
                    if ce > r.start && cs < r.end {
                        pieces.push((ch, r.start.max(cs) - cs, r.end.min(ce) - cs));
                    }
                }
                Ok(Box::new(ChunkReader {
                    store: self.clone(),
                    pieces: pieces.into_iter(),
                    cur: std::io::Cursor::new(Vec::new()),
                    verify: whole.then(|| (blake3::Hasher::new(), *hash)),
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

/// Converts the `io::Error` wrapping of a core error back into it.
fn unwrap_io(e: std::io::Error) -> Error {
    match e.downcast::<Error>() {
        Ok(inner) => inner,
        Err(e) => Error::Io(e),
    }
}

/// Streams a chunked object one chunk at a time.
struct ChunkReader {
    store: PackStore,
    pieces: std::vec::IntoIter<(Hash, u64, u64)>,
    cur: std::io::Cursor<Vec<u8>>,
    /// Whole-object reads also verify the reassembled hash at EOF.
    verify: Option<(blake3::Hasher, Hash)>,
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.cur.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let Some((ch, a, b)) = self.pieces.next() else {
                if let Some((h, want)) = self.verify.take()
                    && Hash(*h.finalize().as_bytes()) != want
                {
                    return Err(std::io::Error::other(Error::Corrupt(format!("reassembled hash mismatch for {want}"))));
                }
                return Ok(0);
            };
            let loc = self
                .store
                .lookup(&ch)
                .map_err(std::io::Error::other)?
                .ok_or_else(|| std::io::Error::other(Error::Corrupt(format!("missing chunk {ch}"))))?;
            let d = self.store.read_blob(&ch, loc).map_err(std::io::Error::other)?;
            if let Some((h, _)) = self.verify.as_mut() {
                h.update(&d);
            }
            let slice = d
                .get(a as usize..b as usize)
                .ok_or_else(|| std::io::Error::other(Error::Corrupt("chunk shorter than manifest".into())))?;
            self.cur = std::io::Cursor::new(slice.to_vec());
        }
    }
}
