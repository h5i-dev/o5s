//! The Front API: how domain-specific "fronts" (Git, OCI, Hugging Face,
//! package indexes, model weights, ...) plug into the domain-agnostic core.
//!
//! The core stores objects, moves names and runs GC the same way for every
//! domain. A front contributes only what is domain-specific:
//! - an **encoding plan** per put ([`Front::plan`]): chunker, byte grouping,
//!   and whether to store the object as a derived `base + patch` object;
//! - **owned name prefixes** ([`Front::prefixes`]): names under them can only
//!   be moved through that front's handle ([`LocalRepo::front`]). Claims nest,
//!   and the longest claimed prefix decides (e.g. `git/` and `git/hf/`);
//! - a **GC tracer** and a repack **grouper** for the objects it understands.
//!
//! Which front and settings apply to a name is configuration: a **profile**
//! (`[profiles.<p>]` in config.toml) assigned to name prefixes
//! (`[namespaces]`). Fronts never affect identity or reads: every object is
//! the BLAKE3 of the bytes `get` returns, however it was encoded.

use crate::gc::{GroupKey, Grouper, Tracer};
use pico2peta_core::{Encoding, Error, Hash, Name, ObjectStore, Repository, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;

/// What a front sees when planning one put.
pub struct PlanCtx<'a> {
    /// The name the object is being stored for, if any.
    pub name: Option<&'a Name>,
    /// The object the name currently points at, when the profile asks to
    /// derive new versions from the previous one (`derive = "previous"`).
    pub previous: Option<Hash>,
    /// The encoding the profile alone would use (chunker, plus an
    /// offset-aligned derive from `previous` if requested).
    pub default: Encoding,
    /// The store (to read the previous version's layout, for example).
    pub store: &'a dyn ObjectStore,
}

/// A domain front. Implementations live outside the core crates.
///
/// A front that keeps its own [`crate::FrontHandle`] must hold it weakly (or
/// outside the front): the repository holds the registry, the registry holds
/// the front, and a strong handle inside the front would hold the repository,
/// an `Arc` cycle that leaks the `LocalRepo`.
pub trait Front: Send + Sync {
    /// Stable id, used in config (`front = "<id>"`) and for handles.
    fn id(&self) -> &str;

    /// Name prefixes this front owns. Names under an owned prefix can only be
    /// moved through [`LocalRepo::front`]`(id)`. Longest claim wins.
    fn prefixes(&self) -> Vec<String> {
        Vec::new()
    }

    /// Plans the encoding of one put. The front may read a prefix of `src`
    /// (e.g. a file header) and returns the bytes it consumed, which the core
    /// puts back in front of the rest of the stream. The default keeps
    /// `ctx.default` and reads nothing.
    fn plan(&self, ctx: &PlanCtx<'_>, src: &mut dyn Read) -> Result<(Vec<u8>, Encoding)> {
        let _ = src;
        Ok((Vec::new(), ctx.default.clone()))
    }

    /// A tracer for one GC run, built once at its start (e.g. over a snapshot
    /// of the front's id-index roots taken now, which is what makes it safe
    /// with short grace periods). `None` (the default) = use
    /// [`Front::children`].
    fn gc_tracer(&self, repo: &dyn Repository) -> Result<Option<Box<dyn Tracer>>> {
        let _ = repo;
        Ok(None)
    }

    /// Children of `hash` this front knows about (GC). Every front sees every
    /// object; the registry takes the union.
    fn children(&self, hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        let _ = (hash, store);
        Ok(Vec::new())
    }

    /// Repack grouping keys for (some of) `live` (see [`Grouper`]).
    fn group(&self, repo: &dyn Repository, live: &HashSet<Hash>) -> Result<HashMap<Hash, GroupKey>> {
        let _ = (repo, live);
        Ok(HashMap::new())
    }
}

/// The fronts a repository is opened with (built-ins are registered by the
/// binary: CLI, server).
#[derive(Clone, Default)]
pub struct FrontRegistry {
    fronts: Vec<Arc<dyn Front>>,
}

impl FrontRegistry {
    /// An empty registry (no fronts, no claimed prefixes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a front. Ids must be unique, and two fronts can't claim the same
    /// prefix (nested claims are fine).
    pub fn register(&mut self, f: Arc<dyn Front>) -> Result<()> {
        if self.get(f.id()).is_some() {
            return Err(Error::Parse(format!("front {:?} registered twice", f.id())));
        }
        for p in f.prefixes() {
            if let Some(other) = self.fronts.iter().find(|o| o.prefixes().contains(&p)) {
                return Err(Error::Parse(format!("prefix {p:?} claimed by both {:?} and {:?}", other.id(), f.id())));
            }
        }
        self.fronts.push(f);
        Ok(())
    }

    /// Builder form of [`FrontRegistry::register`].
    pub fn with(mut self, f: Arc<dyn Front>) -> Result<Self> {
        self.register(f)?;
        Ok(self)
    }

    /// The front with this id.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Front>> {
        self.fronts.iter().find(|f| f.id() == id)
    }

    /// All registered fronts.
    pub fn fronts(&self) -> &[Arc<dyn Front>] {
        &self.fronts
    }

    /// The front owning `name` (longest claimed prefix), if any.
    pub fn owner(&self, name: &str) -> Option<&str> {
        self.fronts
            .iter()
            .flat_map(|f| f.prefixes().into_iter().map(move |p| (p, f.id())))
            .filter(|(p, _)| name.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, id)| id)
    }

    /// A GC tracer over every front (union of their children). Fronts with
    /// a [`Front::gc_tracer`] contribute that instead, built now, so call this
    /// once per GC run.
    pub fn tracer(&self, repo: &dyn Repository) -> Result<FrontsTracer> {
        let mut parts: Vec<Part> = Vec::new();
        for f in &self.fronts {
            parts.push(match f.gc_tracer(repo)? {
                Some(t) => Part::Snapshot(t),
                None => Part::Front(f.clone()),
            });
        }
        Ok(FrontsTracer { parts })
    }

    /// A repack grouper over every front (the first front to key an object wins,
    /// in registration order).
    pub fn grouper(&self) -> FrontsGrouper {
        FrontsGrouper { fronts: self.fronts.clone() }
    }
}

/// [`Tracer`] that asks every front and takes the union.
pub struct FrontsTracer {
    parts: Vec<Part>,
}

enum Part {
    Front(Arc<dyn Front>),
    Snapshot(Box<dyn Tracer>),
}

impl Tracer for FrontsTracer {
    fn children(&self, hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        let mut out: Vec<Hash> = Vec::new();
        for p in &self.parts {
            let cs = match p {
                Part::Front(f) => f.children(hash, store)?,
                Part::Snapshot(t) => t.children(hash, store)?,
            };
            for c in cs {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        Ok(out)
    }
}

/// [`Grouper`] that merges every front's keys.
pub struct FrontsGrouper {
    fronts: Vec<Arc<dyn Front>>,
}

impl Grouper for FrontsGrouper {
    fn assign(&self, repo: &dyn Repository, live: &HashSet<Hash>) -> Result<HashMap<Hash, GroupKey>> {
        let mut out = HashMap::new();
        for f in &self.fronts {
            for (h, k) in f.group(repo, live)? {
                out.entry(h).or_insert(k);
            }
        }
        Ok(out)
    }
}

/// How new versions of a name are stored relative to the previous one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DerivePolicy {
    /// Store each object on its own.
    #[default]
    None,
    /// Offer the name's current object as the base (`base + patch`); the
    /// front (if any) decides the alignment.
    Previous,
}

/// A domain profile: `[profiles.<name>]` in config.toml.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Front that plans puts (by id). `None` = the profile's settings alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front: Option<String>,
    /// Chunker id (`"fastcdc"`, `"xet"`). `None` = the store default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunker: Option<String>,
    /// Derived-object policy for new versions.
    #[serde(default, skip_serializing_if = "is_default")]
    pub derive: DerivePolicy,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl Profile {
    /// The encoding this profile implies on its own.
    pub fn encoding(&self, previous: Option<Hash>) -> Encoding {
        Encoding {
            chunker: self.chunker.clone(),
            group_width: None,
            derive: match (self.derive, previous) {
                (DerivePolicy::Previous, Some(base)) => {
                    Some(pico2peta_core::Derive { base, patch: pico2peta_core::PatchFormat::Xor, segments: Vec::new() })
                }
                _ => None,
            },
        }
    }
}
