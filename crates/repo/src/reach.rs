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
//! `delete_ref` on any clone of that `LocalRepo` notifies it before returning.
//! A name created from nothing (version 1) only adds objects, so its target is
//! queued as a **pending root** of its cell; the commit does no traversal. A
//! background worker drains pending roots (one cell at a time, under that
//! cell's lock), and a query on a cell with pending roots drains them first, so
//! answers are exact and read-your-writes holds. (The worker never runs an
//! exact recompute: that is left to the next query of a dirty cell.) A move, a re-creation after a
//! delete, or a delete may make objects unreachable, so it marks the cell
//! **dirty** at once (dropping its pending roots); a query on a dirty cell
//! recomputes that cell exactly first. "Unreadable as soon as its last readable
//! name is deleted" therefore holds immediately.
//!
//! **Limits (v1).** In memory only (rebuilt at open, about one GC mark per
//! cell). Writes by other processes or hosts to the same repository are not
//! observed; the server must be the repository's only writer for the index to
//! be exact (the CLI on the server host writes through its own handle).

use crate::{LocalRepo, Tracer};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Repository, Result};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, Weak};

/// The cell prefixes and one state per cell.
type Cells = (Arc<Vec<String>>, Vec<Arc<Cell>>);

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
    /// The cell prefixes and one state per cell (replaced together on a policy reload).
    cells: RwLock<Cells>,
    /// Cells with pending roots, for the worker (which holds the queue, not the index).
    queue: Arc<Queue>,
}

#[derive(Default)]
struct Queue {
    work: Mutex<(VecDeque<Arc<Cell>>, bool)>,
    wake: Condvar,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Tell the worker to stop now rather than at its next timeout.
        if let Ok(mut w) = self.queue.work.lock() {
            w.1 = true;
        }
        self.queue.wake.notify_all();
    }
}

/// One cell. Commits touch only `inbox` (briefly); whoever settles the cell holds `members`.
#[derive(Default)]
struct Cell {
    members: Mutex<Members>,
    inbox: Mutex<Inbox>,
}

#[derive(Default)]
struct Members {
    set: HashSet<Hash>,
    /// Recompute from the cell's names before the next answer.
    dirty: bool,
}

/// Changes committed since the cell was last settled.
#[derive(Default)]
struct Inbox {
    /// Targets of names created from nothing (only additions).
    roots: Vec<Hash>,
    /// A move or delete happened: the next settle recomputes exactly.
    dirty: bool,
}

impl ReachIndex {
    /// Builds the index over `cells` (each a name prefix; `""` covers every
    /// name) and registers it with `repo` (and every clone of it).
    pub fn open(repo: &LocalRepo, cells: Vec<String>) -> Result<ReachIndex> {
        let idx = ReachIndex {
            inner: Arc::new(Inner {
                repo: repo.clone(),
                cells: RwLock::new(Self::fresh(cells)),
                queue: Arc::default(),
            }),
        };
        idx.rebuild_all()?;
        // The observer and the worker hold the index weakly: dropping the index stops both.
        repo.add_ref_observer(Arc::new(Observer(Arc::downgrade(&idx.inner))));
        let weak = Arc::downgrade(&idx.inner);
        let queue = idx.inner.queue.clone();
        std::thread::Builder::new()
            .name("p2p-reach".into())
            .spawn(move || worker(weak, queue))
            .map_err(|e| Error::Corrupt(format!("reach worker: {e}")))?;
        Ok(idx)
    }

    fn fresh(cells: Vec<String>) -> Cells {
        let states = (0..cells.len())
            .map(|_| {
                Arc::new(Cell { members: Mutex::new(Members { set: HashSet::new(), dirty: true }), ..Cell::default() })
            })
            .collect();
        (Arc::new(cells), states)
    }

    /// Replaces the cell list (a policy reload) and rebuilds every cell.
    pub fn on_policy_reload(&self, cells: Vec<String>) -> Result<()> {
        *self.inner.cells.write().map_err(|_| poisoned())? = Self::fresh(cells);
        self.rebuild_all()
    }

    /// For each hash, the candidate cells whose names reach it (in candidate
    /// order). Unknown cell ids are ignored.
    pub fn cells_reaching_many(&self, hashes: &[Hash], candidates: &[CellId]) -> Result<Vec<Vec<CellId>>> {
        let (cells, states) = self.snapshot()?;
        let mut out = vec![Vec::new(); hashes.len()];
        for &c in candidates {
            let Some(state) = states.get(c as usize) else { continue };
            let mut m = lock(&state.members)?;
            self.settle(&cells, c as usize, state, &mut m)?;
            for (i, h) in hashes.iter().enumerate() {
                if m.set.contains(h) {
                    out[i].push(c);
                }
            }
        }
        Ok(out)
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

    fn snapshot(&self) -> Result<Cells> {
        let g = self.inner.cells.read().map_err(|_| poisoned())?;
        Ok((g.0.clone(), g.1.clone()))
    }

    fn rebuild_all(&self) -> Result<()> {
        let (cells, states) = self.snapshot()?;
        for (c, state) in states.iter().enumerate() {
            let mut m = lock(&state.members)?;
            self.settle(&cells, c, state, &mut m)?;
        }
        Ok(())
    }

    /// Brings a cell up to date (the caller holds its `members`): applies its inbox until the inbox
    /// is empty, an exact recompute if anything made it dirty, else the new roots' closures.
    fn settle(&self, cells: &[String], c: usize, cell: &Cell, m: &mut Members) -> Result<()> {
        loop {
            let (roots, dirty) = {
                let mut inbox = cell.inbox.lock().map_err(|_| poisoned())?;
                (std::mem::take(&mut inbox.roots), std::mem::take(&mut inbox.dirty))
            };
            m.dirty |= dirty;
            if m.dirty {
                m.set = self.mark_cell(cells, c)?;
                m.dirty = false;
            } else if !roots.is_empty() {
                let repo = &self.inner.repo;
                let tracer = repo.fronts().tracer(repo)?;
                for h in roots {
                    if let Err(e) = closure(repo, &tracer, h, &mut m.set) {
                        // Never guess: recompute exactly on the next answer.
                        m.dirty = true;
                        return Err(e);
                    }
                }
            } else {
                return Ok(());
            }
        }
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

    /// The commit path: O(changes), no traversal, never waits for a traversal.
    fn on_change(&self, changes: &[(Name, Option<Hash>, u64)]) {
        let Ok((cells, states)) = self.snapshot() else { return };
        let mut queued: Vec<usize> = Vec::new();
        for (name, new, version) in changes {
            let Some(c) = Self::cell_of(&cells, name.as_str()) else { continue };
            let c = c as usize;
            let Ok(mut inbox) = states[c].inbox.lock() else { continue };
            match (new, *version) {
                (Some(h), 1) => {
                    inbox.roots.push(*h);
                    if !queued.contains(&c) {
                        queued.push(c);
                    }
                }
                _ => {
                    inbox.dirty = true;
                    inbox.roots.clear();
                }
            }
        }
        if !queued.is_empty()
            && let Ok(mut w) = self.inner.queue.work.lock()
        {
            for c in queued {
                w.0.push_back(states[c].clone());
            }
            self.inner.queue.wake.notify_one();
        }
    }
}

fn poisoned() -> Error {
    Error::Corrupt("reach index lock poisoned".into())
}

fn lock(m: &Mutex<Members>) -> Result<MutexGuard<'_, Members>> {
    m.lock().map_err(|_| poisoned())
}

/// Drains pending roots in the background until the index is dropped. Holds
/// the index only while settling one cell, so dropping it ends the thread.
fn worker(weak: Weak<Inner>, queue: Arc<Queue>) {
    loop {
        let state = {
            let Ok(mut w) = queue.work.lock() else { return };
            loop {
                if w.1 {
                    return;
                }
                if let Some(s) = w.0.pop_front() {
                    break s;
                }
                w = match queue.wake.wait(w) {
                    Ok(w) => w,
                    Err(_) => return,
                };
            }
        };
        let Some(inner) = weak.upgrade() else { return };
        let idx = ReachIndex { inner };
        let Ok((cells, states)) = idx.snapshot() else { return };
        // A cell from before a policy reload is gone: skip it.
        let Some(c) = states.iter().position(|s| Arc::ptr_eq(s, &state)) else { continue };
        // Only additions are done early. A cell that a move or delete made dirty is recomputed
        // by its next query instead: an exact recompute is a full mark of the cell, and running
        // it in the background right after a push would compete with the next push.
        let inbox_dirty = state.inbox.lock().map(|i| i.dirty).unwrap_or(true);
        if inbox_dirty {
            continue;
        }
        if let Ok(mut m) = lock(&state.members)
            && !m.dirty
        {
            let _ = idx.settle(&cells, c, &state, &mut m);
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

/// Adds everything reachable from `root` to `seen` (stopping at members), one
/// breadth-first wave at a time so fronts can resolve a wave's children in one
/// batch. Objects that are missing (a dangling ref) contribute nothing.
fn closure(repo: &LocalRepo, tracer: &dyn Tracer, root: Hash, seen: &mut HashSet<Hash>) -> Result<()> {
    let mut wave = vec![root];
    while !wave.is_empty() {
        let mut live = Vec::with_capacity(wave.len());
        for h in wave.drain(..) {
            if seen.contains(&h) || !repo.packs().contains(&h)? {
                continue;
            }
            seen.insert(h);
            live.push(h);
        }
        let mut next = Vec::new();
        for h in &live {
            if let Some(kids) = repo.packs().chunks(h)? {
                next.extend(kids.into_iter().filter(|k| !seen.contains(k)));
            }
        }
        for kids in children_of_wave(tracer, &live, repo)? {
            next.extend(kids.into_iter().filter(|k| !seen.contains(k)));
        }
        wave = next;
    }
    Ok(())
}

/// The front children of each object of a wave.
fn children_of_wave(tracer: &dyn Tracer, live: &[Hash], repo: &LocalRepo) -> Result<Vec<Vec<Hash>>> {
    tracer.children_many(live, repo as &dyn ObjectStore)
}
