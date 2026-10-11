//! Experimental live-boundary graph summaries, independent of whole-file edits.
//! Rebuild a touched weak component from original edges, avoiding stale shortcut
//! edges after new edges, deletion, or restoration. The Lean `Summary` invariant
//! specifies the constructor; exhaustive tests check this Rust implementation.
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
pub struct Projection {
    indices: BTreeMap<u64, usize>,
    ids: Vec<u64>,
    weak_rows: Vec<Vec<usize>>,
    original_rows: Vec<Vec<usize>>,
    dead_rows: Vec<Vec<usize>>,
    alive: Vec<bool>,
    marks: Vec<bool>,
    live: BTreeSet<u64>,
    outgoing: BTreeMap<u64, BTreeSet<u64>>,
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
            let (index, added) = self.ensure_vertex(id);
            if added || self.live.contains(&id) != alive {
                touched.insert(id);
            }
            if alive {
                self.dead_rows[index].clear();
                self.live.insert(id);
            } else {
                if self.alive[index] {
                    self.dead_rows[index] = self.original_rows[index].clone();
                }
                self.live.remove(&id);
            }
            self.alive[index] = alive;
        }
        for &(a, b) in edges {
            let (ia, _) = self.ensure_vertex(a);
            let (ib, _) = self.ensure_vertex(b);
            if self.outgoing.entry(a).or_default().insert(b) {
                self.original_rows[ia].push(ib);
                if !self.alive[ia] {
                    self.dead_rows[ia].push(ib);
                }
                self.weak_rows[ia].push(ib);
                if ia != ib {
                    self.weak_rows[ib].push(ia);
                }
                touched.extend([a, b]);
            }
        }
        let touched = self.affected_component(touched);
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
            let index = self.indices[&source];
            let search = o5s_kernel::indexed::reachable(
                &self.dead_rows,
                &self.original_rows[index],
                std::mem::take(&mut self.marks),
            );
            work.examined_edges += self.original_rows[index].len()
                + search
                    .queue
                    .iter()
                    .map(|i| self.dead_rows[*i].len())
                    .sum::<usize>();
            let boundary = o5s_kernel::indexed::keep_live(&search.queue, &self.alive)
                .into_iter()
                .map(|i| self.ids[i])
                .collect();
            self.marks = search.marks;
            self.summary.insert(source, boundary);
        }
        work
    }
    fn ensure_vertex(&mut self, id: u64) -> (usize, bool) {
        if let Some(&index) = self.indices.get(&id) {
            return (index, false);
        }
        let index = self.ids.len();
        self.indices.insert(id, index);
        self.ids.push(id);
        self.weak_rows.push(Vec::new());
        self.original_rows.push(Vec::new());
        self.dead_rows.push(Vec::new());
        self.alive.push(false);
        self.marks.push(false);
        (index, true)
    }
    fn affected_component(&mut self, touched: BTreeSet<u64>) -> BTreeSet<u64> {
        let seeds = touched.iter().map(|id| self.indices[id]).collect();
        let search = o5s_kernel::indexed::reachable(
            &self.weak_rows,
            &seeds,
            std::mem::take(&mut self.marks),
        );
        self.marks = search.marks;
        search.queue.into_iter().map(|i| self.ids[i]).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_components_match_extracted_reference_for_every_small_graph() {
        for edge_mask in 0..512 {
            let mut edges = Vec::new();
            for u in 0..3 {
                for v in 0..3 {
                    if edge_mask & (1 << (u * 3 + v)) != 0 {
                        edges.push((u, v));
                    }
                }
            }
            let mut projection = Projection::default();
            projection.apply(&[(0, false), (1, false), (2, false)], &edges);
            for seed_mask in 0..8 {
                let seeds = (0..3)
                    .filter(|id| seed_mask & (1 << id) != 0)
                    .collect::<Vec<_>>();
                let expected = o5s_kernel::graph::weak_component(&edges, &seeds)
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    projection.affected_component(seeds.into_iter().collect()),
                    expected
                );
            }
        }
    }
    #[test]
    fn dense_indices_preserve_sparse_ids_when_components_join_and_marks_are_reused() {
        let ids = [u64::MAX, 17, 9_000_000_001, 0];
        let mut projection = Projection::default();
        projection.apply(&ids.map(|id| (id, false)), &[]);
        let mut edges = Vec::new();
        for pair in [(17, 0), (u64::MAX, 9_000_000_001), (0, 9_000_000_001)] {
            edges.push(pair);
            projection.apply(&[], &[pair]);
            for id in ids {
                let expected = o5s_kernel::graph::weak_component(&edges, &vec![id]);
                assert_eq!(
                    projection.affected_component(BTreeSet::from([id])),
                    expected.into_iter().collect(),
                );
                assert!(projection.marks.iter().all(|mark| !mark));
            }
        }
        projection.apply(&[(42, false)], &[(42, 17)]);
        assert_eq!(projection.affected_component(BTreeSet::from([42])).len(), 5);
        assert!(projection.marks.iter().all(|mark| !mark));
    }
}
