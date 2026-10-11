//! Latency of single uncontended registry transactions on PostgreSQL.
//! `O5S_DATABASE_URL=... cargo run --release -p o5s-server --example pg_latency -- 500`
use o5s_platform::{Actor, Command};
use serde_json::json;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let url = std::env::var("O5S_DATABASE_URL").expect("O5S_DATABASE_URL");
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500);
    let schema = format!("o5s_latency_{}", std::process::id());
    let pg = o5s_server::pg::connect(&url, &schema, 4).await.unwrap();
    let a = |user| Actor { tenant: 1, user };
    let cmd = |v| serde_json::from_value::<Command>(v).unwrap();
    pg.execute(&a(1), cmd(json!({"cmd":"create","repo":"r"})), None)
        .await
        .unwrap();
    let t = Instant::now();
    for i in 0..n {
        pg.execute(
            &a(1),
            cmd(
                json!({"cmd":"propose","repo":"r","intent":"x","dependencies":[],
            "edits":[{"path":format!("f{i}"),"content":"x"}]}),
            ),
            None,
        )
        .await
        .unwrap();
    }
    let propose = t.elapsed() / n as u32;
    let t = Instant::now();
    for id in 1..=n {
        pg.execute(
            &a(2),
            cmd(json!({"cmd":"approve","repo":"r","patch":id})),
            None,
        )
        .await
        .unwrap();
    }
    let approve = t.elapsed() / n as u32;
    let t = Instant::now();
    for _ in 0..n {
        pg.engine
            .snapshot(
                o5s_platform::repository_key(1, "r"),
                &o5s_registry::Command::Probe,
            )
            .await
            .unwrap();
    }
    println!(
        "propose {propose:?}  approve {approve:?}  probe {:?}",
        t.elapsed() / n as u32
    );
}
