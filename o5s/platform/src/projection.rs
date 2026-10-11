//! Experimental live-boundary graph summaries, independent of whole-file edits.
//! Rebuild a touched weak component from original edges, avoiding stale shortcut
//! edges after new edges, deletion, or restoration. The Lean `Summary` invariant
//! specifies the constructor; exhaustive tests check this Rust implementation.
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
pub struct Projection {
    nodes: BTreeSet<u64>,
    live: BTreeSet<u64>,
    outgoing: BTreeMap<u64, BTreeSet<u64>>,
    incoming: BTreeMap<u64, BTreeSet<u64>>,
    summary: BTreeMap<u64, BTreeSet<u64>>,
}
#[derive(Default, Debug, PartialEq, Eq)]
pub struct ProjectionWork {
    pub component_vertices: usize,
    pub rebuilt_sources: usize,
    pub examined_edges: usize,
}
impl Projection {
    /// Apply a batch, then rebuild once per affected component. Retain original
    /// graph information; summaries are derived state, never patch identity.
    pub fn apply(&mut self, vertices: &[(u64, bool)], edges: &[(u64, u64)]) -> ProjectionWork {
        let mut touched = BTreeSet::new();
        for &(id, alive) in vertices {
            if self.nodes.insert(id) || self.live.contains(&id) != alive {
                touched.insert(id);
            }
            if alive {
                self.live.insert(id);
            } else {
                self.live.remove(&id);
            }
        }
        for &(a, b) in edges {
            self.nodes.extend([a, b]);
            if self.outgoing.entry(a).or_default().insert(b) {
                self.incoming.entry(b).or_default().insert(a);
                touched.extend([a, b]);
            }
        }
        let mut stack: Vec<u64> = touched.iter().copied().collect();
        while let Some(id) = stack.pop() {
            for next in self
                .outgoing
                .get(&id)
                .into_iter()
                .flatten()
                .chain(self.incoming.get(&id).into_iter().flatten())
            {
                if touched.insert(*next) {
                    stack.push(*next);
                }
            }
        }
        let mut work = ProjectionWork {
            component_vertices: touched.len(),
            ..Default::default()
        };
        for &source in &touched {
            self.summary.remove(&source);
            if !self.live.contains(&source) {
                continue;
            }
            work.rebuilt_sources += 1;
            let mut seen = BTreeSet::new();
            let mut boundary = BTreeSet::new();
            let mut stack: Vec<u64> = self
                .outgoing
                .get(&source)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            while let Some(next) = stack.pop() {
                work.examined_edges += 1;
                if self.live.contains(&next) {
                    boundary.insert(next);
                } else if seen.insert(next) {
                    stack.extend(self.outgoing.get(&next).into_iter().flatten().copied());
                }
            }
            self.summary.insert(source, boundary);
        }
        work
    }
    pub fn original_edges(&self) -> Vec<(u64, u64)> {
        self.outgoing
            .iter()
            .flat_map(|(a, bs)| bs.iter().map(move |b| (*a, *b)))
            .collect()
    }
    pub fn summary_edges(&self) -> Vec<(u64, u64)> {
        self.summary
            .iter()
            .flat_map(|(a, bs)| bs.iter().map(move |b| (*a, *b)))
            .collect()
    }
    pub fn live_reachable(&self, source: u64) -> BTreeSet<u64> {
        if !self.live.contains(&source) {
            return BTreeSet::new();
        }
        let mut found = BTreeSet::from([source]);
        let mut stack = vec![source];
        while let Some(x) = stack.pop() {
            for y in self.summary.get(&x).into_iter().flatten() {
                if found.insert(*y) {
                    stack.push(*y);
                }
            }
        }
        found
    }
}
