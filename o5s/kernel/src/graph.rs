//! Verified reachability primitive for original and live-boundary graphs.
//! The summary constructor is outside this kernel's proof boundary.
pub fn reachable(edges: &Vec<(u64, u64)>, start: &Vec<u64>) -> Vec<u64> {
    h5i_app_std::graph::reachable(edges, start)
}
