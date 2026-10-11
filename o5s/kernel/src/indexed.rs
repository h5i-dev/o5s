//! Dense adjacency traversal with reusable marks. The native adapter supplies
//! stable vertex indices and all-false marks of the same length as the rows.
//! Every reached vertex is queued once; clearing touches only that queue.
pub struct Traversal {
    pub marks: Vec<bool>,
    pub queue: Vec<usize>,
}

pub fn enqueue(mut state: Traversal, neighbors: &Vec<usize>) -> Traversal {
    let mut i = 0;
    while i < neighbors.len() {
        let vertex = neighbors[i];
        if !state.marks[vertex] {
            state.marks[vertex] = true;
            state.queue.push(vertex);
        }
        i += 1;
    }
    state
}

pub fn clear(mut state: Traversal) -> Traversal {
    let mut i = 0;
    while i < state.queue.len() {
        state.marks[state.queue[i]] = false;
        i += 1;
    }
    state
}

pub fn reachable(rows: &Vec<Vec<usize>>, seeds: &Vec<usize>, marks: Vec<bool>) -> Traversal {
    let mut state = enqueue(
        Traversal {
            marks,
            queue: Vec::new(),
        },
        seeds,
    );
    let mut i = 0;
    while i < state.queue.len() {
        let vertex = state.queue[i];
        state = enqueue(state, &rows[vertex]);
        i += 1;
    }
    clear(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_search_matches_reference_and_reuses_clean_marks() {
        for mask in 0..512 {
            let mut rows = vec![vec![]; 3];
            let mut edges = Vec::new();
            for (u, row) in rows.iter_mut().enumerate() {
                for v in 0..3 {
                    if mask & (1 << (u * 3 + v)) != 0 {
                        row.push(v);
                        edges.push((u as u64, v as u64));
                    }
                }
            }
            let mut marks = vec![false; 3];
            for seed_mask in 0..8 {
                let mut seeds = (0..3)
                    .filter(|v| seed_mask & (1 << v) != 0)
                    .collect::<Vec<_>>();
                seeds.extend(seeds.clone());
                let mut expected =
                    crate::graph::reachable(&edges, &seeds.iter().map(|v| *v as u64).collect())
                        .into_iter()
                        .map(|v| v as usize)
                        .collect::<Vec<_>>();
                expected.sort_unstable();
                let state = reachable(&rows, &seeds, marks);
                assert!(state.marks.iter().all(|v| !v));
                let mut actual = state.queue;
                actual.sort_unstable();
                assert_eq!(actual, expected);
                marks = state.marks;
            }
        }
    }
}
