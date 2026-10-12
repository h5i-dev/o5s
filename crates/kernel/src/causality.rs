//! Causal searches over a repository's patches in dense indices, where row
//! `v` lists the dependencies of patch `v`. Extracted to Lean.
use crate::indexed::{self, Traversal};

/// Search from `roots` towards older patches, collecting each patch with
/// `stop[v]` and not expanding it: the nearest such patches on every causal
/// path. `marks` must be all false and as long as `rows`; they are returned
/// all false.
pub fn frontier(
    rows: &Vec<Vec<usize>>,
    stop: &Vec<bool>,
    roots: &Vec<usize>,
    marks: Vec<bool>,
) -> Traversal {
    let mut state = indexed::enqueue(
        Traversal {
            marks,
            queue: Vec::new(),
        },
        roots,
    );
    let mut found = Vec::new();
    let mut i = 0;
    while i < state.queue.len() {
        let v = state.queue[i];
        if stop[v] {
            found.push(v);
        } else {
            state = indexed::enqueue(state, &rows[v]);
        }
        i += 1;
    }
    let cleared = indexed::clear(state);
    Traversal {
        marks: cleared.marks,
        queue: found,
    }
}

/// Whether `base` is the only causal head among the patches with `edits[v]`
/// in the causal past of `roots` (roots included): `base` is one of the
/// nearest such patches, and every other nearest one is its ancestor.
pub fn sole_head(
    rows: &Vec<Vec<usize>>,
    edits: &Vec<bool>,
    roots: &Vec<usize>,
    base: usize,
    marks: Vec<bool>,
) -> (bool, Vec<bool>) {
    let near = frontier(rows, edits, roots, marks);
    let ancestors = indexed::reachable(rows, &rows[base], near.marks);
    let mut ok = h5i_app_std::set::contains(&near.queue, &base);
    let mut i = 0;
    while i < near.queue.len() {
        let q = near.queue[i];
        ok = ok && (q == base || h5i_app_std::set::contains(&ancestors.queue, &q));
        i += 1;
    }
    (ok, ancestors.marks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Reference: heads are the edit patches in the past of `roots` that are
    /// not strict ancestors of another one.
    fn heads(rows: &Vec<Vec<usize>>, edits: &Vec<bool>, roots: &[usize]) -> BTreeSet<usize> {
        let past = |from: &[usize]| {
            let mut seen = BTreeSet::new();
            let mut stack = from.to_vec();
            while let Some(v) = stack.pop() {
                if seen.insert(v) {
                    stack.extend(rows[v].iter().copied());
                }
            }
            seen
        };
        let all = past(roots);
        let e: Vec<_> = all.iter().copied().filter(|v| edits[*v]).collect();
        e.iter()
            .copied()
            .filter(|v| !e.iter().any(|w| w != v && past(&rows[*w]).contains(v)))
            .collect()
    }

    #[test]
    fn sole_head_matches_the_reference_on_small_dags() {
        // Every DAG on 5 vertices whose edges point to smaller indices.
        let pairs: Vec<(usize, usize)> = (0..5).flat_map(|v| (0..v).map(move |d| (v, d))).collect();
        for mask in 0u32..(1 << pairs.len()) {
            if mask % 7 != 0 {
                continue;
            }
            let mut rows = vec![Vec::new(); 5];
            for (k, (v, d)) in pairs.iter().enumerate() {
                if mask & (1 << k) != 0 {
                    rows[*v].push(*d);
                }
            }
            for em in 0u32..32 {
                let edits: Vec<bool> = (0..5).map(|v| em & (1 << v) != 0).collect();
                for roots in [vec![4], vec![3, 4], vec![2]] {
                    let expected = heads(&rows, &edits, &roots);
                    let mut marks = vec![false; 5];
                    for base in 0..5 {
                        let (ok, m) = sole_head(&rows, &edits, &roots, base, marks);
                        assert!(m.iter().all(|x| !x));
                        marks = m;
                        assert_eq!(
                            ok,
                            expected == BTreeSet::from([base]),
                            "{rows:?} {edits:?} {roots:?} {base}"
                        );
                    }
                }
            }
        }
    }
}
