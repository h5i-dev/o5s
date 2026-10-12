//! How single operations scale with a repository's history (`history` mode).
//!
//! One repository grows through the given sizes of admitted graph-document
//! patches, each creating its own file. `independent` patches have no
//! dependencies, so every one is a causal head; in a `chain` each depends on
//! the previous one, so there is one head. At each size every operation runs
//! `samples` times on its own, with the whole history in place: a proposal,
//! the admission of one approved patch, the channel view, one document, a Git
//! export, and a workspace round trip (create it, edit a file, propose).
use crate::{Client, Samples, phase};
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};

const ADMIN: u64 = 1;
const REVIEWER: u64 = 2;
const CHECKER: u64 = 3;

fn file(path: &str, text: &str) -> Value {
    json!({"path": path, "operations": [{"kind": "insert", "left": "Start", "right": "End",
        "atoms": [text.as_bytes()]}]})
}

async fn admit(c: &Client, repo: &str, ids: &[u64], concurrency: usize) -> Result<(), String> {
    let reviews = |user: u64, cmd: &'static str| {
        let jobs = ids.to_vec();
        phase(concurrency, jobs, move |id| {
            let body = if cmd == "approve" {
                json!({"cmd":"approve","repo":repo,"patch":id})
            } else {
                json!({"cmd":"check","repo":repo,"patch":id,"passed":true})
            };
            async move { c.call(user, "/api/command", Some(body)).await }
        })
    };
    for (user, cmd) in [(REVIEWER, "approve"), (CHECKER, "check")] {
        let (s, _, _) = reviews(user, cmd).await;
        if s.failed != 0 {
            return Err(format!("{} {cmd} requests failed", s.failed));
        }
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    for chunk in sorted.chunks(1000) {
        c.call(
            ADMIN,
            "/api/command",
            Some(json!({"cmd":"merge_batch","repo":repo,
            "channel":"main","patches":chunk})),
        )
        .await?;
    }
    Ok(())
}

/// Grow `repo` from `from` to `to` admitted patches.
async fn grow(
    c: &Client,
    repo: &str,
    chain: bool,
    from: u64,
    to: u64,
    last: &mut Option<u64>,
    concurrency: usize,
) -> Result<(), String> {
    let mut ids = Vec::new();
    if chain {
        for k in from..to {
            let deps: Vec<u64> = last.iter().copied().collect();
            let v = c
                .call(
                    ADMIN,
                    "/api/command",
                    Some(json!({"cmd":"propose_graph","repo":repo,
                "intent":format!("history {k}"),"dependencies":deps,
                "edits":[file(&format!("h/{k}.txt"), &format!("line {k}\n"))]})),
                )
                .await?;
            let id = v["patch"].as_u64().ok_or("no patch id")?;
            *last = Some(id);
            ids.push(id);
        }
    } else {
        let (s, _, out) = phase(concurrency, (from..to).collect(), |k| async move {
            c.call(
                ADMIN,
                "/api/command",
                Some(json!({"cmd":"propose_graph","repo":repo,
                "intent":format!("history {k}"),"dependencies":[],
                "edits":[file(&format!("h/{k}.txt"), &format!("line {k}\n"))]})),
            )
            .await
        })
        .await;
        if s.failed != 0 {
            return Err(format!("{} proposals failed", s.failed));
        }
        ids = out.iter().filter_map(|v| v["patch"].as_u64()).collect();
    }
    admit(c, repo, &ids, concurrency).await
}

/// Time `f` `n` times, sequentially; also report the largest reply size.
/// Returns the report and the successful replies.
async fn time<F, Fut>(n: usize, mut f: F) -> (Value, Vec<Value>)
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut samples = Samples::default();
    let mut bytes = 0;
    let mut error = None;
    let mut replies = Vec::new();
    let started = Instant::now();
    for i in 0..n {
        let t = Instant::now();
        match f(i).await {
            Ok(v) => {
                samples.ms.push(t.elapsed().as_secs_f64() * 1000.0);
                bytes = bytes.max(v.to_string().len());
                replies.push(v);
            }
            Err(e) => {
                samples.failed += 1;
                error.get_or_insert(e);
            }
        }
    }
    let mut r = samples.report(started.elapsed().as_secs_f64());
    r["reply_bytes"] = json!(bytes);
    if let Some(e) = error {
        r["error"] = json!(e);
    }
    (r, replies)
}

pub async fn run(
    c: Arc<Client>,
    sizes: &[u64],
    chain: bool,
    samples: usize,
    concurrency: usize,
) -> Value {
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo = format!("history-{run}");
    let c = &*c;
    c.call(
        ADMIN,
        "/api/command",
        Some(json!({"cmd":"create","repo":repo})),
    )
    .await
    .unwrap();
    let (mut have, mut last, mut rows) = (0u64, None, Vec::new());
    for &size in sizes {
        let t = Instant::now();
        grow(c, &repo, chain, have, size, &mut last, concurrency)
            .await
            .unwrap();
        let grown = t.elapsed().as_secs_f64();
        have = size;
        let r = repo.as_str();
        let (propose, replies) = time(samples, |i| {
            let deps: Vec<u64> = last.iter().copied().collect();
            let body = json!({"cmd":"propose_graph","repo":r,"intent":"measured",
                "dependencies":deps,"edits":[file(&format!("m/{size}-{i}.txt"), "measured\n")]});
            async move { c.call(ADMIN, "/api/command", Some(body)).await }
        })
        .await;
        let proposed: Vec<u64> = replies.iter().filter_map(|v| v["patch"].as_u64()).collect();
        // Approve and check them first, so the timed command is the admission.
        for &id in &proposed {
            c.call(
                REVIEWER,
                "/api/command",
                Some(json!({"cmd":"approve","repo":r,"patch":id})),
            )
            .await
            .unwrap();
            c.call(
                CHECKER,
                "/api/command",
                Some(json!({"cmd":"check","repo":r,"patch":id,"passed":true})),
            )
            .await
            .unwrap();
        }
        let (merge, _) = time(proposed.len(), |i| {
            let body = json!({"cmd":"merge","repo":r,"channel":"main","patch":proposed[i]});
            async move { c.call(ADMIN, "/api/command", Some(body)).await }
        })
        .await;
        let (view, _) = time(samples, |_| async move {
            c.call(ADMIN, &format!("/api/view?repo={r}"), None).await
        })
        .await;
        let (document, _) = time(samples, |_| async move {
            c.call(
                ADMIN,
                &format!("/api/document?repo={r}&channel=main&path=h/0.txt"),
                None,
            )
            .await
        })
        .await;
        let (export, _) = time(samples, |_| async move {
            c.call(
                ADMIN,
                "/api/export",
                Some(json!({"repo":r,"channel":"main"})),
            )
            .await
        })
        .await;
        // Each sample is a new agent, so each gets a new workspace.
        let agent = |i: usize| 10_000 + size * 100 + i as u64;
        let (workspace, created) = time(samples, |i| async move {
            c.call(agent(i), "/api/worktree", Some(json!({"repo": r})))
                .await
        })
        .await;
        let paths: Vec<(String, Value)> = created
            .iter()
            .map(|v| {
                (
                    v["path"].as_str().unwrap_or_default().to_string(),
                    v["commit"].clone(),
                )
            })
            .collect();
        let mut heads = 0;
        for (path, _) in &paths {
            let out = std::process::Command::new("git")
                .args(["-C", path, "log", "-1", "--format=%B"])
                .output()
                .map_err(|e| e.to_string());
            if let Ok(o) = out {
                let message = String::from_utf8_lossy(&o.stdout).into_owned();
                heads = message
                    .split("Patch hashes:")
                    .nth(1)
                    .map_or(0, |h| h.split_whitespace().count());
            }
            let _ = std::fs::write(
                std::path::Path::new(path).join("h/0.txt"),
                "line 0\nedited\n",
            );
        }
        let (workspace_propose, _) = time(paths.len(), |i| {
            let (_, commit) = paths[i].clone();
            async move {
                c.call(
                    agent(i),
                    "/api/worktree/propose",
                    Some(json!({"repo":r,"commit":commit,"intent":"workspace edit"})),
                )
                .await
            }
        })
        .await;
        let row = json!({"patches": size, "grow_seconds": grown, "snapshot_heads": heads,
            "propose": propose, "merge": merge, "view": view, "document": document,
            "export": export, "workspace": workspace, "workspace_propose": workspace_propose});
        eprintln!("{row}");
        rows.push(row);
    }
    json!({"repo": repo, "shape": if chain { "chain" } else { "independent" }, "sizes": rows})
}
