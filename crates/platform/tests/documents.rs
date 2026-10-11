use o5s_kernel::model::{Anchor, NodeId, Operation, OperationId};
use o5s_platform::{
    Actor, Command, Database,
    documents::{GraphEdit, identity},
};
use serde_json::Value;

fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn create() -> Database {
    let mut db = Database::default();
    db.execute(&actor(1), Command::Create { repo: "r".into() })
        .unwrap();
    db
}
fn propose(db: &mut Database, deps: Vec<u64>, operations: Vec<Operation>) -> Value {
    db.execute(
        &actor(1),
        Command::ProposeGraph {
            repo: "r".into(),
            intent: "edit".into(),
            dependencies: deps,
            edits: vec![GraphEdit {
                path: "file".into(),
                operations,
            }],
        },
    )
    .unwrap()
}
fn admit(db: &mut Database, id: u64) {
    db.execute(
        &actor(2),
        Command::Approve {
            repo: "r".into(),
            patch: id,
        },
    )
    .unwrap();
    db.execute(
        &actor(3),
        Command::Check {
            repo: "r".into(),
            patch: id,
            passed: true,
        },
    )
    .unwrap();
    db.execute(
        &actor(2),
        Command::Merge {
            repo: "r".into(),
            channel: "main".into(),
            patch: id,
        },
    )
    .unwrap();
}
fn node(reply: &Value, ordinal: u32) -> NodeId {
    NodeId {
        insertion: OperationId {
            patch: identity(reply["hash"].as_str().unwrap()).unwrap(),
            ordinal: 0,
        },
        ordinal,
    }
}
fn insert(left: Anchor, right: Anchor, text: &[&str]) -> Operation {
    Operation::Insert {
        left,
        right,
        atoms: text.iter().map(|s| s.as_bytes().to_vec()).collect(),
    }
}

#[test]
fn graph_operations_are_admitted_rendered_and_durably_serialized() {
    let mut db = create();
    let base = propose(
        &mut db,
        vec![],
        vec![insert(Anchor::Start, Anchor::End, &["a\n", "b\n", "c\n"])],
    );
    admit(&mut db, base["patch"].as_u64().unwrap());
    let delete = propose(
        &mut db,
        vec![1],
        vec![Operation::Delete {
            targets: vec![node(&base, 1)],
        }],
    );
    admit(&mut db, delete["patch"].as_u64().unwrap());
    assert_eq!(
        db.repo(&actor(1), "r")
            .unwrap()
            .materialize("main")
            .unwrap()
            .files["file"],
        "a\nc\n"
    );
    let restore = propose(
        &mut db,
        vec![2],
        vec![Operation::Restore {
            deletion: OperationId {
                patch: identity(delete["hash"].as_str().unwrap()).unwrap(),
                ordinal: 0,
            },
        }],
    );
    admit(&mut db, restore["patch"].as_u64().unwrap());
    let encoded = serde_json::to_vec(&db).unwrap();
    let reopened: Database = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        reopened
            .repo(&actor(1), "r")
            .unwrap()
            .materialize("main")
            .unwrap()
            .files["file"],
        "a\nb\nc\n"
    );
}

#[test]
fn concurrent_order_is_explicit_and_resolution_does_not_hide_late_arrivals() {
    let mut db = create();
    let base = propose(
        &mut db,
        vec![],
        vec![insert(Anchor::Start, Anchor::End, &["a\n", "b\n"])],
    );
    admit(&mut db, 1);
    let left = Anchor::Node(node(&base, 0));
    let right = Anchor::Node(node(&base, 1));
    let x = propose(&mut db, vec![1], vec![insert(left, right, &["x\n"])]);
    let y = propose(&mut db, vec![1], vec![insert(left, right, &["y\n"])]);
    admit(&mut db, 3);
    admit(&mut db, 2);
    let repo = db.repo(&actor(1), "r").unwrap();
    assert!(
        repo.materialize("main")
            .unwrap()
            .conflicts
            .contains_key("file")
    );
    let resolve = propose(
        &mut db,
        vec![2, 3],
        vec![Operation::Order {
            before: node(&x, 0),
            after: node(&y, 0),
        }],
    );
    admit(&mut db, resolve["patch"].as_u64().unwrap());
    assert_eq!(
        db.repo(&actor(1), "r")
            .unwrap()
            .materialize("main")
            .unwrap()
            .files["file"],
        "a\nx\ny\nb\n"
    );
    let late = propose(&mut db, vec![1], vec![insert(left, right, &["late\n"])]);
    admit(&mut db, late["patch"].as_u64().unwrap());
    let view = db
        .repo(&actor(1), "r")
        .unwrap()
        .materialize("main")
        .unwrap();
    assert!(!view.files.contains_key("file"));
    assert!(view.conflicts.contains_key("file"));
}

#[test]
fn unobserved_anchors_and_legacy_mix_are_refused_without_mutation() {
    let mut db = create();
    let base = propose(
        &mut db,
        vec![],
        vec![insert(Anchor::Start, Anchor::End, &["a\n"])],
    );
    let before = serde_json::to_value(&db).unwrap();
    let result = db.execute(
        &actor(1),
        Command::ProposeGraph {
            repo: "r".into(),
            intent: "bad".into(),
            dependencies: vec![],
            edits: vec![GraphEdit {
                path: "file".into(),
                operations: vec![insert(Anchor::Node(node(&base, 0)), Anchor::End, &["x"])],
            }],
        },
    );
    assert!(result.is_err());
    assert_eq!(before, serde_json::to_value(&db).unwrap());
    assert!(
        db.execute(
            &actor(1),
            Command::Propose {
                repo: "r".into(),
                intent: "bad".into(),
                dependencies: vec![1],
                edits: vec![o5s_platform::Edit {
                    path: "file".into(),
                    content: Some("replacement".into())
                }]
            }
        )
        .is_err()
    );
    assert_eq!(before, serde_json::to_value(&db).unwrap());
}
