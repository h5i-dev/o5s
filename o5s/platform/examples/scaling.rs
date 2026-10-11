use o5s_platform::{Edit, IncrementalView, Patch, Repository, projection::Projection};
use std::{collections::BTreeSet, time::Instant};

fn append(repo: &mut Repository, id: u64, dependencies: Vec<u64>) {
    repo.patches.insert(
        id,
        Patch {
            id,
            hash: format!("{id:064x}"),
            author: id,
            intent: "scaling fixture".into(),
            dependencies,
            edits: vec![Edit {
                path: "file".into(),
                content: Some(id.to_string()),
            }],
            line_edits: vec![],
            approvals: BTreeSet::new(),
            checks_passed: true,
        },
    );
    repo.channels.entry("main".into()).or_default().push(id);
}

fn main() {
    for chained in [false, true] {
        let mut repo = Repository::default();
        for id in 1..=10_000 {
            append(
                &mut repo,
                id,
                if chained && id > 1 {
                    vec![id - 1]
                } else {
                    vec![]
                },
            );
        }
        let mut cache = IncrementalView::default();
        let start = Instant::now();
        let work = cache.sync_append_only(&repo, "main").unwrap();
        println!(
            "{}: {work:?}, update={:?}",
            if chained { "chain" } else { "independent" },
            start.elapsed()
        );
        println!(
            "unchanged: {:?}",
            cache.sync_append_only(&repo, "main").unwrap()
        );
        if chained {
            append(&mut repo, 10_001, vec![10_000]);
            println!(
                "append: {:?}",
                cache.sync_append_only(&repo, "main").unwrap()
            );
        }
        let start = Instant::now();
        let view = cache.content().unwrap();
        println!(
            "render={:?}, files={}, conflicts={}",
            start.elapsed(),
            view.files.len(),
            view.conflicts.len()
        );
    }
    let mut graph = Projection::default();
    let vertices: Vec<_> = (0..10_000).map(|i| (i, i == 0 || i == 9_999)).collect();
    let edges: Vec<_> = (0..9_999).map(|i| (i, i + 1)).collect();
    let start = Instant::now();
    let work = graph.apply(&vertices, &edges);
    println!(
        "dead-chain summary: {work:?}, build={:?}, edges={:?}, reachable={:?}",
        start.elapsed(),
        graph.summary_edges(),
        graph.live_reachable(0)
    );
    let rows = vec![Vec::new(); 100_000];
    let mut marks = vec![false; rows.len()];
    let allocation = marks.as_ptr();
    let seeds = vec![99_999];
    let start = Instant::now();
    let mut visited = 0;
    for _ in 0..10_000 {
        let search = o5s_kernel::indexed::reachable(&rows, &seeds, marks);
        assert_eq!(search.marks.as_ptr(), allocation);
        visited += search.queue.len();
        marks = search.marks;
    }
    println!(
        "indexed isolated component: vertices={}, searches=10000, visited={visited}, reused_marks=true, elapsed={:?}",
        rows.len(),
        start.elapsed()
    );
}
