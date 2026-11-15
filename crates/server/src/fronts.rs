//! The server's domain fronts on the core Front API (`pico2peta_repo::front`).
//!
//! | front | claims | GC tracer (snapshot per run) | repack grouping |
//! |---|---|---|---|
//! | `git` | `git/` (Git repos, the HF front's repos, `git/_oidmap`, `git/_lfs*`, `git/_xet`) | [`GitTracer`] | [`GitGrouper`] |
//! | `oci` | `oci/` (OCI repos, `oci/_digest`) | [`OciTracer`] (also every segment map) | none |
//! | `pypi` | `pypi/` (index files) | record → content | none |
//! | `safetensors` | none (built-in, `crates/weights`) | none | none |
//!
//! The HF front writes through the `git` front: an HF repo *is* a Git repo, so it has no
//! claim of its own. Fronts are always registered, even with an endpoint disabled, so GC
//! always understands the data.
//!
//! Each front holds a `Weak` reference to its own handle ([`LocalRepo::front`]). The
//! [`Fronts`] value owns the strong ones: a strong reference would form a cycle
//! (repo → registry → front → handle → repo).

use crate::git::{GitGrouper, GitTracer};
use crate::oci::OciTracer;
use pico2peta_core::{Error, Hash, ObjectStore, Repository, Result};
use pico2peta_repo::LocalRepo;
use pico2peta_repo::front::{Front, FrontRegistry};
use pico2peta_repo::gc::{GroupKey, Grouper, Tracer};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

type Handle = OnceLock<Weak<dyn Repository>>;

fn upgrade(h: &Handle, id: &str) -> Result<Arc<dyn Repository>> {
    h.get()
        .and_then(Weak::upgrade)
        .ok_or_else(|| Error::Corrupt(format!("front {id:?} is not attached to a repository")))
}

/// Git object graph (and LFS pointers, Xet records) under `git/`.
#[derive(Default)]
pub struct GitFront {
    handle: Handle,
}

struct GitSnapshot(GitTracer);

impl Tracer for GitSnapshot {
    fn children(&self, h: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        Ok(self.0.children_of(h, store)?.unwrap_or_default())
    }
}

impl Front for GitFront {
    fn id(&self) -> &str {
        "git"
    }
    fn prefixes(&self) -> Vec<String> {
        vec!["git/".into()]
    }
    fn gc_tracer(&self, _repo: &dyn Repository) -> Result<Option<Box<dyn Tracer>>> {
        Ok(Some(Box::new(GitSnapshot(GitTracer::new(upgrade(&self.handle, "git")?)?))))
    }
    fn group(&self, repo: &dyn Repository, live: &HashSet<Hash>) -> Result<HashMap<Hash, GroupKey>> {
        GitGrouper.assign(repo, live)
    }
}

/// OCI manifests and indexes under `oci/`. Its tracer also follows every segment map.
#[derive(Default)]
pub struct OciFront {
    handle: Handle,
}

struct OciSnapshot(OciTracer);

impl Tracer for OciSnapshot {
    fn children(&self, h: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        Ok(self.0.children_of(h, store)?.unwrap_or_default())
    }
}

impl Front for OciFront {
    fn id(&self) -> &str {
        "oci"
    }
    fn prefixes(&self) -> Vec<String> {
        vec!["oci/".into()]
    }
    fn gc_tracer(&self, _repo: &dyn Repository) -> Result<Option<Box<dyn Tracer>>> {
        Ok(Some(Box::new(OciSnapshot(OciTracer::new(upgrade(&self.handle, "oci")?)))))
    }
}

/// PyPI index files under `pypi/`: record → content.
#[derive(Default)]
pub struct PypiFront;

impl Front for PypiFront {
    fn id(&self) -> &str {
        "pypi"
    }
    fn prefixes(&self) -> Vec<String> {
        vec!["pypi/".into()]
    }
    fn children(&self, h: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        Ok(crate::pypi::record_child(h, store)?.into_iter().collect())
    }
}

/// Provenance records are a core concept (`_prov/`), not a front; GC passes them as the
/// extra tracer. Everything it doesn't recognize is a leaf.
pub struct Leaves;

impl Tracer for Leaves {
    fn children(&self, _: &Hash, _: &dyn ObjectStore) -> Result<Vec<Hash>> {
        Ok(Vec::new())
    }
}

/// The repository opened with the server's fronts, plus each front's write handle.
pub struct Fronts {
    pub repo: LocalRepo,
    handles: HashMap<String, Arc<dyn Repository>>,
}

impl Fronts {
    pub fn open(repo: LocalRepo) -> Result<Fronts> {
        let (git, oci) = (Arc::new(GitFront::default()), Arc::new(OciFront::default()));
        let reg = FrontRegistry::new()
            .with(git.clone())?
            .with(oci.clone())?
            .with(Arc::new(PypiFront))?
            .with(Arc::new(pico2peta_weights::SafetensorsFront))?;
        let repo = repo.with_fronts(reg)?;
        let mut handles: HashMap<String, Arc<dyn Repository>> = HashMap::new();
        for (id, slot) in [("git", &git.handle), ("oci", &oci.handle)] {
            let h: Arc<dyn Repository> = Arc::new(repo.front(id)?);
            let _ = slot.set(Arc::downgrade(&h));
            handles.insert(id.to_string(), h);
        }
        handles.insert("pypi".into(), Arc::new(repo.front("pypi")?));
        Ok(Fronts { repo, handles })
    }

    /// Write handle of front `id` (`git`, `oci`).
    pub fn handle(&self, id: &str) -> Result<Arc<dyn Repository>> {
        self.handles.get(id).cloned().ok_or_else(|| Error::Parse(format!("front {id:?} has no handle")))
    }

    /// The handle to move these names with: the owning front's if all owned names share
    /// one owner, else the plain repository (which refuses names owned by a front).
    pub fn handle_for<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Arc<dyn Repository> {
        let mut owner: Option<&str> = None;
        for n in names {
            match (self.repo.fronts().owner(n), owner) {
                (None, _) => {}
                (Some(o), None) => owner = Some(o),
                (Some(o), Some(prev)) if o == prev => {}
                _ => return Arc::new(self.repo.clone()),
            }
        }
        owner.and_then(|o| self.handles.get(o).cloned()).unwrap_or_else(|| Arc::new(self.repo.clone()))
    }
}
