//! File modes, binary content, empty graph documents, removal and mixed proposals.
use o5s_platform::*;
use serde_json::json;

fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}
fn run(db: &mut Database, user: u64, v: serde_json::Value) -> Result<serde_json::Value, Refusal> {
    let mut v = v;
    v["repo"] = json!("r");
    db.execute(&actor(user), serde_json::from_value(v).unwrap())
}
fn patch(db: &mut Database, v: serde_json::Value) -> u64 {
    run(db, 1, v).unwrap()["patch"].as_u64().unwrap()
}
fn admit(db: &mut Database, channel: &str, id: u64) {
    run(db, 2, json!({"cmd":"approve","patch":id})).unwrap();
    run(db, 3, json!({"cmd":"check","patch":id,"passed":true})).unwrap();
    run(db, 2, json!({"cmd":"merge","channel":channel,"patch":id})).unwrap();
}
fn fresh() -> Database {
    let mut db = Database::default();
    run(&mut db, 1, json!({"cmd":"create"})).unwrap();
    db
}
fn view(db: &Database, channel: &str) -> View {
    db.repo(&actor(1), "r")
        .unwrap()
        .materialize(channel)
        .unwrap()
}

#[test]
fn base64_round_trips() {
    for bytes in [&b""[..], b"a", b"ab", b"abc", &[0, 255, 7, 128, 9]] {
        assert_eq!(base64_decode(&base64_encode(bytes)).unwrap(), bytes);
    }
    assert!(base64_decode("abc").is_none());
    assert!(base64_decode("YQ==YQ==").is_none());
}

#[test]
fn binary_and_executable_files_keep_bytes_and_mode() {
    let mut db = fresh();
    let bytes = [0u8, 159, 146, 150, 255];
    let id = patch(
        &mut db,
        json!({"cmd":"propose","intent":"tools","dependencies":[],"edits":[
        {"path":"bin/blob","content":base64_encode(&bytes),"base64":true},
        {"path":"run.sh","content":"#!/bin/sh\n","executable":true}]}),
    );
    admit(&mut db, "main", id);
    let v = view(&db, "main");
    assert_eq!(v.bytes("bin/blob").unwrap(), (bytes.to_vec(), false));
    assert_eq!(v.bytes("run.sh").unwrap(), (b"#!/bin/sh\n".to_vec(), true));
    assert!(
        run(
            &mut db,
            1,
            json!({"cmd":"propose","intent":"bad","dependencies":[],
        "edits":[{"path":"x","content":"not base64!","base64":true}]})
        )
        .is_err()
    );
    // Line edits need text, and keep the base's mode.
    assert_eq!(
        run(
            &mut db,
            1,
            json!({"cmd":"propose_lines","intent":"x","dependencies":[],
            "edits":[{"path":"bin/blob","base":id,"start":0,"end":1,"content":"x"}]})
        )
        .unwrap_err()
        .0,
        "line edits need a text file; replace binary files whole"
    );
    let line = patch(
        &mut db,
        json!({"cmd":"propose_lines","intent":"shell","dependencies":[],
        "edits":[{"path":"run.sh","base":id,"start":0,"end":1,"content":"#!/bin/bash\n"}]}),
    );
    admit(&mut db, "main", line);
    assert_eq!(
        view(&db, "main").bytes("run.sh").unwrap(),
        (b"#!/bin/bash\n".to_vec(), true)
    );
}

#[test]
fn concurrent_mode_changes_conflict() {
    let mut db = fresh();
    let base = patch(
        &mut db,
        json!({"cmd":"propose","intent":"b","dependencies":[],"edits":[{"path":"s","content":"x"}]}),
    );
    admit(&mut db, "main", base);
    let a = patch(
        &mut db,
        json!({"cmd":"propose","intent":"exec","dependencies":[base],"edits":[{"path":"s","content":"x","executable":true}]}),
    );
    let b = patch(
        &mut db,
        json!({"cmd":"propose","intent":"keep","dependencies":[base],"edits":[{"path":"s","content":"y"}]}),
    );
    admit(&mut db, "main", a);
    admit(&mut db, "main", b);
    let v = view(&db, "main");
    let alternatives = &v.conflicts["s"];
    assert_eq!(alternatives.len(), 2);
    assert!(
        alternatives
            .iter()
            .any(|x| x.executable && x.content.as_deref() == Some("x"))
    );
}

#[test]
fn plain_text_identities_are_unchanged_by_new_attributes() {
    let mut db = fresh();
    let reply = run(&mut db, 1, json!({"cmd":"propose","intent":"i","dependencies":[],"edits":[{"path":"a","content":"b"}]})).unwrap();
    let repo = db.repo(&actor(1), "r").unwrap();
    // The pre-attribute encoding: `{"path":..,"content":..}` in field order.
    let expected = body::hash(br#"["o5s-patch-v1",1,"i",[],[{"path":"a","content":"b"}]]"#);
    assert_eq!(reply["hash"], expected);
    assert_eq!(repo.patches[&1].hash, expected);
}

fn insert(text: &str) -> serde_json::Value {
    json!({"kind":"insert","left":"Start","right":"End","atoms":[text.as_bytes()]})
}

#[test]
fn graph_documents_can_be_empty_removed_and_restored_with_conflicts() {
    let mut db = fresh();
    // An empty document.
    let empty = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"empty","dependencies":[],
        "edits":[{"path":"e.txt","operations":[]}]}),
    );
    let doc = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"doc","dependencies":[],
        "edits":[{"path":"d.txt","operations":[insert("hello\n")],"executable":true}]}),
    );
    admit(&mut db, "main", empty);
    admit(&mut db, "main", doc);
    let v = view(&db, "main");
    assert_eq!(v.files["e.txt"], "");
    assert_eq!(v.bytes("d.txt").unwrap(), (b"hello\n".to_vec(), true));
    run(
        &mut db,
        4,
        json!({"cmd":"fork","source":"main","channel":"side"}),
    )
    .unwrap();
    // Removal, and a concurrent edit of the same document.
    let removal = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"rm","dependencies":[doc],
        "edits":[{"path":"d.txt","operations":[],"remove":true}]}),
    );
    admit(&mut db, "main", removal);
    assert!(!view(&db, "main").files.contains_key("d.txt"));
    let hex = db.repo(&actor(1), "r").unwrap().patches[&doc].hash.clone();
    let id: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let node = json!({"insertion":{"patch":id,"ordinal":0},"ordinal":0});
    let edit = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"edit","dependencies":[doc],
        "edits":[{"path":"d.txt","executable":true,"operations":[
            {"kind":"insert","left":{"Node":node},"right":"End","atoms":[b"world\n"]}]}]}),
    );
    admit(&mut db, "side", edit);
    assert_eq!(view(&db, "side").files["d.txt"], "hello\nworld\n");
    admit(&mut db, "main", edit);
    let v = view(&db, "main");
    assert_eq!(v.conflicts["d.txt"].len(), 2, "{v:?}");
    // A resolution observing both restores the file.
    let fix = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"keep","dependencies":[removal, edit],
        "edits":[{"path":"d.txt","operations":[],"executable":true}]}),
    );
    admit(&mut db, "main", fix);
    let v = view(&db, "main");
    assert!(v.conflicts.is_empty());
    assert_eq!(
        v.bytes("d.txt").unwrap(),
        (b"hello\nworld\n".to_vec(), true)
    );
}

#[test]
fn mixed_proposals_carry_graph_and_binary_files() {
    let mut db = fresh();
    let id = patch(
        &mut db,
        json!({"cmd":"propose_graph","intent":"mixed","dependencies":[],
        "edits":[{"path":"doc.txt","operations":[insert("text\n")]}],
        "files":[{"path":"img.bin","content":base64_encode(&[1,2,3]),"base64":true}]}),
    );
    admit(&mut db, "main", id);
    let v = view(&db, "main");
    assert_eq!(v.files["doc.txt"], "text\n");
    assert_eq!(v.bytes("img.bin").unwrap().0, vec![1, 2, 3]);
    // The same path cannot be both.
    assert!(
        run(
            &mut db,
            1,
            json!({"cmd":"propose_graph","intent":"both","dependencies":[],
        "edits":[{"path":"x","operations":[insert("a\n")]}],"files":[{"path":"x","content":"b"}]})
        )
        .is_err()
    );
    // Kinds stay fixed per path.
    assert_eq!(
        run(&mut db, 1, json!({"cmd":"propose","intent":"x","dependencies":[],"edits":[{"path":"doc.txt","content":"b"}]})).unwrap_err().0,
        "graph document requires graph operations"
    );
    // A restart replays the same view.
    let replayed: Database = serde_json::from_value(serde_json::to_value(&db).unwrap()).unwrap();
    assert_eq!(
        replayed
            .repo(&actor(1), "r")
            .unwrap()
            .materialize("main")
            .unwrap(),
        v
    );
}
