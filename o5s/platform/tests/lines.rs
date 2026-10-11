use o5s_platform::{lines::LineEdit, *};
fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn propose(db: &mut Database, base: u64, start: u64, end: u64, content: &str) -> u64 {
    let reply = db
        .execute(
            &actor(1),
            Command::ProposeLines {
                repo: "test".into(),
                intent: format!("edit {start} {end} {content}"),
                dependencies: vec![],
                edits: vec![LineEdit {
                    path: "a".into(),
                    base,
                    start,
                    end,
                    content: content.into(),
                }],
            },
        )
        .unwrap();
    reply["patch"].as_u64().unwrap()
}
fn integrate(db: &mut Database, id: u64, channel: &str) {
    db.execute(
        &actor(2),
        Command::Approve {
            repo: "test".into(),
            patch: id,
        },
    )
    .unwrap();
    db.execute(
        &actor(3),
        Command::Check {
            repo: "test".into(),
            patch: id,
            passed: true,
        },
    )
    .unwrap();
    db.execute(
        &actor(1),
        Command::Merge {
            repo: "test".into(),
            channel: channel.into(),
            patch: id,
        },
    )
    .unwrap();
}
fn fixture() -> Database {
    let mut db = Database::default();
    db.execute(
        &actor(1),
        Command::Create {
            repo: "test".into(),
        },
    )
    .unwrap();
    db.execute(
        &actor(1),
        Command::Propose {
            repo: "test".into(),
            intent: "base".into(),
            dependencies: vec![],
            edits: vec![Edit {
                path: "a".into(),
                content: Some("one\ntwo\nthree\nfour\n".into()),
            }],
        },
    )
    .unwrap();
    integrate(&mut db, 1, "main");
    db.execute(
        &actor(1),
        Command::Fork {
            repo: "test".into(),
            source: "main".into(),
            channel: "other".into(),
        },
    )
    .unwrap();
    db
}
#[test]
fn disjoint_same_file_changes_merge_in_either_order_and_match_replay() {
    let mut db = fixture();
    let a = propose(&mut db, 1, 0, 1, "ONE\n");
    let b = propose(&mut db, 1, 2, 3, "THREE\n");
    integrate(&mut db, a, "main");
    integrate(&mut db, b, "main");
    integrate(&mut db, b, "other");
    integrate(&mut db, a, "other");
    let r = db.repo(&actor(1), "test").unwrap();
    let v = r.materialize("main").unwrap();
    assert!(v.conflicts.is_empty());
    assert_eq!(v.files["a"], "ONE\ntwo\nTHREE\nfour\n");
    assert_eq!(v, r.materialize("other").unwrap());
    assert_eq!(v, r.materialize_reference("main").unwrap());
}
#[test]
fn overlap_delete_insert_remains_a_reviewable_conflict() {
    let mut db = fixture();
    let a = propose(&mut db, 1, 1, 2, "");
    let b = propose(&mut db, 1, 1, 1, "insert\n");
    integrate(&mut db, a, "main");
    integrate(&mut db, b, "main");
    let v = db
        .repo(&actor(1), "test")
        .unwrap()
        .materialize("main")
        .unwrap();
    assert_eq!(v.conflicts["a"].len(), 2);
    assert!(!v.files.contains_key("a"));
}
#[test]
fn edits_on_previous_line_snapshot_preserve_prior_content_and_reject_stale_base() {
    let mut db = fixture();
    let a = propose(&mut db, 1, 0, 1, "ONE\n");
    integrate(&mut db, a, "main");
    let b = propose(&mut db, a, 2, 3, "THREE\n");
    integrate(&mut db, b, "main");
    let r = db.repo(&actor(1), "test").unwrap();
    assert_eq!(
        r.materialize("main").unwrap().files["a"],
        "ONE\ntwo\nTHREE\nfour\n"
    );
    let before = serde_json::to_value(&db).unwrap();
    assert!(
        db.execute(
            &actor(1),
            Command::ProposeLines {
                repo: "test".into(),
                intent: "stale".into(),
                dependencies: vec![a],
                edits: vec![LineEdit {
                    path: "a".into(),
                    base: 1,
                    start: 3,
                    end: 4,
                    content: "FOUR\n".into()
                }]
            }
        )
        .is_err()
    );
    assert_eq!(before, serde_json::to_value(&db).unwrap());
}
#[test]
fn invalid_ranges_do_not_modify_registry_and_roundtrip_retains_identity() {
    let mut db = fixture();
    let before = serde_json::to_value(&db).unwrap();
    assert!(
        db.execute(
            &actor(1),
            Command::ProposeLines {
                repo: "test".into(),
                intent: "bad".into(),
                dependencies: vec![],
                edits: vec![LineEdit {
                    path: "a".into(),
                    base: 1,
                    start: 0,
                    end: 99,
                    content: "x".into()
                }]
            }
        )
        .is_err()
    );
    assert_eq!(before, serde_json::to_value(&db).unwrap());
    let a = propose(&mut db, 1, 0, 1, "ONE\n");
    let b = propose(&mut db, 1, 0, 1, "ONE\n");
    assert_eq!(a, b);
    integrate(&mut db, a, "main");
    let loaded: Database = serde_json::from_slice(&serde_json::to_vec(&db).unwrap()).unwrap();
    assert_eq!(
        db.repo(&actor(1), "test")
            .unwrap()
            .materialize("main")
            .unwrap(),
        loaded
            .repo(&actor(1), "test")
            .unwrap()
            .materialize("main")
            .unwrap()
    );
}
#[test]
fn unicode_crlf_empty_file_and_insert_at_end_preserve_exact_bytes() {
    for original in ["日本語\r\n二行目\r\n末尾", "", "no final newline"] {
        let mut db = Database::default();
        db.execute(
            &actor(1),
            Command::Create {
                repo: "test".into(),
            },
        )
        .unwrap();
        db.execute(
            &actor(1),
            Command::Propose {
                repo: "test".into(),
                intent: "base".into(),
                dependencies: vec![],
                edits: vec![Edit {
                    path: "a".into(),
                    content: Some(original.into()),
                }],
            },
        )
        .unwrap();
        integrate(&mut db, 1, "main");
        let n = original.split_inclusive('\n').count() as u64;
        let id = propose(&mut db, 1, n, n, "追加\n");
        integrate(&mut db, id, "main");
        assert_eq!(
            db.repo(&actor(1), "test")
                .unwrap()
                .materialize("main")
                .unwrap()
                .files["a"],
            format!("{original}追加\n")
        );
    }
}

#[test]
fn indexed_range_merge_matches_pairwise_spec_in_every_small_permutation() {
    use o5s_platform::lines::LineVersion;
    let ranges: Vec<_> = (0..=4).flat_map(|a| (a..=4).map(move |z| (a, z))).collect();
    for &(a, z) in &ranges {
        for &(b, w) in &ranges {
            for &(c, v) in &ranges {
                let versions: Vec<_> = [(a, z), (b, w), (c, v)]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (start, end))| LineVersion {
                        base: 1,
                        original: "a\nb\nc\nd\n".into(),
                        changes: vec![LineEdit {
                            path: "a".into(),
                            base: 1,
                            start,
                            end,
                            content: format!("{i}\n"),
                        }],
                    })
                    .collect();
                let conflict = |(a, z): (u64, u64), (b, w): (u64, u64)| {
                    if a == z {
                        b <= a && a <= w
                    } else if b == w {
                        a <= b && b <= z
                    } else {
                        a < w && b < z
                    }
                };
                let safe = !conflict((a, z), (b, w))
                    && !conflict((a, z), (c, v))
                    && !conflict((b, w), (c, v));
                let mut expected = None;
                for order in [
                    [0, 1, 2],
                    [0, 2, 1],
                    [1, 0, 2],
                    [1, 2, 0],
                    [2, 0, 1],
                    [2, 1, 0],
                ] {
                    let merged =
                        LineVersion::merge_all(order.into_iter().map(|i| Some(&versions[i])));
                    assert_eq!(merged.is_some(), safe, "{a}..{z}, {b}..{w}, {c}..{v}");
                    if let Some(merged) = merged {
                        let rendered = merged.render().unwrap();
                        if let Some(expected) = &expected {
                            assert_eq!(&rendered, expected);
                        } else {
                            expected = Some(rendered);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn ten_thousand_disjoint_line_agents_render_one_file_without_conflicts() {
    use o5s_platform::lines::LineVersion;
    let original = "old\n".repeat(10_000);
    let versions: Vec<_> = (0..10_000)
        .map(|n| LineVersion {
            base: 1,
            original: original.clone(),
            changes: vec![LineEdit {
                path: "a".into(),
                base: 1,
                start: n,
                end: n + 1,
                content: format!("agent {n}\n"),
            }],
        })
        .collect();
    let merged = LineVersion::merge_all(versions.iter().map(Some)).unwrap();
    assert_eq!(merged.changes.len(), 10_000);
    assert_eq!(
        merged.render().unwrap(),
        (0..10_000)
            .map(|n| format!("agent {n}\n"))
            .collect::<String>()
    );
}
