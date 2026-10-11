//! Per-command cost of the kernel-backed local engine as a repository grows.
use o5s_platform::{Actor, Command, Database};
use serde_json::json;
use std::time::Instant;

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1000);
    let mut db = Database::default();
    let a = |user| Actor { tenant: 1, user };
    let cmd = |v| serde_json::from_value::<Command>(v).unwrap();
    db.execute(&a(1), cmd(json!({"cmd":"create","repo":"r"})))
        .unwrap();
    let base: String = (0..n).map(|i| format!("line {i}\n")).collect();
    db.execute(
        &a(1),
        cmd(
            json!({"cmd":"propose","repo":"r","intent":"base","dependencies":[],
        "edits":[{"path":"f","content":base}]}),
        ),
    )
    .unwrap();
    let t = Instant::now();
    for i in 0..n {
        db.execute(
            &a(10 + i),
            cmd(
                json!({"cmd":"propose_lines","repo":"r","intent":"edit","dependencies":[],
            "edits":[{"path":"f","base":1,"start":i,"end":i+1,"content":format!("LINE {i}\n")}]}),
            ),
        )
        .unwrap();
    }
    let propose = t.elapsed();
    let t = Instant::now();
    for id in 2..n + 2 {
        db.execute(&a(2), cmd(json!({"cmd":"approve","repo":"r","patch":id})))
            .unwrap();
    }
    println!(
        "{n} line proposals: {:?} each; approvals: {:?} each",
        propose / n as u32,
        t.elapsed() / n as u32
    );
}
