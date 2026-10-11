use o5s_platform::*;
fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn create() -> Database {
    let mut d = Database::default();
    d.execute(
        &actor(1),
        Command::Create {
            repo: "project".into(),
        },
    )
    .unwrap();
    d
}
fn propose(d: &mut Database, user: u64, deps: Vec<u64>, path: &str, content: Option<&str>) -> u64 {
    d.execute(
        &actor(user),
        Command::Propose {
            repo: "project".into(),
            intent: format!("agent {user}"),
            dependencies: deps,
            edits: vec![Edit {
                path: path.into(),
                content: content.map(str::to_owned),
            }],
        },
    )
    .unwrap()["patch"]
        .as_u64()
        .unwrap()
}
fn ready(d: &mut Database, id: u64) {
    d.execute(
        &actor(100_001),
        Command::Approve {
            repo: "project".into(),
            patch: id,
        },
    )
    .unwrap();
    d.execute(
        &actor(100_002),
        Command::Check {
            repo: "project".into(),
            patch: id,
            passed: true,
        },
    )
    .unwrap();
}
fn merge(d: &mut Database, id: u64, channel: &str) -> Result<serde_json::Value, Refusal> {
    d.execute(
        &actor(1),
        Command::Merge {
            repo: "project".into(),
            channel: channel.into(),
            patch: id,
        },
    )
}
fn view(d: &Database, channel: &str) -> View {
    d.repo(&actor(1), "project")
        .unwrap()
        .materialize(channel)
        .unwrap()
}
#[test]
fn conflict_resolution_is_a_patch_and_survives_delivery_order() {
    let mut d = create();
    let base = propose(&mut d, 1, vec![], "src/app.rs", Some("base"));
    ready(&mut d, base);
    merge(&mut d, base, "main").unwrap();
    d.execute(
        &actor(2),
        Command::Fork {
            repo: "project".into(),
            source: "main".into(),
            channel: "reverse".into(),
        },
    )
    .unwrap();
    let a = propose(&mut d, 1, vec![base], "src/app.rs", Some("Alice"));
    let b = propose(&mut d, 2, vec![base], "src/app.rs", Some("Bob"));
    ready(&mut d, a);
    ready(&mut d, b);
    for (channel, ids) in [("main", vec![a, b]), ("reverse", vec![b, a])] {
        for id in ids {
            merge(&mut d, id, channel).unwrap();
        }
    }
    assert_eq!(view(&d, "main"), view(&d, "reverse"));
    assert_eq!(view(&d, "main").conflicts["src/app.rs"].len(), 2);
    let resolution = propose(&mut d, 3, vec![a, b], "src/app.rs", Some("Alice + Bob"));
    ready(&mut d, resolution);
    for c in ["main", "reverse"] {
        merge(&mut d, resolution, c).unwrap();
        merge(&mut d, resolution, c).unwrap();
    }
    assert_eq!(view(&d, "main"), view(&d, "reverse"));
    assert_eq!(view(&d, "main").files["src/app.rs"], "Alice + Bob");
    assert!(view(&d, "main").conflicts.is_empty());
}
#[test]
fn stacks_require_dependency_closure_and_reviews_bind_to_immutable_records() {
    let mut d = create();
    let a = propose(&mut d, 1, vec![], "a", Some("a"));
    let b = propose(&mut d, 1, vec![a], "b", Some("b"));
    assert!(
        merge(&mut d, a, "main")
            .unwrap_err()
            .0
            .contains("unapproved")
    );
    assert!(
        d.execute(
            &actor(1),
            Command::Approve {
                repo: "project".into(),
                patch: a
            }
        )
        .is_err()
    );
    ready(&mut d, a);
    ready(&mut d, b);
    assert!(
        merge(&mut d, b, "main")
            .unwrap_err()
            .0
            .contains("dependency")
    );
    let hash = d.repo(&actor(1), "project").unwrap().patches[&a]
        .hash
        .clone();
    merge(&mut d, a, "main").unwrap();
    merge(&mut d, b, "main").unwrap();
    assert_eq!(d.repo(&actor(1), "project").unwrap().patches[&a].hash, hash);
    assert_eq!(propose(&mut d, 1, vec![], "a", Some("a")), a);
    let changed = propose(&mut d, 1, vec![], "a", Some("different"));
    assert!(
        d.repo(&actor(1), "project").unwrap().patches[&changed]
            .approvals
            .is_empty()
    );
}
#[test]
fn deletes_and_prefix_collisions_never_silently_overwrite() {
    let mut d = create();
    let a = propose(&mut d, 1, vec![], "a", Some("file"));
    ready(&mut d, a);
    merge(&mut d, a, "main").unwrap();
    let b = propose(&mut d, 2, vec![], "a/b", Some("nested"));
    ready(&mut d, b);
    merge(&mut d, b, "main").unwrap();
    assert_eq!(view(&d, "main").conflicts.len(), 2);
    assert_eq!(view(&d, "main").conflicts["a"][0].patch, a);
    let delete = propose(&mut d, 3, vec![a, b], "a", None);
    ready(&mut d, delete);
    merge(&mut d, delete, "main").unwrap();
    assert!(view(&d, "main").conflicts.is_empty());
    assert_eq!(view(&d, "main").files["a/b"], "nested");
    let concurrent = propose(&mut d, 4, vec![a], "a", Some("new"));
    ready(&mut d, concurrent);
    merge(&mut d, concurrent, "main").unwrap();
    assert!(
        view(&d, "main").conflicts["a"]
            .iter()
            .any(|a| a.content.is_none())
    );
}
#[test]
fn tenant_isolation_and_path_validation() {
    let d = create();
    assert!(d.repo(&Actor { tenant: 2, user: 1 }, "project").is_err());
    for path in [
        "../x",
        "/x",
        "a/../b",
        ".git/config",
        ".GIT/config",
        "a\\b",
        "a\0b",
        "a//b",
        "",
    ] {
        assert!(!valid_path(path), "{path:?}");
    }
    assert!(valid_path("src/日本語.rs"));
}
#[test]
fn ten_thousand_agent_patches_materialize_without_losing_conflicts() {
    let start = std::time::Instant::now();
    let mut d = create();
    for user in 1..=10_000 {
        let id = propose(
            &mut d,
            user,
            vec![],
            "shared.txt",
            Some(&format!("agent {user}")),
        );
        ready(&mut d, id);
        merge(&mut d, id, "main").unwrap();
    }
    let v = view(&d, "main");
    assert_eq!(v.patches.len(), 10_000);
    assert_eq!(v.conflicts["shared.txt"].len(), 10_000);
    eprintln!(
        "10,000 proposal/review/check/merge + materialization: {:?}",
        start.elapsed()
    );
}

#[test]
fn batch_admission_is_atomic_and_topologically_orders_dependencies() {
    let mut d = create();
    let a = propose(&mut d, 1, vec![], "a", Some("base"));
    let b = propose(&mut d, 2, vec![a], "b", Some("dependent"));
    ready(&mut d, a);
    let before = serde_json::to_value(&d).unwrap();
    assert!(
        d.execute(
            &actor(1),
            Command::MergeBatch {
                repo: "project".into(),
                channel: "main".into(),
                patches: vec![b, a]
            }
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&d).unwrap(), before);
    ready(&mut d, b);
    d.execute(
        &actor(1),
        Command::MergeBatch {
            repo: "project".into(),
            channel: "main".into(),
            patches: vec![b, a, a],
        },
    )
    .unwrap();
    assert_eq!(view(&d, "main").patches, vec![a, b]);
}
