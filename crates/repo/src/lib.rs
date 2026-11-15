//! `LocalRepo`: a pico2peta repository on local disk, optionally with
//! sealed packs on any other [`Backend`] (for example S3), implementing
//! [`Repository`].
//!
//! # Layout
//! ```text
//! <root>/config.toml    format version + pack tunables (written once at init)
//! <root>/staging/       per-writer open packs (always local)
//! <root>/objects/       LocalDisk backend root (when the backend is local)
//! <root>/refs/journal   ref journal
//! ```
//! Any number of `LocalRepo` handles, in any number of processes, can use one
//! root at the same time. See `pico2peta-pack` and `pico2peta-refs` for the
//! concurrency protocols.

pub mod gc;

pub use gc::{GcOptions, GcReport, OpaqueObjects, Tracer};
use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Observed, RefValue, ReplaceOutcome, Repository, Result, Rule};
use pico2peta_pack::{PackStats, PackStore};
use pico2peta_refs::RefStore;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Current on-disk repository format.
pub const FORMAT_VERSION: u32 = 1;

/// Contents of `config.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// On-disk format version. Readers refuse unknown versions.
    pub format: u32,
    /// Pack tunables.
    pub pack: PackSection,
}

/// `[pack]` section of the config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackSection {
    pub chunk_threshold: usize,
    pub cdc_min: u32,
    pub cdc_avg: u32,
    pub cdc_max: u32,
    pub pack_cap: u64,
    pub zstd_level: i32,
}

impl From<&PackConfig> for PackSection {
    fn from(c: &PackConfig) -> Self {
        PackSection {
            chunk_threshold: c.chunk_threshold,
            cdc_min: c.cdc_min,
            cdc_avg: c.cdc_avg,
            cdc_max: c.cdc_max,
            pack_cap: c.pack_cap,
            zstd_level: c.zstd_level,
        }
    }
}

impl From<&PackSection> for PackConfig {
    fn from(c: &PackSection) -> Self {
        PackConfig {
            chunk_threshold: c.chunk_threshold,
            cdc_min: c.cdc_min,
            cdc_avg: c.cdc_avg,
            cdc_max: c.cdc_max,
            pack_cap: c.pack_cap,
            zstd_level: c.zstd_level,
        }
    }
}

/// A local repository handle. Cheap to clone, thread-safe.
#[derive(Clone)]
pub struct LocalRepo {
    root: PathBuf,
    packs: PackStore,
    refs: Arc<RefStore>,
    backend: Arc<dyn Backend>,
}

impl LocalRepo {
    /// Creates a new repository at `root` (which may exist but must not
    /// contain a repository).
    pub fn init(root: impl AsRef<Path>, pack: &PackConfig) -> Result<Self> {
        pack.validate()?;
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        let cfg = RepoConfig { format: FORMAT_VERSION, pack: pack.into() };
        let text = toml::to_string(&cfg).map_err(|e| Error::Parse(e.to_string()))?;
        let tmp = root.join(".config.toml.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        // Create-only publish: hard_link fails if config.toml exists.
        let res = std::fs::hard_link(&tmp, root.join("config.toml"));
        let _ = std::fs::remove_file(&tmp);
        match res {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Parse(format!("{} already contains a repository", root.display())));
            }
            Err(e) => return Err(e.into()),
        }
        pico2peta_core::backend::fsync_dir(root)?;
        Self::open(root)
    }

    /// Opens an existing repository with its local backend.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let backend: Arc<dyn Backend> = Arc::new(LocalDisk::new(root.join("objects"))?);
        Self::open_with_backend(root, backend)
    }

    /// Opens an existing repository whose sealed packs live on `backend`.
    /// Staging and refs stay under `root`.
    pub fn open_with_backend(root: impl AsRef<Path>, backend: Arc<dyn Backend>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let cfg = Self::read_config(&root)?;
        let packs = PackStore::open(root.join("staging"), backend.clone(), (&cfg.pack).into())?;
        let refs = Arc::new(RefStore::open(root.join("refs"))?);
        Ok(LocalRepo { root, packs, refs, backend })
    }

    /// Reads and validates `config.toml`.
    pub fn read_config(root: &Path) -> Result<RepoConfig> {
        let p = root.join("config.toml");
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Parse(format!("{} is not a pico2peta repository (no config.toml)", root.display())));
            }
            Err(e) => return Err(e.into()),
        };
        let cfg: RepoConfig = toml::from_str(&text).map_err(|e| Error::Parse(format!("{}: {e}", p.display())))?;
        if cfg.format != FORMAT_VERSION {
            return Err(Error::Unsupported("unknown repository format version"));
        }
        PackConfig::from(&cfg.pack).validate()?;
        Ok(cfg)
    }

    /// Repository root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The backend sealed packs are stored on.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    /// Pack store statistics.
    pub fn pack_stats(&self) -> Result<PackStats> {
        self.packs.refresh()?;
        self.packs.stats()
    }

    /// Direct access to the pack store (GC, repack, benchmarks).
    pub fn packs(&self) -> &PackStore {
        &self.packs
    }
}

impl ObjectStore for LocalRepo {
    fn put(&self, data: &[u8]) -> Result<Hash> {
        self.packs.put(data)
    }
    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash> {
        self.packs.put_stream(src)
    }
    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>> {
        self.packs.get(hash, range)
    }
    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>> {
        self.packs.open(hash, range)
    }
    fn size(&self, hash: &Hash) -> Result<u64> {
        self.packs.size(hash)
    }
    fn contains(&self, hash: &Hash) -> Result<bool> {
        self.packs.contains(hash)
    }
    fn flush(&self) -> Result<()> {
        self.packs.flush()
    }
}

impl Repository for LocalRepo {
    fn resolve(&self, name: &Name) -> Result<Option<RefValue>> {
        self.refs.resolve(name)
    }

    fn replace(&self, name: &Name, read_set: &[(Name, Observed)], new: Hash) -> Result<ReplaceOutcome> {
        // Publish our objects first, so a ref never points at an object that
        // other handles can't see.
        self.packs.flush()?;
        let packs = &self.packs;
        self.refs.replace(name, read_set, new, &|h| packs.contains(h))
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(Name, RefValue)>> {
        self.refs.list(prefix)
    }

    fn set_rule(&self, prefix: &str, rule: Rule) -> Result<()> {
        self.refs.set_rule(prefix, rule)
    }

    fn rules(&self) -> Result<Vec<(String, Rule)>> {
        self.refs.rules()
    }
}

pub use pico2peta_pack::PackConfig;

/// Convenience for applying optional CLI or config overrides to a [`PackConfig`].
pub trait PackConfigExt {
    /// Replaces the fields that are `Some`.
    fn apply_overrides(&mut self, chunk_threshold: Option<usize>, pack_cap: Option<u64>, zstd_level: Option<i32>);
}

impl PackConfigExt for PackConfig {
    fn apply_overrides(&mut self, chunk_threshold: Option<usize>, pack_cap: Option<u64>, zstd_level: Option<i32>) {
        if let Some(v) = chunk_threshold {
            self.chunk_threshold = v;
            // Keep chunks strictly below the threshold: scale CDC sizes down if needed.
            if self.cdc_max as usize >= v {
                let max = ((v / 2) as u32).max(1024);
                self.cdc_max = max;
                self.cdc_avg = (max / 4).max(256);
                self.cdc_min = (max / 16).max(64);
            }
        }
        if let Some(v) = pack_cap {
            self.pack_cap = v;
        }
        if let Some(v) = zstd_level {
            self.zstd_level = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_open_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
        assert!(LocalRepo::init(d.path(), &PackConfig::tiny()).is_err(), "double init must fail");
        let h = r.put(b"hello").unwrap();
        let main = Name::new("main").unwrap();
        assert_eq!(
            r.replace(&main, &[(main.clone(), Observed::Absent)], h).unwrap(),
            ReplaceOutcome::Ok { version: 1 }
        );
        // Another handle sees both the object and the ref.
        let r2 = LocalRepo::open(d.path()).unwrap();
        let v = r2.resolve(&main).unwrap().unwrap();
        assert_eq!(r2.get(&v.hash, None).unwrap(), b"hello");
        assert!(matches!(
            r.replace(&main, &[(main.clone(), Observed::Version(1))], Hash::of(b"absent")),
            Err(Error::MissingObject(_))
        ));
    }

    #[test]
    fn not_a_repo() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(LocalRepo::open(d.path()), Err(Error::Parse(_))));
        std::fs::write(
            d.path().join("config.toml"),
            "format = 99\n[pack]\nchunk_threshold=1\ncdc_min=1\ncdc_avg=1\ncdc_max=1\npack_cap=1\nzstd_level=1\n",
        )
        .unwrap();
        assert!(LocalRepo::open(d.path()).is_err());
    }
}
