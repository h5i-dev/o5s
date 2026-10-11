use o5s_platform::projection::*;
use std::collections::BTreeSet;
#[test]
fn every_three_vertex_graph_and_liveness_preserves_reachability() {
    for mask in 0..512 {
        let edges: Vec<_> = (0..9)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| (i / 3, i % 3))
            .collect();
        for live in 0..8 {
            let vertices: Vec<_> = (0..3).map(|i| (i, live & (1 << i) != 0)).collect();
            let mut p = Projection::default();
            p.apply(&vertices, &edges);
            for &(u, alive) in &vertices {
                if !alive {
                    continue;
                }
                let original = o5s_kernel::graph::reachable(&edges, &vec![u]);
                let projected = o5s_kernel::graph::reachable(&p.summary_edges(), &vec![u]);
                let expected: BTreeSet<_> = original
                    .into_iter()
                    .filter(|v| live & (1 << v) != 0)
                    .collect();
                assert_eq!(
                    projected.into_iter().collect::<BTreeSet<_>>(),
                    expected,
                    "mask={mask} live={live} u={u}"
                );
                assert_eq!(p.live_reachable(u), expected);
            }
        }
    }
}
#[test]
fn updates_only_rebuild_touched_components_and_dead_edge_insertions_are_safe() {
    let mut p = Projection::default();
    p.apply(
        &[(0, true), (1, false), (2, true), (10, true), (11, true)],
        &[(0, 1), (10, 11)],
    );
    let work = p.apply(&[], &[(1, 2)]);
    assert_eq!(work.component_vertices, 3);
    assert_eq!(work.rebuilt_sources, 2);
    assert_eq!(p.live_reachable(0), BTreeSet::from([0, 2]));
    assert_eq!(p.live_reachable(10), BTreeSet::from([10, 11]));
    assert_eq!(p.apply(&[], &[(1, 2)]), ProjectionWork::default());
    p.apply(&[(1, true)], &[]);
    assert_eq!(p.live_reachable(0), BTreeSet::from([0, 1, 2]));
    p.apply(&[(1, false), (2, false)], &[]);
    assert_eq!(p.live_reachable(0), BTreeSet::from([0]));
}
#[test]
fn long_dead_chain_has_constant_size_live_summary() {
    let mut p = Projection::default();
    let vertices: Vec<_> = (0..10_000).map(|i| (i, i == 0 || i == 9_999)).collect();
    let edges: Vec<_> = (0..9_999).map(|i| (i, i + 1)).collect();
    let work = p.apply(&vertices, &edges);
    assert_eq!(work.rebuilt_sources, 2);
    assert_eq!(p.summary_edges(), vec![(0, 9_999)]);
    assert_eq!(p.live_reachable(0), BTreeSet::from([0, 9_999]));
}
