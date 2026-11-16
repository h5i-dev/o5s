//! Commit graph for the Git front: the history walks (negotiation, the "what the client has"
//! boundary, shallow cuts, the commit waves of a pack walk) need only each commit's tree,
//! parents and time, and each tag's target. Reading those from the store means one object read
//! (and decompression) per commit, every request.
//!
//! [`Graph`] keeps them in memory, keyed by oid, with the core hash of each node so that refs
//! (which point at core objects) resolve to oids without reading their objects. Nodes are
//! content-addressed facts, so they are never stale; the cache is bounded and simply cleared
//! when full. Each repository's graph is also persisted, as one object behind
//! `git/_commitgraph/<repo>`, rewritten in the background after pushes, so a fresh process
//! loads a repository's history with one read. Like git's commit-graph file, it is derived
//! data: losing it costs speed, not correctness.

use crate::git::{Kind, Oid};
use pico2peta_core::{Error, Hash, Name, Observed, Repository, Result};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 8] = b"P2PCGR01";
/// Nodes kept in memory across repositories (about 150 bytes each).
const MAX_NODES: usize = 4 << 20;

/// A commit or tag, as the walks need it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HistNode {
    pub(crate) kind: Kind,
    /// Commit: tree, then parents. Tag: its target.
    pub(crate) refs: Vec<Oid>,
    /// Committer time (commits; 0 for tags).
    pub(crate) time: u64,
    /// The core object.
    pub(crate) hash: Hash,
}

#[derive(Default)]
struct Inner {
    nodes: HashMap<Oid, Arc<HistNode>>,
    by_hash: HashMap<Hash, Oid>,
    /// Repositories whose persisted graph was loaded (or found missing) by this process.
    loaded: HashSet<String>,
    /// Node count of each repository's graph as last persisted or loaded.
    persisted: HashMap<String, usize>,
    /// Repositories with a background refresh running.
    refreshing: HashSet<String>,
}

/// The in-memory commit graph (all repositories).
#[derive(Default)]
pub(crate) struct Graph {
    inner: Mutex<Inner>,
    /// Serializes loads of persisted graphs (one read per repository, not one per thread).
    load: Mutex<()>,
}

pub(crate) fn graph_ref(repo: &str) -> Option<Name> {
    Name::new(format!("git/_commitgraph/{repo}")).ok()
}

impl Graph {
    pub(crate) fn get(&self, oid: &Oid) -> Option<Arc<HistNode>> {
        self.inner.lock().ok()?.nodes.get(oid).cloned()
    }

    pub(crate) fn oid_of_hash(&self, h: &Hash) -> Option<Oid> {
        self.inner.lock().ok()?.by_hash.get(h).copied()
    }

    pub(crate) fn note_hash(&self, h: Hash, oid: Oid) {
        if let Ok(mut g) = self.inner.lock() {
            g.by_hash.insert(h, oid);
        }
    }

    pub(crate) fn insert(&self, oid: Oid, n: Arc<HistNode>) {
        if let Ok(mut g) = self.inner.lock() {
            if g.nodes.len() >= MAX_NODES {
                // Bounded: start over (everything is derivable; repos reload their graphs).
                g.nodes.clear();
                g.by_hash.clear();
                g.loaded.clear();
                g.persisted.clear();
            }
            g.by_hash.insert(n.hash, oid);
            g.nodes.insert(oid, n);
        }
    }

    /// Loads `repo`'s persisted graph into memory, once per process.
    pub(crate) fn ensure_loaded(&self, repo_store: &dyn Repository, repo: &str) -> Result<()> {
        if self.inner.lock().map_err(|_| poisoned())?.loaded.contains(repo) {
            return Ok(());
        }
        let _g = self.load.lock().map_err(|_| poisoned())?;
        if self.inner.lock().map_err(|_| poisoned())?.loaded.contains(repo) {
            return Ok(()); // loaded while we waited
        }
        let mut count = 0;
        if let Some(n) = graph_ref(repo)
            && let Some(v) = repo_store.resolve(&n)?
        {
            match repo_store.get(&v.hash, None) {
                Ok(b) => {
                    let nodes = decode(&b)?;
                    count = nodes.len();
                    for (o, node) in nodes {
                        self.insert(o, Arc::new(node));
                    }
                }
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let mut g = self.inner.lock().map_err(|_| poisoned())?;
        g.loaded.insert(repo.to_string());
        g.persisted.insert(repo.to_string(), count);
        Ok(())
    }

    /// Whether this process loaded `repo`'s persisted graph (or found none).
    pub(crate) fn is_loaded(&self, repo: &str) -> bool {
        self.inner.lock().is_ok_and(|g| g.loaded.contains(repo))
    }

    /// Whether `repo` has a persisted graph of at least `n` nodes.
    pub(crate) fn persisted_at_least(&self, repo: &str, n: usize) -> bool {
        self.inner.lock().ok().and_then(|g| g.persisted.get(repo).copied()).is_some_and(|c| c >= n)
    }

    /// Persists `nodes` (all of `repo`'s history) as its graph object.
    pub(crate) fn persist(&self, store: &dyn Repository, repo: &str, nodes: &[(Oid, Arc<HistNode>)]) -> Result<()> {
        let Some(name) = graph_ref(repo) else { return Ok(()) };
        let h = store.put(&encode(nodes))?;
        let obs = match store.resolve(&name)? {
            Some(v) => Observed::Version(v.version),
            None => Observed::Absent,
        };
        // A lost race means another writer persisted a graph just now: fine either way.
        let _ = store.replace(&name, &[(name.clone(), obs)], h)?;
        if let Ok(mut g) = self.inner.lock() {
            g.persisted.insert(repo.to_string(), nodes.len());
        }
        Ok(())
    }

    /// Marks a background refresh of `repo` as running; `false` if one already is.
    pub(crate) fn start_refresh(&self, repo: &str) -> bool {
        self.inner.lock().is_ok_and(|mut g| g.refreshing.insert(repo.to_string()))
    }

    pub(crate) fn end_refresh(&self, repo: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.refreshing.remove(repo);
        }
    }
}

fn poisoned() -> Error {
    Error::Corrupt("commit graph lock poisoned".into())
}

fn encode(nodes: &[(Oid, Arc<HistNode>)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + nodes.len() * 120);
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&(nodes.len() as u64).to_le_bytes());
    for (o, n) in nodes {
        v.push(o.len() as u8);
        v.extend_from_slice(o);
        v.push(n.kind as u8);
        v.extend_from_slice(&n.time.to_le_bytes());
        v.extend_from_slice(n.hash.as_bytes());
        v.extend_from_slice(&(n.refs.len() as u32).to_le_bytes());
        for r in &n.refs {
            v.push(r.len() as u8);
            v.extend_from_slice(r);
        }
    }
    v
}

/// Fuzzing: the persisted graph decoder must reject garbage without panicking.
pub(crate) fn fuzz_decode(b: &[u8]) {
    let _ = decode(b);
}

fn decode(b: &[u8]) -> Result<Vec<(Oid, HistNode)>> {
    let bad = || Error::Corrupt("bad commit graph object".into());
    let mut p = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = b.get(p..p + n).ok_or_else(bad)?;
        p += n;
        Ok(s)
    };
    if take(8)? != MAGIC {
        return Err(bad());
    }
    let count = u64::from_le_bytes(take(8)?.try_into().map_err(|_| bad())?) as usize;
    let mut out = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let l = take(1)?[0] as usize;
        let o = Oid::from_slice(take(l)?).ok_or_else(bad)?;
        let kind = Kind::from_code(take(1)?[0]).ok_or_else(bad)?;
        let time = u64::from_le_bytes(take(8)?.try_into().map_err(|_| bad())?);
        let hash = Hash(take(32)?.try_into().map_err(|_| bad())?);
        let nr = u32::from_le_bytes(take(4)?.try_into().map_err(|_| bad())?) as usize;
        if nr > 1 << 16 {
            return Err(bad());
        }
        let mut refs = Vec::with_capacity(nr);
        for _ in 0..nr {
            let l = take(1)?[0] as usize;
            refs.push(Oid::from_slice(take(l)?).ok_or_else(bad)?);
        }
        out.push((o, HistNode { kind, refs, time, hash }));
    }
    if p != b.len() {
        return Err(bad());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_roundtrip_and_rejects_garbage() {
        let o = |i: u8| Oid::from_slice(&[i; 20]).unwrap();
        let o32 = Oid::from_slice(&[9u8; 32]).unwrap();
        let nodes = vec![
            (
                o(1),
                Arc::new(HistNode { kind: Kind::Commit, refs: vec![o(2), o(3), o(4)], time: 77, hash: Hash::of(b"a") }),
            ),
            (o32, Arc::new(HistNode { kind: Kind::Tag, refs: vec![o32], time: 0, hash: Hash::of(b"b") })),
        ];
        let enc = encode(&nodes);
        let dec = decode(&enc).unwrap();
        assert_eq!(dec.len(), 2);
        for ((a, na), (b, nb)) in nodes.iter().zip(&dec) {
            assert_eq!(a, b);
            assert_eq!(**na, *nb);
        }
        for cut in [0, 8, 15, 20, enc.len() - 1] {
            assert!(decode(&enc[..cut]).is_err(), "{cut}");
        }
        let mut extra = enc.clone();
        extra.push(0);
        assert!(decode(&extra).is_err());
    }
}
