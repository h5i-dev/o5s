use o5s_platform::{Actor, Command, Database};
use serde_json::json;

fn command(v: serde_json::Value) -> Command {
    serde_json::from_value(v).unwrap()
}

#[test]
fn stale_views_never_move_an_exported_ref_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Database::default();
    let a = |user| Actor { tenant: 1, user };
    db.execute(&a(1), command(json!({"cmd":"create","repo":"r"})))
        .unwrap();
    let admit = |db: &mut Database, text: &str, deps: Vec<u64>| {
        let id = db
            .execute(
                &a(1),
                command(json!({"cmd":"propose","repo":"r","intent":text,
                "dependencies":deps,"edits":[{"path":"f","content":text}]})),
            )
            .unwrap()["patch"]
            .as_u64()
            .unwrap();
        db.execute(
            &a(2),
            command(json!({"cmd":"approve","repo":"r","patch":id})),
        )
        .unwrap();
        db.execute(
            &a(3),
            command(json!({"cmd":"check","repo":"r","patch":id,"passed":true})),
        )
        .unwrap();
        db.execute(
            &a(2),
            command(json!({"cmd":"merge","repo":"r","channel":"main","patch":id})),
        )
        .unwrap();
        id
    };
    let first = admit(&mut db, "one", vec![]);
    let old = db.repo(&a(1), "r").unwrap().clone();
    admit(&mut db, "two", vec![first]);
    let new = db.repo(&a(1), "r").unwrap().clone();
    let fresh = o5s_server::artifacts::export(dir.path(), 1, "r", "main", &new).unwrap();
    let stale = o5s_server::artifacts::export(dir.path(), 1, "r", "main", &old).unwrap();
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["commit"], fresh["commit"]);
    let again = o5s_server::artifacts::export(dir.path(), 1, "r", "main", &new).unwrap();
    assert_eq!(again["commit"], fresh["commit"]);
}
