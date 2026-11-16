//! Per-cell reachability for object-read authorization (net's authz kernel).
//!
//! A principal may read an object only if it is reachable from a name the
//! principal can read. Names are grouped into **cells**: the deepest cell
//! prefix (net passes its policy prefixes, and `""` if it grants on it) that
//! covers a name, segment-aligned as in `pico2peta_kernel::names::is_under`.
//! Readability depends only on cells, so this index keeps, per cell, the set of
//! objects reachable from the cell's names, by the same traversal as GC's mark:
//! ref targets, manifest chunks, delta bases, Frame3 references (all via
//! `PackStore::chunks`) and front children (`FrontRegistry::tracer`).
//!
//! **Freshness.** The index registers as a ref observer of its repository
//! handle: every successful `replace` / `replace_many` / `update_many` /
//! `delete_ref` on any clone of that `LocalRepo` notifies it before returning
//! (read-your-writes). A name created from nothing (version 1) adds its
//! target's closure to its cell at once. A move, a re-creation after a delete,
//! or a delete may make objects unreachable, so it marks the cell **dirty**; a
//! query on a dirty cell recomputes that cell exactly first. "Unreadable as soon
//! as its last readable name is deleted" therefore holds immediately.
//!
//! **Limits (v1).** In memory only (rebuilt at open, about one GC mark per
//! cell). Writes by other processes or hosts to the same repository are not
//! observed; the server must be the repository's only writer for the index to
//! be exact (the CLI on the server host writes through its own handle).

use crate::{LocalRepo, Tracer};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Repository, Result};
use std::collections::{HashSet, VecDeque};
use std::sync::Weak;
use std::sync::{Arc, Mutex};

/// A cell: an index into the cell list given at open or reload.
pub type CellId = u32;

/// Notified of every committed name change on a repository handle.
pub trait RefObserver: Send + Sync {
    /// `changes`: (name, new target or `None` for a delete, version after).
    fn changed(&self, repo: &LocalRepo, changes: &[(Name, Option<Hash>, u64)]);
}

/// The index (cheap to clone; clones share state).
#[derive(Clone)]
pub struct ReachIndex {
    inner: Arc<Inner>,
}

struct Inner {
    repo: LocalRepo,
    state: Mutex<State>,
}

struct State {
    cells: Vec<String>,
    members: Vec<HashSet<Hash>>,
    dirty: Vec<bool>,
}

impl ReachIndex {
    /// Builds the index over `cells` (each a name prefix; `""` covers every
    /// name) and registers it with `repo` (and every clone of it).
    pub fn open(repo: &LocalRepo, cells: Vec<String>) -> Result<ReachIndex> {
        let n = cells.len();
        let idx = ReachIndex {
            inner: Arc::new(Inner {
                repo: repo.clone(),
                state: Mutex::new(State { cells, members: vec![HashSet::new(); n], dirty: vec![true; n] }),
            }),
        };
        idx.rebuild_all()?;
        // The observer holds the index weakly, so dropping the index makes it a no-op.
        repo.add_ref_observer(Arc::new(Observer(Arc::downgrade(&idx.inner))));
        Ok(idx)
    }

    /// Replaces the cell list (a policy reload) and rebuilds every cell.
    pub fn on_policy_reload(&self, cells: Vec<String>) -> Result<()> {
        {
            let mut st = self.lock()?;
            let n = cells.len();
            st.cells = cells;
            st.members = vec![HashSet::new(); n];
            st.dirty = vec![true; n];
        }
        self.rebuild_all()
    }

    /// For each hash, the candidate cells whose names reach it (in candidate
    /// order). Unknown cell ids are ignored.
    pub fn cells_reaching_many(&self, hashes: &[Hash], candidates: &[CellId]) -> Result<Vec<Vec<CellId>>> {
        let mut st = self.lock()?;
        for &c in candidates {
            let c = c as usize;
            if c < st.dirty.len() && st.dirty[c] {
                let members = self.mark_cell(&st.cells, c)?;
                st.members[c] = members;
                st.dirty[c] = false;
            }
        }
        Ok(hashes
            .iter()
            .map(|h| {
                candidates
                    .iter()
                    .copied()
                    .filter(|&c| (c as usize) < st.members.len() && st.members[c as usize].contains(h))
                    .collect()
            })
            .collect())
    }

    /// Whether each hash is reachable from some candidate cell.
    pub fn readable_many(&self, hashes: &[Hash], candidates: &[CellId]) -> Result<Vec<bool>> {
        Ok(self.cells_reaching_many(hashes, candidates)?.into_iter().map(|v| !v.is_empty()).collect())
    }

    /// The cell of `name`: the deepest cell prefix covering it.
    pub fn cell_of(cells: &[String], name: &str) -> Option<CellId> {
        let mut best: Option<(usize, usize)> = None;
        for (i, p) in cells.iter().enumerate() {
            if pico2peta_core::is_under(name, p) && best.is_none_or(|(_, l)| p.len() > l) {
                best = Some((i, p.len()));
            }
        }
        best.map(|(i, _)| i as CellId)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.inner.state.lock().map_err(|_| Error::Corrupt("reach index lock poisoned".into()))
    }

    fn rebuild_all(&self) -> Result<()> {
        let mut st = self.lock()?;
        for c in 0..st.cells.len() {
            let members = self.mark_cell(&st.cells, c)?;
            st.members[c] = members;
            st.dirty[c] = false;
        }
        Ok(())
    }

    /// Everything reachable from the names whose cell is `c`.
    fn mark_cell(&self, cells: &[String], c: usize) -> Result<HashSet<Hash>> {
        let repo = &self.inner.repo;
        let mut members = HashSet::new();
        let tracer = repo.fronts().tracer(repo)?;
        let prefix = &cells[c];
        for (name, v) in repo.list_refs(prefix.trim_end_matches('/'))? {
            if Self::cell_of(cells, name.as_str()) == Some(c as CellId) {
                closure(repo, &tracer, v.hash, &mut members)?;
            }
        }
        Ok(members)
    }

    fn on_change(&self, changes: &[(Name, Option<Hash>, u64)]) {
        let Ok(mut st) = self.lock() else { return };
        let repo = &self.inner.repo;
        // One tracer per batch, built now: fronts' snapshots (e.g. git's oid map) then see the
        // objects the writer stored before moving the names.
        let tracer = repo.fronts().tracer(repo);
        for (name, new, version) in changes {
            let Some(c) = Self::cell_of(&st.cells, name.as_str()) else { continue };
            let c = c as usize;
            if st.dirty[c] {
                continue;
            }
            match (new, *version) {
                (Some(h), 1) => {
                    // Created from nothing: only additions. On any error, fall
                    // back to an exact recompute (dirty) rather than guess.
                    let ok = match &tracer {
                        Ok(t) => closure(repo, t, *h, &mut st.members[c]),
                        Err(_) => Err(Error::Corrupt("front tracer".into())),
                    };
                    if ok.is_err() {
                        st.dirty[c] = true;
                    }
                }
                _ => st.dirty[c] = true,
            }
        }
    }
}

struct Observer(Weak<Inner>);

impl RefObserver for Observer {
    fn changed(&self, _repo: &LocalRepo, changes: &[(Name, Option<Hash>, u64)]) {
        if let Some(inner) = self.0.upgrade() {
            ReachIndex { inner }.on_change(changes);
        }
    }
}

/// Adds everything reachable from `root` to `seen` (stopping at members).
/// Objects that are missing (a dangling ref) contribute nothing.
fn closure(repo: &LocalRepo, tracer: &dyn Tracer, root: Hash, seen: &mut HashSet<Hash>) -> Result<()> {
    let mut queue = VecDeque::from([root]);
    while let Some(h) = queue.pop_front() {
        if seen.contains(&h) {
            continue;
        }
        if !repo.packs().contains(&h)? {
            continue;
        }
        seen.insert(h);
        if let Some(kids) = repo.packs().chunks(&h)? {
            queue.extend(kids.into_iter().filter(|k| !seen.contains(k)));
        }
        for k in tracer.children(&h, repo as &dyn ObjectStore)? {
            if !seen.contains(&k) {
                queue.push_back(k);
            }
        }
    }
    Ok(())
}
