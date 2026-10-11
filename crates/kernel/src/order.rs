//! Frontier selection for a partially emitted precedence graph.
//! A vertex is ready exactly when no incoming predecessor remains un-emitted.

pub fn predecessors(edges: &Vec<(u64, u64)>, vertex: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < edges.len() {
        if edges[i].1 == vertex {
            out.push(edges[i].0);
        }
        i += 1;
    }
    out
}

pub fn ready(predecessors: &Vec<u64>, remaining: &Vec<u64>) -> bool {
    !h5i_app_std::set::intersects(predecessors, remaining)
}

/// Retain every ready vertex, including incomparable alternatives. Consumers
/// must not silently resolve ambiguity by choosing a numeric identifier.
pub fn frontier(edges: &Vec<(u64, u64)>, remaining: &Vec<u64>) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < remaining.len() {
        let vertex = remaining[i];
        let incoming = predecessors(edges, vertex);
        if ready(&incoming, remaining) {
            out.push(vertex);
        }
        i += 1;
    }
    out
}

/// Linear frontier scan over caller-maintained indegrees. Every index in
/// `remaining` must address `indegree`; maintaining these counts as the number
/// of remaining incoming edges is the native adapter's invariant.
pub fn frontier_counts(indegree: &Vec<usize>, remaining: &Vec<usize>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < remaining.len() {
        let vertex = remaining[i];
        if indegree[vertex] == 0 {
            out.push(vertex);
        }
        i += 1;
    }
    out
}
