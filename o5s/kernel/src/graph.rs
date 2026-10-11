//! Verified reachability primitive for original and live-boundary graphs.
//! Live-boundary construction stays in the extractable Rust subset.
pub fn reachable(edges: &Vec<(u64, u64)>, start: &Vec<u64>) -> Vec<u64> {
    h5i_app_std::graph::reachable(edges, start)
}

/// Keep edges whose source is dead. Once a search reaches a live boundary it
/// cannot pass through it; subsequent live-to-live walks use another row.
pub fn dead_edges(edges: &Vec<(u64, u64)>, live: &Vec<u64>) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < edges.len() {
        if !h5i_app_std::set::contains(live, &edges[i].0) {
            out.push(edges[i]);
        }
        i += 1;
    }
    out
}

pub fn successors(edges: &Vec<(u64, u64)>, source: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < edges.len() {
        if edges[i].0 == source {
            out.push(edges[i].1);
        }
        i += 1;
    }
    out
}

pub fn keep_live(nodes: &Vec<u64>, live: &Vec<u64>) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        if h5i_app_std::set::contains(live, &nodes[i]) {
            out.push(nodes[i]);
        }
        i += 1;
    }
    out
}

/// Exact nonempty dead-interior walks from one source to live boundaries.
/// Starting from direct successors avoids inventing reflexive summary edges.
pub fn boundary(edges: &Vec<(u64, u64)>, live: &Vec<u64>, source: u64) -> Vec<u64> {
    let dead = dead_edges(edges, live);
    let start = successors(edges, source);
    let reached = reachable(&dead, &start);
    keep_live(&reached, live)
}

/// Row-compressed exact summary; row i belongs to live[i]. Keeping rows avoids
/// an unchecked multiplication of vertex and edge counts for a flat Vec.
pub fn summary_rows(edges: &Vec<(u64, u64)>, live: &Vec<u64>) -> Vec<Vec<u64>> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < live.len() {
        rows.push(boundary(edges, live, live[i]));
        i += 1;
    }
    rows
}

/// Reference weak-component construction. The optimized native index is tested
/// against this extracted implementation; both directions use original edges.
pub fn weak_edges(edges: &Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    let mut out = edges.clone();
    let mut i = 0;
    while i < edges.len() {
        out.push((edges[i].1, edges[i].0));
        i += 1;
    }
    out
}

pub fn weak_component(edges: &Vec<(u64, u64)>, seeds: &Vec<u64>) -> Vec<u64> {
    reachable(&weak_edges(edges), seeds)
}
