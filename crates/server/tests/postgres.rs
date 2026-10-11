//! PostgreSQL storage, against `O5S_TEST_DATABASE_URL` (skipped when unset).
//! Each test uses its own PostgreSQL schema.
use o5s_platform::{Actor, Command, Database};
use o5s_server::{Failure, pg::PgBackend};

#[test]
fn engine_tables_match_the_schema_macro() {
    assert_eq!(o5s_pgwire::tables(), schema_spec());
}

pub struct Registry;
o5s_registry::registry_tables!(Registry);
use serde_json::json;
use std::sync::Arc;

fn url() -> Option<String> {
    let url = std::env::var("O5S_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("O5S_TEST_DATABASE_URL unset; skipping PostgreSQL test");
    }
    url
}

fn schema(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("o5s_test_{name}_{}_{nanos}", std::process::id())
}

async fn backend(url: &str, schema: &str) -> PgBackend {
    o5s_server::pg::connect(url, schema, 8).await.unwrap()
}

/// Remove a test's schema.
async fn drop_schema(url: &str, schema: &str) {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
        .await
        .unwrap();
}

fn actor(user: u64) -> Actor {
    Actor { tenant: 1, user }
}

fn command(value: serde_json::Value) -> Command {
    serde_json::from_value(value).unwrap()
}

async fn run(pg: &PgBackend, user: u64, value: serde_json::Value) -> serde_json::Value {
    match pg.execute(&actor(user), command(value.clone()), None).await {
        Ok(v) => v,
        Err(e) => panic!("{value}: {e:?}"),
    }
}

/// The same commands on the local engine and on PostgreSQL.
fn script() -> Vec<(u64, serde_json::Value)> {
    vec![
        (1, json!({"cmd":"create","repo":"alpha"})),
        (
            1,
            json!({"cmd":"propose","repo":"alpha","intent":"base","dependencies":[],
            "edits":[{"path":"a.txt","content":"one\ntwo\nthree\n"}]}),
        ),
        (2, json!({"cmd":"approve","repo":"alpha","patch":1})),
        (
            3,
            json!({"cmd":"check","repo":"alpha","patch":1,"passed":true}),
        ),
        (
            2,
            json!({"cmd":"merge","repo":"alpha","channel":"main","patch":1}),
        ),
        (
            4,
            json!({"cmd":"fork","repo":"alpha","source":"main","channel":"side"}),
        ),
        (
            1,
            json!({"cmd":"propose_lines","repo":"alpha","intent":"first line","dependencies":[],
            "edits":[{"path":"a.txt","base":1,"start":0,"end":1,"content":"ONE\n"}]}),
        ),
        (
            4,
            json!({"cmd":"propose_lines","repo":"alpha","intent":"last line","dependencies":[],
            "edits":[{"path":"a.txt","base":1,"start":2,"end":3,"content":"THREE\n"}]}),
        ),
        (
            1,
            json!({"cmd":"propose_graph","repo":"alpha","intent":"graph","dependencies":[],
            "edits":[{"path":"g.txt","operations":[{"kind":"insert","left":"Start","right":"End","atoms":[[104,105]]}]}]}),
        ),
        (2, json!({"cmd":"approve","repo":"alpha","patch":2})),
        (2, json!({"cmd":"approve","repo":"alpha","patch":3})),
        (2, json!({"cmd":"approve","repo":"alpha","patch":4})),
        (
            3,
            json!({"cmd":"check","repo":"alpha","patch":2,"passed":true}),
        ),
        (
            3,
            json!({"cmd":"check","repo":"alpha","patch":3,"passed":true}),
        ),
        (
            3,
            json!({"cmd":"check","repo":"alpha","patch":4,"passed":true}),
        ),
        (
            2,
            json!({"cmd":"merge_batch","repo":"alpha","channel":"main","patches":[4,3,2]}),
        ),
    ]
}

#[tokio::test]
async fn postgres_matches_the_local_engine_and_survives_restart() {
    let Some(url) = url() else { return };
    let schema = schema("parity");
    let pg = backend(&url, &schema).await;
    let mut local = Database::default();
    for (user, value) in script() {
        let expected = local.execute(&actor(user), command(value.clone())).unwrap();
        assert_eq!(run(&pg, user, value).await, expected);
    }
    // Refusals agree and commit nothing.
    for (user, value) in [
        (1, json!({"cmd":"approve","repo":"alpha","patch":1})),
        (
            2,
            json!({"cmd":"merge","repo":"alpha","channel":"main","patch":9}),
        ),
        (1, json!({"cmd":"create","repo":"alpha"})),
        (
            1,
            json!({"cmd":"propose","repo":"alpha","intent":"x","dependencies":[],
            "edits":[{"path":"g.txt","content":"legacy"}]}),
        ),
    ] {
        let expected = local
            .execute(&actor(user), command(value.clone()))
            .unwrap_err();
        match pg.execute(&actor(user), command(value), None).await {
            Err(Failure::Refused(r)) => assert_eq!(r, expected),
            other => panic!("{other:?}"),
        }
    }
    let view = pg.repository(&actor(1), "alpha").await.unwrap();
    let expected = local.repo(&actor(1), "alpha").unwrap();
    assert_eq!(view.patches, expected.patches);
    assert_eq!(view.channels, expected.channels);
    assert_eq!(view.events, expected.events);
    assert_eq!(
        view.materialize("main").unwrap().files["a.txt"],
        "ONE\ntwo\nTHREE\n"
    );
    // A new process reads the same committed state from scratch.
    drop(pg);
    let restarted = backend(&url, &schema).await;
    let reread = restarted.repository(&actor(1), "alpha").await.unwrap();
    assert_eq!(*reread, *view);
    assert_eq!(
        restarted.repositories(&actor(1)).await.unwrap(),
        vec!["alpha".to_string()]
    );
    // Other organizations see nothing.
    let other = Actor { tenant: 2, user: 1 };
    assert!(matches!(
        restarted.repository(&other, "alpha").await,
        Err(Failure::Refused(_))
    ));
    assert!(restarted.repositories(&other).await.unwrap().is_empty());
    drop(restarted);
    drop_schema(&url, &schema).await;
}

#[tokio::test]
async fn concurrent_agents_and_replicas_commit_serializably() {
    let Some(url) = url() else { return };
    let schema = schema("concurrent");
    let replicas = [
        Arc::new(backend(&url, &schema).await),
        Arc::new(backend(&url, &schema).await),
    ];
    run(&replicas[0], 1, json!({"cmd":"create","repo":"hot"})).await;
    run(&replicas[1], 1, json!({"cmd":"create","repo":"cold"})).await;
    let agents = 48;
    let mut tasks = Vec::new();
    for agent in 0..agents {
        let pg = replicas[agent % 2].clone();
        let repo = if agent % 3 == 0 { "cold" } else { "hot" };
        tasks.push(tokio::spawn(async move {
            let reply = pg
                .execute(
                    &Actor { tenant: 1, user: 100 + agent as u64 },
                    command(json!({"cmd":"propose","repo":repo,"intent":format!("agent {agent}"),
                        "dependencies":[],"edits":[{"path":format!("f{agent}.txt"),"content":"x"}]})),
                    None,
                )
                .await
                .unwrap();
            (repo, reply["patch"].as_u64().unwrap())
        }));
    }
    let mut ids: std::collections::BTreeMap<&str, Vec<u64>> = Default::default();
    for t in tasks {
        let (repo, id) = t.await.unwrap();
        ids.entry(repo).or_default().push(id);
    }
    for (repo, mut handles) in ids {
        handles.sort_unstable();
        let expected: Vec<u64> = (1..=handles.len() as u64).collect();
        assert_eq!(handles, expected, "{repo}: handles are dense and unique");
    }
    // Concurrent reviews and admissions of every hot patch.
    let hot = replicas[0].repository(&actor(1), "hot").await.unwrap();
    let mut tasks = Vec::new();
    for id in hot.patches.keys().copied() {
        let pg = replicas[id as usize % 2].clone();
        tasks.push(tokio::spawn(async move {
            for (user, cmd) in [
                (2, json!({"cmd":"approve","repo":"hot","patch":id})),
                (
                    3,
                    json!({"cmd":"check","repo":"hot","patch":id,"passed":true}),
                ),
                (
                    4,
                    json!({"cmd":"merge","repo":"hot","channel":"main","patch":id}),
                ),
            ] {
                pg.execute(&actor(user), command(cmd), None).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    for pg in &replicas {
        let view = pg.repository(&actor(1), "hot").await.unwrap();
        let mut members = view.channels["main"].clone();
        assert_eq!(members.len(), hot.patches.len());
        members.sort_unstable();
        assert_eq!(members, hot.patches.keys().copied().collect::<Vec<_>>());
        assert_eq!(
            view.materialize("main").unwrap().files.len(),
            hot.patches.len()
        );
    }
    let a = replicas[0].repository(&actor(1), "hot").await.unwrap();
    let b = replicas[1].repository(&actor(1), "hot").await.unwrap();
    assert_eq!(a.channels, b.channels);
    assert_eq!(a.events, b.events);
    drop(replicas);
    drop_schema(&url, &schema).await;
}

#[tokio::test]
async fn idempotency_keys_commit_once() {
    let Some(url) = url() else { return };
    let schema = schema("idem");
    let pg = backend(&url, &schema).await;
    let create = || command(json!({"cmd":"create","repo":"alpha"}));
    let first = pg
        .execute(&actor(1), create(), Some("req-1"))
        .await
        .unwrap();
    assert_eq!(
        pg.execute(&actor(1), create(), Some("req-1"))
            .await
            .unwrap(),
        first
    );
    let propose = || {
        command(
            json!({"cmd":"propose","repo":"alpha","intent":"once","dependencies":[],
            "edits":[{"path":"a","content":"a"}]}),
        )
    };
    let p = pg
        .execute(&actor(1), propose(), Some("req-2"))
        .await
        .unwrap();
    assert_eq!(
        pg.execute(&actor(1), propose(), Some("req-2"))
            .await
            .unwrap(),
        p
    );
    assert!(matches!(
        pg.execute(&actor(1), create(), Some("req-2")).await,
        Err(Failure::Conflict(_))
    ));
    let view = pg.repository(&actor(1), "alpha").await.unwrap();
    assert_eq!(view.patches.len(), 1);
    assert_eq!(view.events.len(), 2);
    drop(pg);
    drop_schema(&url, &schema).await;
}
