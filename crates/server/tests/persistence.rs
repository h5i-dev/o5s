use o5s_platform::{Actor, Command};
use o5s_server::{Failure, Store, legacy};
use serde_json::json;
use std::io::Write;

fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn command(value: serde_json::Value) -> Command {
    serde_json::from_value(value).unwrap()
}
fn snapshot(store: &Store) -> serde_json::Value {
    serde_json::to_value(&store.db).unwrap()
}
fn log(dir: &std::path::Path) -> Vec<u8> {
    std::fs::read(dir.join("registry.log")).unwrap()
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
fn fixture(dir: &std::path::Path) -> (Store, u64, u64) {
    let mut store = Store::open(dir).unwrap();
    for repo in ["alpha", "unrelated"] {
        store
            .execute(&actor(1), command(json!({"cmd":"create","repo":repo})))
            .unwrap();
        propose(&mut store, repo);
    }
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
            command(json!({"cmd":"approve","repo":"alpha","patch":1})),
        )
        .unwrap();
    store
        .execute(
            &actor(3),
            command(json!({"cmd":"check","repo":"alpha","patch":1,"passed":true})),
        )
        .unwrap();
    (store, 1, second)
}

#[test]
fn failed_appends_change_neither_memory_nor_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, first, _) = fixture(dir.path());
    let before = snapshot(&store);
    let disk = log(dir.path());
    for (a, value) in [
        (
            actor(4),
            json!({"cmd":"approve","repo":"alpha","patch":first}),
        ),
        (
            actor(4),
            json!({"cmd":"check","repo":"alpha","patch":first,"passed":false}),
        ),
        (
            actor(1),
            json!({"cmd":"propose_graph","repo":"alpha","intent":"failed graph patch",
            "dependencies":[],"edits":[{"path":"graph.txt","operations":[
                {"kind":"insert","left":"Start","right":"End","atoms":[[65,66]]}]}]}),
        ),
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
        store.inject_append_failure();
        let error = store.execute(&a, command(value)).unwrap_err();
        assert!(error.0.starts_with("storage unavailable:"), "{error}");
        assert_eq!(snapshot(&store), before);
        // The partial record was truncated away.
        assert_eq!(log(dir.path()), disk);
    }
    // An already registered canonical patch succeeds without writing.
    assert_eq!(propose(&mut store, "alpha"), first);
    assert_eq!(log(dir.path()), disk);
    drop(store);
    assert_eq!(snapshot(&Store::open(dir.path()).unwrap()), before);
}

#[test]
fn refused_batches_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, first, second) = fixture(dir.path());
    let before = snapshot(&store);
    let disk = log(dir.path());
    let error = store
        .execute(
            &actor(2),
            command(json!({"cmd":"merge_batch","repo":"alpha","channel":"main","patches":[first,second]})),
        )
        .unwrap_err();
    assert_eq!(error.0, "patch unapproved");
    assert_eq!(snapshot(&store), before);
    assert_eq!(log(dir.path()), disk);
}

#[test]
fn commits_survive_restart_and_torn_tails_are_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let (store, first, _) = fixture(dir.path());
    let before = snapshot(&store);
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(snapshot(&reopened), before);
    let p = &reopened.db.repo(&actor(1), "alpha").unwrap().patches[&first];
    assert!(p.approvals.contains(&2));
    assert!(p.checks_passed);
    drop(reopened);
    // A crash during append leaves an incomplete final record.
    let complete = log(dir.path());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("registry.log"))
        .unwrap();
    file.write_all(b"0123 {\"effects\":[").unwrap();
    drop(file);
    let mut recovered = Store::open(dir.path()).unwrap();
    assert_eq!(snapshot(&recovered), before);
    assert_eq!(log(dir.path()), complete);
    recovered
        .execute(
            &actor(2),
            command(json!({"cmd":"approve","repo":"unrelated","patch":1})),
        )
        .unwrap();
    let after = snapshot(&recovered);
    drop(recovered);
    assert_eq!(snapshot(&Store::open(dir.path()).unwrap()), after);
}

#[test]
fn corruption_before_the_tail_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    drop(fixture(dir.path()).0);
    let mut bytes = log(dir.path());
    let first_newline = bytes.iter().position(|b| *b == b'\n').unwrap();
    bytes[first_newline - 2] ^= 1;
    std::fs::write(dir.path().join("registry.log"), &bytes).unwrap();
    let error = Store::open(dir.path()).err().unwrap().to_string();
    assert!(error.contains("corrupt record"), "{error}");
}

#[test]
fn idempotency_keys_replay_and_reject_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let create = || command(json!({"cmd":"create","repo":"alpha"}));
    let first = store
        .execute_keyed(&actor(1), create(), Some("k1"))
        .unwrap();
    // The lost response is retrieved instead of failing as a duplicate.
    let again = store
        .execute_keyed(&actor(1), create(), Some("k1"))
        .unwrap();
    assert_eq!(first, again);
    assert!(matches!(
        store.execute_keyed(
            &actor(1),
            command(json!({"cmd":"create","repo":"beta"})),
            Some("k1")
        ),
        Err(Failure::Conflict(_))
    ));
    // Keys belong to one user.
    assert!(matches!(
        store.execute_keyed(&actor(2), create(), Some("k1")),
        Err(Failure::Refused(_))
    ));
    drop(store);
    let mut reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .execute_keyed(&actor(1), create(), Some("k1"))
            .unwrap(),
        first
    );
}

#[test]
fn legacy_snapshots_import_with_identities_and_membership() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = o5s_platform::Database::default();
    let a = actor(1);
    db.execute(&a, command(json!({"cmd":"create","repo":"alpha"})))
        .unwrap();
    let id = db
        .execute(
            &a,
            command(
                json!({"cmd":"propose","repo":"alpha","intent":"base","dependencies":[],
                "edits":[{"path":"a.txt","content":"one\ntwo\n"}]}),
            ),
        )
        .unwrap()["patch"]
        .as_u64()
        .unwrap();
    db.execute(
        &actor(2),
        command(json!({"cmd":"approve","repo":"alpha","patch":id})),
    )
    .unwrap();
    db.execute(
        &actor(3),
        command(json!({"cmd":"check","repo":"alpha","patch":id,"passed":true})),
    )
    .unwrap();
    db.execute(
        &actor(2),
        command(json!({"cmd":"merge","repo":"alpha","channel":"main","patch":id})),
    )
    .unwrap();
    db.execute(
        &actor(2),
        command(json!({"cmd":"fork","repo":"alpha","source":"main","channel":"side"})),
    )
    .unwrap();
    let line = db
        .execute(
            &a,
            command(
                json!({"cmd":"propose_lines","repo":"alpha","intent":"edit","dependencies":[],
                "edits":[{"path":"a.txt","base":id,"start":1,"end":2,"content":"TWO\n"}]}),
            ),
        )
        .unwrap()["patch"]
        .as_u64()
        .unwrap();
    // The pre-kernel snapshot shape: repositories by tenant and name.
    let repo = db.repo(&a, "alpha").unwrap();
    let legacy = json!({"tenants":{"1":{"alpha":{
        "patches": repo.patches, "channels": repo.channels, "events": repo.events}}}});
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("import-json")
    );
    legacy::import(dir.path()).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let imported = store.db.repo(&a, "alpha").unwrap();
    assert_eq!(imported.patches, repo.patches);
    assert_eq!(imported.channels, repo.channels);
    assert_eq!(imported.patches[&line].line_edits[0].base, id);
    assert_eq!(store.db.repositories(&a), vec!["alpha".to_string()]);
    // The imported registry keeps working: identity deduplication and review.
    let again = store
        .execute(
            &a,
            command(
                json!({"cmd":"propose","repo":"alpha","intent":"base","dependencies":[],
                "edits":[{"path":"a.txt","content":"one\ntwo\n"}]}),
            ),
        )
        .unwrap();
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["patch"], id);
    store
        .execute(
            &actor(2),
            command(json!({"cmd":"approve","repo":"alpha","patch":line})),
        )
        .unwrap();
    assert!(dir.path().join("state.json.imported").exists());
}

#[test]
fn live_backups_copy_complete_records_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, _) = fixture(dir.path());
    let before = snapshot(&store);
    // The server keeps running; a torn append is in progress.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("registry.log"))
        .unwrap();
    file.write_all(b"abc").unwrap();
    let out = dir.path().join("backup.log");
    let summary = o5s_server::local::backup(&dir.path().join("registry.log"), &out).unwrap();
    assert_eq!(summary.repositories, 2);
    assert_eq!(summary.torn, 0);
    assert_eq!(
        o5s_server::local::verify(&dir.path().join("registry.log"))
            .unwrap()
            .torn,
        3
    );
    drop(store);
    let restored = tempfile::tempdir().unwrap();
    std::fs::copy(&out, restored.path().join("registry.log")).unwrap();
    assert_eq!(snapshot(&Store::open(restored.path()).unwrap()), before);
}
