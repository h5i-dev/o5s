use o5s_platform::*;
use std::collections::BTreeSet;
fn add(r: &mut Repository, id: u64, dependencies: Vec<u64>, path: &str, content: Option<String>) {
    r.patches.insert(
        id,
        Patch {
            id,
            hash: format!("{id:064x}"),
            author: id,
            intent: "test".into(),
            dependencies,
            edits: vec![Edit {
                path: path.into(),
                content,
            }],
            line_edits: vec![],
            approvals: BTreeSet::new(),
            checks_passed: true,
        },
    );
    r.channels.entry("main".into()).or_default().push(id);
}
#[test]
fn long_history_updates_one_register_without_ancestry_table() {
    let mut r = Repository::default();
    for id in 1..=10_000 {
        add(
            &mut r,
            id,
            if id == 1 { vec![] } else { vec![id - 1] },
            "a",
            Some(id.to_string()),
        );
    }
    let mut c = IncrementalView::default();
    let work = c.sync_append_only(&r, "main").unwrap();
    assert_eq!(work.processed_patches, 10_000);
    assert_eq!(work.dependency_edges, 9_999);
    assert_eq!(work.rendered_paths, 1);
    assert_eq!(c.view().unwrap().files["a"], "10000");
    assert_eq!(c.sync_append_only(&r, "main").unwrap(), ViewWork::default());
    add(&mut r, 10_001, vec![10_000], "a", Some("next".into()));
    assert_eq!(
        c.sync_append_only(&r, "main").unwrap(),
        ViewWork {
            processed_patches: 1,
            dependency_edges: 1,
            updated_paths: 1,
            rendered_paths: 1
        }
    );
    add(
        &mut r,
        10_002,
        vec![10_001],
        "other",
        Some("untouched a".into()),
    );
    assert_eq!(c.sync_append_only(&r, "main").unwrap().dependency_edges, 0);
    assert_eq!(c.view().unwrap().files["a"], "next");
}
#[test]
fn differential_causal_dags_tombstones_and_structural_conflicts() {
    for seed in 0..100u64 {
        let mut n = seed + 1;
        let mut r = Repository::default();
        let mut c = IncrementalView::default();
        for id in 1..=40 {
            n = n.wrapping_mul(6364136223846793005).wrapping_add(1);
            let deps = (1..id)
                .filter(|d| (n.rotate_left(*d as u32) & 15) == 0)
                .collect();
            let paths = ["a", "a/b", "b", "c"];
            add(
                &mut r,
                id,
                deps,
                paths[(n % 4) as usize],
                if n & 16 == 0 {
                    None
                } else {
                    Some((n % 7).to_string())
                },
            );
            c.sync(&r, "main").unwrap();
            assert_eq!(
                c.view().unwrap(),
                r.materialize_reference("main").unwrap(),
                "seed={seed} id={id}"
            );
        }
        r.channels.get_mut("main").unwrap().reverse();
        assert_eq!(c.sync(&r, "main").unwrap(), ViewWork::default());
        assert_eq!(
            r.materialize("main").unwrap(),
            r.materialize_reference("main").unwrap()
        );
        r.channels.insert("main".into(), vec![1]);
        c.sync(&r, "main").unwrap();
        assert_eq!(c.view().unwrap(), r.materialize_reference("main").unwrap());
    }
}

#[test]
fn output_rebuilds_only_changed_paths_and_related_structural_conflicts() {
    let mut repo = Repository::default();
    let mut cache = IncrementalView::default();
    add(&mut repo, 1, vec![], "a", Some("parent".into()));
    add(&mut repo, 2, vec![], "z", Some("unrelated".into()));
    assert_eq!(
        cache
            .sync_append_only(&repo, "main")
            .unwrap()
            .rendered_paths,
        2
    );
    assert_eq!(
        cache
            .sync_append_only(&repo, "main")
            .unwrap()
            .rendered_paths,
        0
    );
    add(&mut repo, 3, vec![], "a/b", Some("child".into()));
    assert_eq!(
        cache
            .sync_append_only(&repo, "main")
            .unwrap()
            .rendered_paths,
        2
    );
    let view = cache.content().unwrap();
    assert_eq!(view.conflicts.len(), 2);
    assert_eq!(view.files["z"], "unrelated");
    add(&mut repo, 4, vec![3], "a/b", None);
    assert_eq!(
        cache
            .sync_append_only(&repo, "main")
            .unwrap()
            .rendered_paths,
        2
    );
    let view = cache.content().unwrap();
    assert!(view.conflicts.is_empty());
    assert_eq!(view.files["a"], "parent");
}
