use o5s_platform::{Actor, Command};
use o5s_server::Store;
use serde_json::json;

#[test]
fn synced_temporary_snapshot_does_not_publish_when_rename_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let a = Actor { tenant: 1, user: 1 };
    let command = |v| serde_json::from_value::<Command>(v).unwrap();
    store
        .execute(&a, command(json!({"cmd":"create","repo":"alpha"})))
        .unwrap();
    store
        .execute(
            &a,
            command(json!({"cmd":"propose","repo":"alpha","intent":"fixture",
        "dependencies":[],"edits":[{"path":"file","content":"body"}]})),
        )
        .unwrap();
    let before = serde_json::to_value(&store.db).unwrap();
    let state = dir.path().join("state.json");
    let saved = dir.path().join("saved.json");
    std::fs::rename(&state, &saved).unwrap();
    std::fs::create_dir(&state).unwrap();
    let approval = command(json!({"cmd":"approve","repo":"alpha","patch":1}));
    assert!(
        store
            .execute(&Actor { tenant: 1, user: 2 }, approval.clone())
            .is_err()
    );
    assert_eq!(serde_json::to_value(&store.db).unwrap(), before);
    let staged: o5s_platform::Database =
        serde_json::from_slice(&std::fs::read(dir.path().join("state.json.tmp")).unwrap()).unwrap();
    assert!(
        staged.tenants[&1]["alpha"].patches[&1]
            .approvals
            .contains(&2)
    );
    std::fs::remove_dir(&state).unwrap();
    std::fs::rename(&saved, &state).unwrap();
    store
        .execute(&Actor { tenant: 1, user: 2 }, approval)
        .unwrap();
    let expected = serde_json::to_value(&store.db).unwrap();
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(serde_json::to_value(&reopened.db).unwrap(), expected);
    assert_eq!(
        reopened.db.tenants[&1]["alpha"]
            .events
            .iter()
            .filter(|event| event.action == "approve")
            .count(),
        1
    );
}
