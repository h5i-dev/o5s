//! The `Backend` trait: where sealed, immutable blobs (packs, indexes) live.
//!
//! # Contract (API v1 — stable; changes are logged in docs/PROGRESS.md)
//!
//! A backend is an S3-shaped store of **immutable** blobs addressed by string keys:
//!
//! * **Keys** are `/`-separated, non-empty segments of `[A-Za-z0-9._-]`
//!   (see [`validate_key`]). Core only uses `packs/<hex>.pack`,
//!   `packs/<hex>.idx` and (future) `meta/...` keys.
//! * **Writes are create-only.** [`Backend::put_if_absent`] must be atomic: a
//!   concurrent reader sees either nothing or the complete blob, never a
//!   prefix. If the key already exists the backend must not overwrite it and
//!   returns [`PutOutcome::AlreadyExists`]. On S3 this is `PUT` with
//!   `If-None-Match: *` (multipart: on `CompleteMultipartUpload`). Core keys
//!   are content-derived, so `AlreadyExists` is a success for core.
//! * **Durability:** once `put_if_absent` returns `Ok`, the blob survives
//!   crashes (local disk: file + directory fsync; S3: 200 response).
//! * **Reads** are ranged and may be served in parallel from many threads.
//!   Reading past the end is an error ([`Error::Corrupt`]), and a missing key is
//!   [`Error::KeyNotFound`].
//! * **Listing** may be eventually consistent with respect to concurrent
//!   writers. Core treats a listed `.idx` as authoritative and re-lists
//!   when it misses an object.
//! * **Delete** is only used by GC (planned) after a grace period. Deleting
//!   a missing key is `Ok`.
//! * **Errors:** retryable failures (throttling, 5xx, timeouts) must be
//!   retried *inside* the backend with backoff. Anything that escapes is
//!   final; use [`Error::Backend`] with `transient = true` if the caller might
//!   retry later at a higher level.
//!
//! The trait is synchronous and `Send + Sync`. Async implementations (for
//! example the S3 backend) run their own runtime internally, and async
//! callers (the server) call through `spawn_blocking`.

use crate::{Error, Result};
use std::io::Read;
use std::ops::Range;
use std::path::Path;

/// The body of a `put_if_absent`. Both forms can be re-read, so a backend
/// can retry or do multipart uploads without buffering everything.
#[derive(Clone, Copy, Debug)]
pub enum PutSource<'a> {
    /// An in-memory buffer.
    Bytes(&'a [u8]),
    /// A local file, complete and fsynced. It must not change during the
    /// call or afterwards. Backends may publish it by reference (e.g. a hard
    /// link) instead of copying it.
    File(&'a Path),
}

impl PutSource<'_> {
    /// Length in bytes.
    pub fn len(&self) -> Result<u64> {
        match self {
            PutSource::Bytes(b) => Ok(b.len() as u64),
            PutSource::File(p) => Ok(std::fs::metadata(p)?.len()),
        }
    }
    /// True if the body has zero length.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
    /// Opens a fresh reader over the whole body.
    pub fn reader(&self) -> Result<Box<dyn Read + Send + '_>> {
        match self {
            PutSource::Bytes(b) => Ok(Box::new(*b)),
            PutSource::File(p) => Ok(Box::new(std::fs::File::open(p)?)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Result of [`Backend::put_if_absent`].
pub enum PutOutcome {
    /// The blob was created by this call.
    Created,
    /// The key already existed; nothing was written.
    AlreadyExists,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A listed or inspected blob.
pub struct BlobMeta {
    /// Its key.
    pub key: String,
    /// Its length in bytes.
    pub len: u64,
}

/// Immutable blob storage for sealed packs and indexes. See the module docs for the full contract.
pub trait Backend: Send + Sync {
    /// Atomically creates `key` with `src`'s bytes unless `key` already exists.
    fn put_if_absent(&self, key: &str, src: PutSource<'_>) -> Result<PutOutcome>;

    /// Reads bytes `range` of `key`. `range.end` must not exceed the blob length.
    fn get_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>>;

    /// Streams the whole blob.
    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>>;

    /// Returns metadata, or `None` if the key doesn't exist.
    fn head(&self, key: &str) -> Result<Option<BlobMeta>>;

    /// Lists the blobs whose key starts with `prefix`, sorted by key.
    fn list(&self, prefix: &str) -> Result<Vec<BlobMeta>>;

    /// Deletes `key` (idempotent). Only GC uses this.
    fn delete(&self, key: &str) -> Result<()>;

    /// Human-readable description for logs (`file:///...`, `s3://bucket/prefix`).
    fn describe(&self) -> String;

    /// Whether `put_if_absent` is atomic against *concurrent* creators, so that
    /// exactly one wins (API v1.9). Content-named packs don't need it, but the
    /// ref log does: it's the commit point. Backends that emulate create-only
    /// with check-then-put (e.g. S3 without conditional writes) must return false.
    fn atomic_create(&self) -> bool {
        true
    }
}

/// Validates a backend key (see the module docs).
pub fn validate_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 1024
        && key.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        });
    if ok { Ok(()) } else { Err(Error::Parse(format!("invalid backend key {key:?}"))) }
}

/// A conformance suite every `Backend` must pass. Call it from your
/// implementation's tests with a fresh, empty backend instance:
///
/// ```ignore
/// pico2peta_core::backend::conformance::run_all(&my_backend);
/// ```
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod conformance {
    use super::*;

    fn read_all(b: &dyn Backend, key: &str) -> Vec<u8> {
        let mut v = Vec::new();
        b.open(key).expect("open").read_to_end(&mut v).expect("read");
        v
    }

    /// Runs every check. Panics with a descriptive message on violation.
    pub fn run_all(b: &dyn Backend) {
        create_only(b);
        ranged_reads(b);
        list_and_head(b);
        file_source(b);
        delete_idempotent(b);
        missing_keys(b);
        concurrent_creators(b);
    }

    /// `put_if_absent` never overwrites.
    pub fn create_only(b: &dyn Backend) {
        let k = "conf/create-only.bin";
        assert_eq!(b.put_if_absent(k, PutSource::Bytes(b"first")).unwrap(), PutOutcome::Created);
        assert_eq!(b.put_if_absent(k, PutSource::Bytes(b"second")).unwrap(), PutOutcome::AlreadyExists);
        assert_eq!(read_all(b, k), b"first", "put_if_absent must not overwrite");
    }

    /// Ranged reads return exact bytes and reject reads past the end.
    pub fn ranged_reads(b: &dyn Backend) {
        let k = "conf/range.bin";
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        b.put_if_absent(k, PutSource::Bytes(&data)).unwrap();
        assert_eq!(b.get_range(k, 0..10).unwrap(), &data[0..10]);
        assert_eq!(b.get_range(k, 9_990..10_000).unwrap(), &data[9_990..]);
        assert_eq!(b.get_range(k, 500..500).unwrap(), b"");
        assert!(b.get_range(k, 9_990..10_001).is_err(), "reading past end must fail");
    }

    /// `list` is prefix-filtered and sorted; `head` reports lengths.
    pub fn list_and_head(b: &dyn Backend) {
        b.put_if_absent("conf/list/a", PutSource::Bytes(b"1")).unwrap();
        b.put_if_absent("conf/list/b/c", PutSource::Bytes(b"22")).unwrap();
        b.put_if_absent("conf/listx", PutSource::Bytes(b"333")).unwrap();
        let l = b.list("conf/list/").unwrap();
        let keys: Vec<_> = l.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["conf/list/a", "conf/list/b/c"]);
        assert_eq!(l[1].len, 2);
        assert_eq!(b.head("conf/listx").unwrap().map(|m| m.len), Some(3));
        assert_eq!(b.head("conf/nope").unwrap(), None);
    }

    /// `PutSource::File` uploads a multi-MiB file intact.
    pub fn file_source(b: &dyn Backend) {
        let dir =
            std::env::temp_dir().join(format!("p2p-conf-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("src.bin");
        let data = vec![0xabu8; 3 << 20];
        std::fs::write(&p, &data).unwrap();
        assert_eq!(b.put_if_absent("conf/file.bin", PutSource::File(&p)).unwrap(), PutOutcome::Created);
        assert_eq!(read_all(b, "conf/file.bin"), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `delete` is idempotent and frees the key for re-creation.
    pub fn delete_idempotent(b: &dyn Backend) {
        b.put_if_absent("conf/del", PutSource::Bytes(b"x")).unwrap();
        b.delete("conf/del").unwrap();
        b.delete("conf/del").unwrap();
        assert_eq!(b.head("conf/del").unwrap(), None);
        assert_eq!(b.put_if_absent("conf/del", PutSource::Bytes(b"y")).unwrap(), PutOutcome::Created);
    }

    /// Missing keys are `KeyNotFound`; invalid keys are rejected.
    pub fn missing_keys(b: &dyn Backend) {
        assert!(matches!(b.get_range("conf/missing", 0..1), Err(Error::KeyNotFound(_))));
        assert!(matches!(b.open("conf/missing").err(), Some(Error::KeyNotFound(_))));
        assert!(b.put_if_absent("../escape", PutSource::Bytes(b"x")).is_err());
        assert!(b.put_if_absent("a//b", PutSource::Bytes(b"x")).is_err());
    }

    /// Many threads race to create the same key: exactly one `Created`, and
    /// the content is one writer's whole body.
    pub fn concurrent_creators(b: &dyn Backend) {
        let bodies: Vec<Vec<u8>> = (0..8u8).map(|i| vec![i; 100_000]).collect();
        let outcomes: Vec<PutOutcome> = std::thread::scope(|s| {
            let hs: Vec<_> = bodies
                .iter()
                .map(|body| s.spawn(move || b.put_if_absent("conf/race", PutSource::Bytes(body)).unwrap()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(outcomes.iter().filter(|o| **o == PutOutcome::Created).count(), 1, "{outcomes:?}");
        let got = read_all(b, "conf/race");
        assert!(bodies.contains(&got), "blob must be exactly one writer's body");
    }
}

// ---------------------------------------------------------------------------
// Local filesystem backend
// ---------------------------------------------------------------------------

/// Local filesystem backend: `key` maps to `<root>/<key>`.
///
/// `put_if_absent` writes a unique temp file, fsyncs it, then publishes it
/// with `link(2)` (which fails atomically if the target exists, giving
/// create-only semantics across processes) and fsyncs the directory.
pub struct LocalDisk {
    root: std::path::PathBuf,
}

impl LocalDisk {
    /// Opens (creating if needed) a backend rooted at `root`.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(LocalDisk { root })
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &str) -> Result<std::path::PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }
}

/// fsyncs a directory so that entries created or removed in it are durable.
pub fn fsync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn not_found_as(key: &str) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| if e.kind() == std::io::ErrorKind::NotFound { Error::KeyNotFound(key.to_string()) } else { e.into() }
}

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Backend for LocalDisk {
    fn put_if_absent(&self, key: &str, src: PutSource<'_>) -> Result<PutOutcome> {
        use std::io::Write;
        let path = self.path(key)?;
        if path.is_file() {
            return Ok(PutOutcome::AlreadyExists);
        }
        if path.exists() {
            return Err(Error::Parse(format!("key {key} names a directory")));
        }
        let dir = path.parent().ok_or_else(|| Error::Parse(format!("bad key {key}")))?.to_path_buf();
        if !dir.is_dir() {
            std::fs::create_dir_all(&dir)?;
            // New directories must be durable too: fsync every ancestor up to the root.
            let mut d = dir.as_path();
            while d.starts_with(&self.root) {
                fsync_dir(d)?;
                match d.parent() {
                    Some(p) if p != d => d = p,
                    _ => break,
                }
            }
        }
        if let PutSource::File(src_path) = src {
            // Same filesystem: publish by hard link. That's atomic and
            // create-only, with no copy. The caller guarantees the file is
            // complete, fsynced and never modified again.
            match std::fs::hard_link(src_path, &path) {
                Ok(()) => {
                    fsync_dir(&dir)?;
                    return Ok(PutOutcome::Created);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(PutOutcome::AlreadyExists),
                Err(_) => {} // e.g. EXDEV (cross-device): fall back to copying
            }
        }
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Unique even across PID namespaces sharing a volume (containers).
        let nonce = blake3::hash(format!("{}-{seq}-{:?}", std::process::id(), std::time::SystemTime::now()).as_bytes());
        let tmp = dir.join(format!(".tmp-{}-{seq}-{}", std::process::id(), &nonce.to_hex()[..12]));
        let res = (|| -> Result<PutOutcome> {
            {
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
                match src {
                    PutSource::Bytes(b) => f.write_all(b)?,
                    PutSource::File(_) => {
                        std::io::copy(&mut src.reader()?, &mut f)?;
                    }
                }
                f.sync_all()?;
            }
            match std::fs::hard_link(&tmp, &path) {
                Ok(()) => {
                    fsync_dir(&dir)?;
                    Ok(PutOutcome::Created)
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if path.is_file() {
                        Ok(PutOutcome::AlreadyExists)
                    } else {
                        Err(Error::Parse(format!("key {key} names a directory")))
                    }
                }
                Err(e) => Err(e.into()),
            }
        })();
        let _ = std::fs::remove_file(&tmp);
        res
    }

    fn get_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        let f = std::fs::File::open(self.path(key)?).map_err(not_found_as(key))?;
        if range.start > range.end {
            return Err(Error::Parse(format!("bad range {range:?}")));
        }
        // Check against the real length before allocating (a corrupt range must not abort).
        if range.end > f.metadata()?.len() {
            return Err(Error::Corrupt(format!("read of {key} at {range:?} is past its end")));
        }
        let mut buf = vec![0u8; (range.end - range.start) as usize];
        f.read_exact_at(&mut buf, range.start).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Corrupt(format!("short read of {key} at {range:?}"))
            } else {
                e.into()
            }
        })?;
        Ok(buf)
    }

    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>> {
        let f = std::fs::File::open(self.path(key)?).map_err(not_found_as(key))?;
        Ok(Box::new(std::io::BufReader::with_capacity(1 << 20, f)))
    }

    fn head(&self, key: &str) -> Result<Option<BlobMeta>> {
        match std::fs::metadata(self.path(key)?) {
            Ok(m) if m.is_file() => Ok(Some(BlobMeta { key: key.to_string(), len: m.len() })),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<BlobMeta>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for ent in rd {
                let ent = ent?;
                let p = ent.path();
                let ft = match ent.file_type() {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                if ft.is_dir() {
                    stack.push(p);
                    continue;
                }
                let Ok(rel) = p.strip_prefix(&self.root) else { continue };
                let Some(rel) = rel.to_str() else { continue };
                if rel.rsplit('/').next().is_some_and(|f| f.starts_with(".tmp-")) || !rel.starts_with(prefix) {
                    continue;
                }
                if validate_key(rel).is_ok() {
                    // A concurrently deleted file (GC, log trimming) just isn't listed.
                    match ent.metadata() {
                        Ok(m) => out.push(BlobMeta { key: rel.to_string(), len: m.len() }),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.path(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                if let Some(d) = path.parent() {
                    fsync_dir(d)?;
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        format!("file://{}", self.root.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_disk_conformance() {
        let d = std::env::temp_dir().join(format!("p2p-ld-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let b = LocalDisk::new(&d).unwrap();
        conformance::run_all(&b);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn keys() {
        assert!(validate_key("packs/ab.pack").is_ok());
        for k in ["", "/a", "a/", "a/../b", "a b", "a\\b"] {
            assert!(validate_key(k).is_err(), "{k}");
        }
    }
}
