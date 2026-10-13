//! Closed-loop load for o5s and a plain-Git baseline on the same workloads.
//!
//! `o5s` drives a running server over its authenticated HTTP API with
//! persistent connections: every author proposes concurrently, an independent
//! reviewer and check reporter report on every proposal, and patches are
//! admitted in batches. `git` runs the same edits as agents with their own
//! clones that `push`, and `pull --rebase` on rejection (`git.rs`). `compare`
//! runs both and prints one report. Final contents are checked in every mode.
//!
//! ```sh
//! H5I_APP_SECRET=... o5s-load compare --url http://127.0.0.1:3000 --agents 1000 --concurrency 64 --workload disjoint
//! ```
//!
//! `history` times single operations as one repository's history grows
//! (`history.rs`):
//!
//! ```sh
//! H5I_APP_SECRET=... o5s-load history --url http://127.0.0.1:3000 --sizes 1000,10000 --shape chain
//! ```
mod git;
mod history;

use futures::{StreamExt, stream};
use o5s_server::auth::HmacAuth;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// One workload, shared by both systems.
pub struct Work {
    pub workload: String,
    pub agents: u64,
    pub concurrency: usize,
    pub batch: usize,
    /// Lines between edited lines in `disjoint` (1 = adjacent lines).
    pub spacing: u64,
    /// Propose through this `o5s` CLI in per-agent Git repositories.
    pub cli: Option<std::path::PathBuf>,
    /// Open loop: proposals arrive at this rate (per second) whatever the
    /// server's latency, instead of `concurrency` closed-loop clients.
    pub rate: Option<f64>,
}

impl Work {
    pub fn line_workload(&self) -> bool {
        matches!(self.workload.as_str(), "disjoint" | "overlap")
    }
    /// The line agent `i` replaces.
    pub fn line_of(&self, i: u64) -> u64 {
        if self.workload == "overlap" {
            0
        } else {
            i * self.spacing
        }
    }
    pub fn base(&self) -> String {
        (0..self.agents * self.spacing).map(expected_line).collect()
    }
}

/// What agent `i` writes.
pub fn content(i: u64) -> String {
    format!("agent {i}\n")
}

/// Line `n` of the shared base.
pub fn expected_line(n: u64) -> String {
    format!("line {n}\n")
}

struct Client {
    http: reqwest::Client,
    url: String,
    auth: HmacAuth,
    tenant: u64,
}

impl Client {
    async fn call(&self, user: u64, route: &str, body: Option<Value>) -> Result<Value, String> {
        let token = self.auth.issue(self.tenant, user, u64::MAX / 2);
        let request = match body {
            Some(b) => self.http.post(format!("{}{route}", self.url)).json(&b),
            None => self.http.get(format!("{}{route}", self.url)),
        };
        let response = request
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let value: Value = response.json().await.map_err(|e| e.to_string())?;
        if status.is_success() {
            Ok(value)
        } else {
            Err(format!("{status}: {value}"))
        }
    }
}

#[derive(Default)]
pub struct Samples {
    pub ms: Vec<f64>,
    pub failed: usize,
}

impl Samples {
    pub fn report(&mut self, seconds: f64) -> Value {
        self.ms.sort_by(|a, b| a.total_cmp(b));
        let p = |q: f64| {
            self.ms
                .get(((self.ms.len().max(1) - 1) as f64 * q) as usize)
                .copied()
                .unwrap_or(0.0)
        };
        json!({"requests": self.ms.len(), "failed": self.failed, "seconds": seconds,
            "per_second": self.ms.len() as f64 / seconds, "p50_ms": p(0.5), "p95_ms": p(0.95),
            "p99_ms": p(0.99), "max_ms": p(1.0)})
    }
}

/// Open loop: job `k` is sent at `k / rate` seconds, whatever earlier jobs
/// are doing, and its latency counts from that scheduled time, so time spent
/// queueing behind an overloaded server is measured, not hidden.
async fn open_phase<F, Fut>(rate: f64, jobs: Vec<u64>, f: F) -> (Samples, f64, Vec<Value>)
where
    F: Fn(u64) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let started = tokio::time::Instant::now();
    let results: Vec<(Duration, Result<Value, String>)> =
        stream::iter(jobs.into_iter().enumerate())
            .map(|(k, j)| {
                let at = started + Duration::from_secs_f64(k as f64 / rate);
                let fut = f(j);
                async move {
                    tokio::time::sleep_until(at).await;
                    let r = fut.await;
                    (at.elapsed(), r)
                }
            })
            .buffer_unordered(100_000)
            .collect()
            .await;
    let mut s = Samples::default();
    let mut values = Vec::new();
    for (d, r) in results {
        match r {
            Ok(v) => {
                s.ms.push(d.as_secs_f64() * 1000.0);
                values.push(v);
            }
            Err(e) => {
                s.failed += 1;
                if s.failed <= 3 {
                    eprintln!("request failed: {e}");
                }
            }
        }
    }
    (s, started.elapsed().as_secs_f64(), values)
}

/// Run `jobs` with at most `n` in flight; time each.
async fn phase<F, Fut>(n: usize, jobs: Vec<u64>, f: F) -> (Samples, f64, Vec<Value>)
where
    F: Fn(u64) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let started = Instant::now();
    let results: Vec<(Duration, Result<Value, String>)> = stream::iter(jobs)
        .map(|j| {
            let fut = f(j);
            async move {
                let t = Instant::now();
                let r = fut.await;
                (t.elapsed(), r)
            }
        })
        .buffer_unordered(n)
        .collect()
        .await;
    let mut s = Samples::default();
    let mut values = Vec::new();
    for (d, r) in results {
        match r {
            Ok(v) => {
                s.ms.push(d.as_secs_f64() * 1000.0);
                values.push(v);
            }
            Err(e) => {
                s.failed += 1;
                if s.failed <= 3 {
                    eprintln!("request failed: {e}");
                }
            }
        }
    }
    (s, started.elapsed().as_secs_f64(), values)
}

async fn o5s(w: &Work, url: &str, tenant: u64) -> Value {
    let secret = std::env::var("H5I_APP_SECRET").expect("H5I_APP_SECRET must sign test tokens");
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(w.concurrency)
        .build()
        .unwrap();
    let c = std::sync::Arc::new(Client {
        http,
        url: url.trim_end_matches('/').into(),
        auth: HmacAuth::new(secret),
        tenant,
    });
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo = |agent: u64| -> String {
        if w.workload == "repositories" {
            format!("load-{run}-{agent}")
        } else {
            format!("load-{run}")
        }
    };
    let (reviewer, checker, admin) = (2u64, 3u64, 1u64);
    let repos: Vec<String> = if w.workload == "repositories" {
        (0..w.agents).map(repo).collect()
    } else {
        vec![repo(0)]
    };
    let mut bases = std::collections::BTreeMap::new();
    for r in &repos {
        c.call(
            admin,
            "/api/command",
            Some(json!({"cmd":"create","repo":r})),
        )
        .await
        .unwrap();
        if w.line_workload() {
            let id = c
                .call(
                    admin,
                    "/api/command",
                    Some(json!({"cmd":"propose","repo":r,"intent":"base",
                "dependencies":[],"edits":[{"path":"base.txt","content":w.base()}]})),
                )
                .await
                .unwrap()["patch"]
                .as_u64()
                .unwrap();
            for (u, cmd) in [
                (reviewer, json!({"cmd":"approve","repo":r,"patch":id})),
                (
                    checker,
                    json!({"cmd":"check","repo":r,"patch":id,"passed":true}),
                ),
                (
                    reviewer,
                    json!({"cmd":"merge","repo":r,"channel":"main","patch":id}),
                ),
            ] {
                c.call(u, "/api/command", Some(cmd)).await.unwrap();
            }
            bases.insert(r.clone(), id);
        }
    }
    // With the CLI, each agent first gets its own Git repository holding the
    // admitted snapshot (setup, not measured).
    let agent_dir = |i: u64| {
        std::env::temp_dir()
            .join(format!("o5s-cli-{run}"))
            .join(format!("agent-{i}"))
    };
    if let Some(cli) = &w.cli {
        let (_, _, _) = phase(w.concurrency, (0..w.agents).collect(), |i| {
            let dir = agent_dir(i);
            let r = repo(i);
            let token = c.auth.issue(tenant, 1000 + i, u64::MAX / 2);
            let line = w.line_workload();
            async move {
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                run_in(&dir, "git", &["init", "-q"], &token, url).await?;
                run_in(&dir, cli.to_str().unwrap(), &["connect", &r], &token, url).await?;
                if line {
                    run_in(&dir, cli.to_str().unwrap(), &["fetch"], &token, url).await?;
                    run_in(
                        &dir,
                        "git",
                        &["checkout", "-q", "--detach", "refs/o5s/main"],
                        &token,
                        url,
                    )
                    .await?;
                }
                Ok(Value::Null)
            }
        })
        .await;
    }
    let started = Instant::now();
    let propose_one = |i: u64| {
        let c = c.clone();
        let r = repo(i);
        let base_id = bases.get(&r).copied();
        let body = if w.line_workload() {
            json!({"cmd":"propose_lines","repo":r,"intent":format!("agent {i}"),"dependencies":[],
                "edits":[{"path":"base.txt","base":base_id.unwrap(),"start":w.line_of(i),"end":w.line_of(i)+1,"content":content(i)}]})
        } else {
            json!({"cmd":"propose","repo":r,"intent":format!("agent {i}"),"dependencies":[],
                "edits":[{"path":format!("agent-{i}.txt"),"content":content(i)}]})
        };
        let cli = w.cli.clone();
        let dir = agent_dir(i);
        let line = w.line_workload().then(|| w.line_of(i));
        let token = c.auth.issue(tenant, 1000 + i, u64::MAX / 2);
        async move {
            let Some(cli) = cli else {
                return c
                    .call(1000 + i, "/api/command", Some(body))
                    .await
                    .map(|v| json!({"repo":r,"patch":v["patch"]}));
            };
            match line {
                Some(n) => {
                    let text =
                        std::fs::read_to_string(dir.join("base.txt")).map_err(|e| e.to_string())?;
                    let mut lines: Vec<String> = text.lines().map(|l| format!("{l}\n")).collect();
                    lines[n as usize] = content(i);
                    std::fs::write(dir.join("base.txt"), lines.concat())
                        .map_err(|e| e.to_string())?;
                }
                None => std::fs::write(dir.join(format!("agent-{i}.txt")), content(i))
                    .map_err(|e| e.to_string())?,
            }
            let out = run_in(
                &dir,
                cli.to_str().unwrap(),
                &["propose", "--intent", &format!("agent {i}")],
                &token,
                url,
            )
            .await?;
            let v: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
            Ok(json!({"repo":r,"patch":v["patch"]}))
        }
    };
    let (mut proposals, proposal_s, replies) = match w.rate {
        Some(rate) => open_phase(rate, (0..w.agents).collect(), propose_one).await,
        None => phase(w.concurrency, (0..w.agents).collect(), propose_one).await,
    };
    if w.cli.is_some() {
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("o5s-cli-{run}")));
    }
    let ids: Vec<(String, u64)> = replies
        .iter()
        .map(|v| {
            (
                v["repo"].as_str().unwrap().to_string(),
                v["patch"].as_u64().unwrap(),
            )
        })
        .collect();
    let (mut reviews, review_s, _) = phase(w.concurrency, (0..ids.len() as u64).collect(), |k| {
        let c = c.clone();
        let (r, id) = ids[k as usize].clone();
        async move {
            c.call(
                reviewer,
                "/api/command",
                Some(json!({"cmd":"approve","repo":r,"patch":id})),
            )
            .await?;
            c.call(
                checker,
                "/api/command",
                Some(json!({"cmd":"check","repo":r,"patch":id,"passed":true})),
            )
            .await
        }
    })
    .await;
    let mut by_repo: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    for (r, id) in &ids {
        by_repo.entry(r.clone()).or_default().push(*id);
    }
    let mut batches: Vec<(String, Vec<u64>)> = Vec::new();
    for (r, list) in by_repo {
        for chunk in list.chunks(w.batch) {
            batches.push((r.clone(), chunk.to_vec()));
        }
    }
    let (mut merges, merge_s, _) = phase(
        if w.workload == "repositories" {
            w.concurrency
        } else {
            1
        },
        (0..batches.len() as u64).collect(),
        |k| {
            let c = c.clone();
            let (r, list) = batches[k as usize].clone();
            async move {
                c.call(
                    reviewer,
                    "/api/command",
                    Some(json!({"cmd":"merge_batch","repo":r,"channel":"main","patches":list})),
                )
                .await
            }
        },
    )
    .await;
    let integrate_s = started.elapsed().as_secs_f64();
    // Check the admitted bytes of every repository (at most 50 sampled).
    let mut exact = true;
    for r in repos
        .iter()
        .take(if w.workload == "repositories" { 50 } else { 1 })
    {
        let view = c
            .call(admin, &format!("/api/view?repo={r}"), None)
            .await
            .unwrap();
        let files = view["files"].as_object().unwrap();
        exact &= match w.workload.as_str() {
            "disjoint" => {
                let mut want: Vec<String> = (0..w.agents * w.spacing).map(expected_line).collect();
                for i in 0..w.agents {
                    want[w.line_of(i) as usize] = content(i);
                }
                files.get("base.txt").and_then(Value::as_str) == Some(&want.concat())
                    && view["conflicts"].as_object().unwrap().is_empty()
            }
            // Every competing replacement is retained as an alternative.
            "overlap" => {
                view["conflicts"]["base.txt"]
                    .as_array()
                    .map(|a| a.len() as u64)
                    == Some(w.agents)
            }
            "files" => files.len() as u64 == w.agents + 0,
            _ => files.len() == 1,
        };
    }
    let failed = proposals.failed + reviews.failed + merges.failed;
    json!({
        "system": "o5s", "transport": "HTTP over loopback",
        "proposals_from": if w.cli.is_some() { "o5s CLI processes in per-agent Git repositories" } else { "HTTP API" },
        "pipeline": "propose; independent approve and check report; merge_batch",
        "offered_proposals_per_second": w.rate,
        "propose": proposals.report(proposal_s), "approve_and_check": reviews.report(review_s),
        "merge_batch": merges.report(merge_s),
        "integrated": if failed == 0 { ids.len() } else { 0 },
        "integrated_per_second": ids.len() as f64 / integrate_s,
        "integrate_seconds": integrate_s,
        "content_matches": exact, "failed": failed,
    })
}

/// Run a program in `dir` as one agent, returning stdout.
async fn run_in(
    dir: &std::path::Path,
    program: &str,
    args: &[&str],
    token: &str,
    url: &str,
) -> Result<String, String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("O5S_URL", url)
        .env("O5S_TOKEN", token)
        .env("HOME", dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn summary(r: &Value) -> String {
    format!(
        "{:>4}: integrated {:>5} of agents, {:>8.1}/s end-to-end, conflicts left to agents {:>4}, retries {:>6}, content ok {}",
        r["system"].as_str().unwrap_or(""),
        r["integrated"],
        r["integrated_per_second"].as_f64().unwrap_or(0.0),
        r["unintegrated_conflicts"].as_u64().unwrap_or(0),
        r["push_attempts"].as_u64().map_or(0, |a| a.saturating_sub(
            r["integrated"].as_u64().unwrap_or(0)
                + r["unintegrated_conflicts"].as_u64().unwrap_or(0)
        )),
        r["content_matches"]
            .as_bool()
            .or(r["remote_content_matches_integrated_changes"].as_bool())
            .unwrap_or(false),
    )
}

#[tokio::main]
async fn main() {
    let mut v: Vec<String> = std::env::args().skip(1).collect();
    let mode = if v.first().is_some_and(|m| !m.starts_with("--")) {
        v.remove(0)
    } else {
        "o5s".into()
    };
    let mut w = Work {
        workload: "disjoint".into(),
        agents: 1000,
        concurrency: 64,
        batch: 100,
        spacing: 1,
        cli: None,
        rate: None,
    };
    let (mut url, mut tenant, mut dir) = ("http://127.0.0.1:3000".to_string(), 1u64, None);
    let (mut sizes, mut chain, mut samples) = (vec![1000u64, 10_000], false, 10usize);
    for pair in v.chunks(2) {
        let value = pair.get(1).cloned().unwrap_or_default();
        match pair[0].as_str() {
            "--url" => url = value,
            "--agents" => w.agents = value.parse().expect("--agents N"),
            "--concurrency" => w.concurrency = value.parse().expect("--concurrency N"),
            "--batch" => w.batch = value.parse().expect("--batch N"),
            "--spacing" => w.spacing = value.parse::<u64>().expect("--spacing N").max(1),
            "--workload" => w.workload = value,
            "--tenant" => tenant = value.parse().expect("--tenant N"),
            "--dir" => dir = Some(std::path::PathBuf::from(value)),
            "--cli" => w.cli = Some(std::fs::canonicalize(value).expect("--cli PATH")),
            "--rate" => w.rate = Some(value.parse().expect("--rate PROPOSALS_PER_SECOND")),
            "--sizes" => {
                sizes = value
                    .split(',')
                    .map(|n| n.parse().expect("--sizes N,N,..."))
                    .collect()
            }
            "--shape" => chain = value == "chain",
            "--samples" => samples = value.parse().expect("--samples N"),
            other => panic!("unknown argument {other}"),
        }
    }
    assert!(
        matches!(
            w.workload.as_str(),
            "disjoint" | "overlap" | "files" | "repositories"
        ),
        "unknown workload"
    );
    let dir = dir.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("o5s-git-baseline-{}", std::process::id()))
    });
    let settings = json!({"workload": w.workload, "agents": w.agents, "concurrency": w.concurrency,
        "batch": w.batch, "spacing": w.spacing, "cli": w.cli.is_some(), "rate": w.rate});
    let report = match mode.as_str() {
        "o5s" => json!({"settings": settings, "o5s": o5s(&w, &url, tenant).await}),
        "git" => json!({"settings": settings, "git": git::run(&w, &dir).await}),
        "history" => {
            let secret =
                std::env::var("H5I_APP_SECRET").expect("H5I_APP_SECRET must sign test tokens");
            let c = std::sync::Arc::new(Client {
                http: reqwest::Client::new(),
                url: url.trim_end_matches('/').into(),
                auth: HmacAuth::new(secret),
                tenant,
            });
            json!({"history": history::run(c, &sizes, chain, samples, w.concurrency).await})
        }
        "compare" => {
            let g = git::run(&w, &dir).await;
            let o = o5s(&w, &url, tenant).await;
            eprintln!("{}\n{}", summary(&g), summary(&o));
            json!({"settings": settings, "git": g, "o5s": o})
        }
        other => panic!("unknown mode {other}: o5s, git, compare or history"),
    };
    let _ = std::fs::remove_dir_all(&dir);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    let failed = report["o5s"]["failed"].as_u64().unwrap_or(0);
    if failed != 0 {
        std::process::exit(1);
    }
}
