use o5s_platform::{Actor, Command};
use o5s_server::Store;
use serde_json::json;

fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn command(value: serde_json::Value) -> Command {
    serde_json::from_value(value).unwrap()
}
fn snapshot(store: &Store) -> serde_json::Value {
    serde_json::to_value(&store.db).unwrap()
}
fn propose(store: &mut Store, name: &str) -> u64 {
    store
        .execute(
            &actor(1),
            command(json!({"cmd":"propose", "repo":name,
        "intent":"fixture", "dependencies":[], "edits":[{"path":"file.txt","content":"body"}]})),
        )
        .unwrap()["patch"]
        .as_u64()
        .unwrap()
}

#[test]
fn failed_fact_writes_restore_memory_disk_and_event_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    for repo in ["alpha", "unrelated"] {
        store
            .execute(&actor(1), command(json!({"cmd":"create","repo":repo})))
            .unwrap();
        propose(&mut store, repo);
    }
    store
        .execute(
            &actor(2),
            command(json!({"cmd":"approve","repo":"alpha","patch":1})),
        )
        .unwrap();
    store
        .execute(
            &actor(3),
            command(json!({"cmd":"check","repo":"alpha","patch":1,"passed":true})),
        )
        .unwrap();
    let before = snapshot(&store);
    let disk = std::fs::read(dir.path().join("state.json")).unwrap();
    // A directory at the temporary snapshot path forces failure before rename.
    std::fs::create_dir(dir.path().join("state.json.tmp")).unwrap();
    for value in [
        json!({"cmd":"approve","repo":"alpha","patch":1}),
        json!({"cmd":"check","repo":"alpha","patch":1,"passed":false}),
    ] {
        let error = store.execute(&actor(4), command(value)).unwrap_err();
        assert!(error.0.starts_with("storage unavailable:"));
        assert_eq!(snapshot(&store), before);
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), disk);
    }
    drop(store);
    assert_eq!(snapshot(&Store::open(dir.path()).unwrap()), before);
}

#[test]
fn failed_repository_writes_and_batch_refusal_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store
        .execute(&actor(1), command(json!({"cmd":"create","repo":"alpha"})))
        .unwrap();
    let first = propose(&mut store, "alpha");
    let second = store
        .execute(
            &actor(1),
            command(json!({"cmd":"propose","repo":"alpha",
        "intent":"unreviewed fixture","dependencies":[],
        "edits":[{"path":"other.txt","content":"different"}]})),
        )
        .unwrap()["patch"]
        .as_u64()
        .unwrap();
    store
        .execute(
            &actor(2),
            command(json!({"cmd":"approve","repo":"alpha","patch":first})),
        )
        .unwrap();
    store
        .execute(
            &actor(3),
            command(json!({"cmd":"check","repo":"alpha","patch":first,"passed":true})),
        )
        .unwrap();
    let before = snapshot(&store);
    let disk = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .execute(
                &actor(2),
                command(json!({"cmd":"merge_batch","repo":"alpha",
        "channel":"main","patches":[first,second]}))
            )
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), disk);
    std::fs::create_dir(dir.path().join("state.json.tmp")).unwrap();
    for (a, value) in [
        (
            actor(1),
            json!({"cmd":"propose","repo":"alpha","intent":"failed new patch",
            "dependencies":[],"edits":[{"path":"failed.txt","content":"not committed"}]}),
        ),
        (
            actor(1),
            json!({"cmd":"propose_lines","repo":"alpha","intent":"failed line patch",
            "dependencies":[],"edits":[{"path":"file.txt","base":first,"start":0,"end":1,"content":"new"}]}),
        ),
        (
            actor(2),
            json!({"cmd":"fork","repo":"alpha","source":"main","channel":"new"}),
        ),
        (
            actor(2),
            json!({"cmd":"merge","repo":"alpha","channel":"main","patch":first}),
        ),
        (
            Actor { tenant: 9, user: 1 },
            json!({"cmd":"create","repo":"newtenant"}),
        ),
    ] {
        assert!(store.execute(&a, command(value)).is_err());
        assert_eq!(snapshot(&store), before);
    }
    // An already registered canonical patch succeeds without rewriting storage.
    assert_eq!(propose(&mut store, "alpha"), first);
    assert_eq!(snapshot(&store), before);
    drop(store);
    assert_eq!(snapshot(&Store::open(dir.path()).unwrap()), before);
}

#[test]
fn successful_fact_updates_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store
        .execute(&actor(1), command(json!({"cmd":"create","repo":"alpha"})))
        .unwrap();
    let patch = propose(&mut store, "alpha");
    store
        .execute(
            &actor(2),
            command(json!({"cmd":"approve","repo":"alpha","patch":patch})),
        )
        .unwrap();
    store
        .execute(
            &actor(3),
            command(json!({"cmd":"check","repo":"alpha","patch":patch,"passed":true})),
        )
        .unwrap();
    let before = snapshot(&store);
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(snapshot(&reopened), before);
    let p = &reopened.db.tenants[&1]["alpha"].patches[&patch];
    assert!(p.approvals.contains(&2));
    assert!(p.checks_passed);
}
