//! `LocalRepo`: a pico2peta repository on local disk, optionally with
//! sealed packs on any other [`Backend`] (for example S3), implementing
//! [`Repository`].
//!
//! # Layout
//! ```text
//! <root>/config.toml    format version + pack tunables (written once at init)
//! <root>/staging/       per-writer open packs (always local)
//! <root>/objects/       LocalDisk backend root (when the backend is local)
//! <root>/refs/journal   ref journal (or `refs/` on the backend with `[refs] store = "backend"`)
//! ```
//! Any number of `LocalRepo` handles, in any number of processes, can use one
//! root at the same time. See `pico2peta-pack` and `pico2peta-refs` for the
//! concurrency protocols.
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod fsck;
pub mod gc;

pub use fsck::FsckReport;
pub use gc::{GcOptions, GcReport, GroupKey, Grouper, OpaqueObjects, Tracer};
use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Observed, RefValue, ReplaceOutcome, Repository, Result, Rule};
pub use pico2peta_pack::ChunkerKind;
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
    /// Where refs are stored (added after format 1; defaults to local).
    #[serde(default)]
    pub refs: RefsSection,
    /// Which chunker large objects use, per name prefix (optional; defaults to
    /// FastCDC everywhere).
    #[serde(default, skip_serializing_if = "ChunkingSection::is_default")]
    pub chunking: ChunkingSection,
}

/// `[chunking]` section: the content-defined chunker per namespace.
///
/// ```toml
/// [chunking]
/// default = "fastcdc"
/// [chunking.prefixes]
/// "models/" = "xet"
/// ```
///
/// The longest matching prefix wins. It applies to `put_stream_for` (the CLI's
/// `put --name`); plain `put_stream` uses `default`. Changing it later is safe:
/// it only affects new writes, and every object reads back the same way
/// whatever chunker wrote it. Content written with different chunkers doesn't
/// share chunks, so keep one chunker per kind of data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkingSection {
    /// Chunker for names no prefix matches (and for unnamed puts).
    #[serde(default)]
    pub default: ChunkerChoice,
    /// Name prefix → chunker.
    #[serde(default)]
    pub prefixes: std::collections::BTreeMap<String, ChunkerChoice>,
}

impl ChunkingSection {
    fn is_default(&self) -> bool {
        *self == ChunkingSection::default()
    }

    /// The chunker for `name` (longest matching prefix, else `default`).
    pub fn for_name(&self, name: &str) -> ChunkerChoice {
        self.prefixes
            .iter()
            .filter(|(p, _)| name.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map_or(self.default, |(_, c)| *c)
    }
}

/// A chunker name in `[chunking]` (see [`ChunkerKind`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkerChoice {
    /// FastCDC v2020 with the `[pack]` cdc sizes.
    #[default]
    Fastcdc,
    /// Byte-exact Hugging Face Xet chunking (8/64/128 KiB).
    Xet,
}

impl From<ChunkerChoice> for ChunkerKind {
    fn from(c: ChunkerChoice) -> Self {
        match c {
            ChunkerChoice::Fastcdc => ChunkerKind::FastCdc,
            ChunkerChoice::Xet => ChunkerKind::Xet,
        }
    }
}

/// `[refs]` section of the config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefsSection {
    /// `local` (default): a journal under `<root>/refs`, with all writers on one host.
    /// `backend`: a create-only log under `refs/` on the pack backend (e.g. S3),
    /// so several hosts may write the repository.
    #[serde(default)]
    pub store: RefsStore,
}

/// Ref storage engine (see [`RefsSection`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefsStore {
    /// Local journal (flock commit point).
    #[default]
    Local,
    /// Create-only log on the backend (conditional-create commit point).
    Backend,
}

/// `[pack]` section of the config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackSection {
    /// See [`PackConfig::chunk_threshold`].
    pub chunk_threshold: usize,
    /// See [`PackConfig::cdc_min`].
    pub cdc_min: u32,
    /// See [`PackConfig::cdc_avg`].
    pub cdc_avg: u32,
    /// See [`PackConfig::cdc_max`].
    pub cdc_max: u32,
    /// See [`PackConfig::pack_cap`].
    pub pack_cap: u64,
    /// See [`PackConfig::zstd_level`].
    pub zstd_level: i32,
    /// Added after format 1 shipped; defaults keep old configs readable.
    #[serde(default = "default_fanout")]
    pub manifest_fanout: usize,
    /// See [`PackConfig::compress_probe`] (optional; default on).
    #[serde(default = "default_true")]
    pub compress_probe: bool,
}

fn default_true() -> bool {
    true
}

fn default_fanout() -> usize {
    pico2peta_pack::store::DEFAULT_MANIFEST_FANOUT
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
            manifest_fanout: c.manifest_fanout,
            compress_probe: c.compress_probe,
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
            manifest_fanout: c.manifest_fanout,
            compress_probe: c.compress_probe,
        }
    }
}

/// A local repository handle. Cheap to clone, thread-safe.
#[derive(Clone)]
pub struct LocalRepo {
    root: PathBuf,
    chunking: ChunkingSection,
    packs: PackStore,
    refs: Arc<RefStore>,
    backend: Arc<dyn Backend>,
    faults: pico2peta_core::fault::FaultHandle,
}

impl LocalRepo {
    /// Creates a new repository at `root` (which may exist but must not
    /// contain a repository).
    pub fn init(root: impl AsRef<Path>, pack: &PackConfig) -> Result<Self> {
        Self::init_with(root, pack, RefsStore::Local)
    }

    /// Like [`LocalRepo::init`], choosing the ref store.
    pub fn init_with(root: impl AsRef<Path>, pack: &PackConfig, refs: RefsStore) -> Result<Self> {
        Self::init_config(
            root,
            &RepoConfig {
                format: FORMAT_VERSION,
                pack: pack.into(),
                refs: RefsSection { store: refs },
                chunking: ChunkingSection::default(),
            },
        )
    }

    /// Creates a repository from a full config (validated first).
    pub fn init_config(root: impl AsRef<Path>, cfg: &RepoConfig) -> Result<Self> {
        let pack = PackConfig::from(&cfg.pack);
        pack.validate()?;
        Self::validate_chunking(&cfg.chunking, &pack)?;
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        let text = toml::to_string(cfg).map_err(|e| Error::Parse(e.to_string()))?;
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
        Self::open_with_faults(root, backend, pico2peta_core::fault::Faults::disarmed())
    }

    /// Like [`LocalRepo::open_with_backend`], with fault injection shared by
    /// the pack store, the ref store and GC (crash-consistency tests).
    pub fn open_with_faults(
        root: impl AsRef<Path>,
        backend: Arc<dyn Backend>,
        faults: pico2peta_core::fault::FaultHandle,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let cfg = Self::read_config(&root)?;
        let packs =
            PackStore::open_with_faults(root.join("staging"), backend.clone(), (&cfg.pack).into(), faults.clone())?;
        let refs = Arc::new(match cfg.refs.store {
            RefsStore::Local => RefStore::open_with_faults(root.join("refs"), faults.clone())?,
            RefsStore::Backend => RefStore::open_backend_with_faults(backend.clone(), "refs/", faults.clone())?,
        });
        Ok(LocalRepo { root, chunking: cfg.chunking, packs, refs, backend, faults })
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
        let pack = PackConfig::from(&cfg.pack);
        pack.validate()?;
        Self::validate_chunking(&cfg.chunking, &pack)?;
        Ok(cfg)
    }

    fn validate_chunking(c: &ChunkingSection, pack: &PackConfig) -> Result<()> {
        let xet = c.default == ChunkerChoice::Xet || c.prefixes.values().any(|v| *v == ChunkerChoice::Xet);
        if xet && pack.chunk_threshold <= pico2peta_pack::XET_MAX {
            return Err(Error::Parse(format!(
                "[chunking] uses xet, which needs pack.chunk_threshold > {} (have {})",
                pico2peta_pack::XET_MAX,
                pack.chunk_threshold
            )));
        }
        Ok(())
    }

    /// The `[chunking]` config in effect.
    pub fn chunking(&self) -> &ChunkingSection {
        &self.chunking
    }

    /// `put_stream` with the chunker configured for `name`'s namespace (see
    /// [`ChunkingSection`]). Doesn't create the name; it only decides how the
    /// object's chunks are cut.
    pub fn put_stream_for(&self, name: &Name, src: &mut dyn Read) -> Result<Hash> {
        self.packs.put_stream_with(self.chunking.for_name(name.as_str()).into(), src)
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

    /// Fault point (no-op unless armed by `open_with_faults`).
    pub(crate) fn fault(&self, point: &'static str) -> Result<()> {
        Ok(self.faults.hit(point)?)
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
        self.packs.put_stream_with(self.chunking.default.into(), src)
    }
    fn put_delta(&self, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
        self.packs.put_delta(base, src)
    }
    fn put_delta_mapped(&self, base: &Hash, segments: &[(u64, u64, u64)], src: &mut dyn Read) -> Result<Hash> {
        self.packs.put_delta_mapped(base, segments, src)
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
    fn ensure_live(&self, hash: &Hash) -> Result<bool> {
        self.packs.ensure_live(hash)
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
        // Make sure the target is in live (non-condemned) storage, then publish
        // our objects, so a ref never points at an object other handles can't
        // see or that GC is about to delete.
        let live = self.packs.ensure_live(&new)?;
        self.packs.flush()?;
        self.refs.replace(name, read_set, new, &|_| Ok(live))
    }

    fn replace_many(
        &self,
        updates: &[(Name, Hash)],
        read_set: &[(Name, Observed)],
    ) -> Result<pico2peta_core::ReplaceManyOutcome> {
        let mut missing = None;
        for (_, h) in updates {
            if !self.packs.ensure_live(h)? {
                missing = Some(*h);
            }
        }
        self.packs.flush()?;
        self.refs.replace_many(updates, read_set, &|h| Ok(missing != Some(*h)))
    }

    fn update_many(
        &self,
        updates: &[(Name, Option<Hash>)],
        read_set: &[(Name, Observed)],
    ) -> Result<pico2peta_core::ReplaceManyOutcome> {
        let mut missing = Vec::new();
        for h in updates.iter().filter_map(|(_, h)| h.as_ref()) {
            if !self.packs.ensure_live(h)? {
                missing.push(*h);
            }
        }
        self.packs.flush()?;
        self.refs.update_many(updates, read_set, &|h| Ok(!missing.contains(h)))
    }

    fn delete_ref(&self, name: &Name, read_set: &[(Name, Observed)]) -> Result<ReplaceOutcome> {
        self.refs.delete(name, read_set)
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

#[cfg(test)]
mod config_compat {
    #[test]
    fn format1_config_without_fanout_still_loads() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("config.toml"),
            "format = 1\n[pack]\nchunk_threshold = 1048576\ncdc_min = 16384\ncdc_avg = 65536\ncdc_max = 262144\npack_cap = 268435456\nzstd_level = 3\n",
        )
        .unwrap();
        let c = super::LocalRepo::read_config(d.path()).unwrap();
        assert_eq!(c.pack.manifest_fanout, pico2peta_pack::store::DEFAULT_MANIFEST_FANOUT);
        assert!(c.pack.compress_probe);
        assert_eq!(c.chunking, super::ChunkingSection::default());
    }

    /// `[chunking]`: longest prefix wins, the config round-trips through
    /// config.toml, `put_stream_for` cuts with the namespace's chunker, and
    /// Xet with a chunk threshold at or below its maximum chunk is refused.
    #[test]
    fn per_namespace_chunking() {
        use super::*;
        let mut chunking = ChunkingSection::default();
        chunking.prefixes.insert("models/".into(), ChunkerChoice::Xet);
        chunking.prefixes.insert("models/legacy/".into(), ChunkerChoice::Fastcdc);
        assert_eq!(chunking.for_name("models/llama/main"), ChunkerChoice::Xet);
        assert_eq!(chunking.for_name("models/legacy/x"), ChunkerChoice::Fastcdc);
        assert_eq!(chunking.for_name("src/heads/main"), ChunkerChoice::Fastcdc);
        let cfg = |pack: &PackConfig, chunking: &ChunkingSection| RepoConfig {
            format: FORMAT_VERSION,
            pack: pack.into(),
            refs: RefsSection::default(),
            chunking: chunking.clone(),
        };
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init_config(d.path(), &cfg(&PackConfig::default(), &chunking)).unwrap();
        drop(r);
        let r = LocalRepo::open(d.path()).unwrap();
        assert_eq!(r.chunking(), &chunking);
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let data: Vec<u8> = (0..3_000_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let h = r.put_stream_for(&Name::new("models/llama/main").unwrap(), &mut &data[..]).unwrap();
        assert_eq!(h, Hash::of(&data));
        assert_eq!(r.get(&h, None).unwrap(), data);
        let lens = pico2peta_chunk::chunk_lengths(&mut pico2peta_chunk::XetChunker::new(), &data);
        let mut off = 0;
        for l in lens {
            assert!(r.contains(&Hash::of(&data[off..off + l])).unwrap());
            off += l;
        }
        // Xet needs chunk_threshold > 128 KiB.
        let d2 = tempfile::tempdir().unwrap();
        assert!(matches!(
            LocalRepo::init_config(d2.path(), &cfg(&PackConfig::tiny(), &chunking)),
            Err(Error::Parse(_))
        ));
    }
}
