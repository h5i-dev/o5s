//! The plain-Git baseline: every agent works in its own clone of one bare
//! repository and integrates the usual way, `git push`, and on rejection
//! `git pull --rebase` and push again. A rebase conflict stops the agent with
//! its change unintegrated, as it would stop an automated agent; a person or
//! agent must then resolve it. Transport is `file://`, which favours Git:
//! o5s requests cross loopback HTTP.
use crate::{Samples, Work, content, expected_line};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::process::Command;

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=agent",
            "-c",
            "user.email=agent@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "pull.rebase=true",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// A bare repository holding one commit: the base file of line workloads, or
/// a README.
async fn origin(dir: &Path, w: &Work) -> PathBuf {
    let bare = dir.join("origin.git");
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(
        dir,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    )
    .await
    .unwrap();
    git(&seed, &["init", "-q", "-b", "main"]).await.unwrap();
    if w.line_workload() {
        std::fs::write(seed.join("base.txt"), w.base()).unwrap();
    } else {
        std::fs::write(seed.join("README"), "seed\n").unwrap();
    }
    git(&seed, &["add", "-A"]).await.unwrap();
    git(&seed, &["commit", "-q", "-m", "seed"]).await.unwrap();
    git(
        &seed,
        &["push", "-q", &format!("file://{}", bare.display()), "main"],
    )
    .await
    .unwrap();
    bare
}

#[derive(Default)]
struct Outcome {
    integrated: bool,
    conflict: bool,
    attempts: u64,
    ms: f64,
}

async fn agent(i: u64, bare: &Path, clone: &Path, w: &Work) -> Result<Outcome, String> {
    let started = Instant::now();
    match w.workload.as_str() {
        "files" | "repositories" => {
            std::fs::write(clone.join(format!("agent-{i}.txt")), content(i)).unwrap()
        }
        _ => {
            let text = std::fs::read_to_string(clone.join("base.txt")).unwrap();
            let mut lines: Vec<String> = text.lines().map(|l| format!("{l}\n")).collect();
            lines[w.line_of(i) as usize] = content(i);
            std::fs::write(clone.join("base.txt"), lines.concat()).unwrap();
        }
    }
    git(clone, &["add", "-A"]).await?;
    git(clone, &["commit", "-q", "-m", &format!("agent {i}")]).await?;
    let remote = format!("file://{}", bare.display());
    let mut o = Outcome::default();
    loop {
        o.attempts += 1;
        if git(clone, &["push", "-q", &remote, "HEAD:main"])
            .await
            .is_ok()
        {
            o.integrated = true;
            break;
        }
        if git(clone, &["pull", "-q", "--rebase", &remote, "main"])
            .await
            .is_err()
        {
            let _ = git(clone, &["rebase", "--abort"]).await;
            o.conflict = true;
            break;
        }
    }
    o.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(o)
}

pub async fn run(w: &Work, dir: &Path) -> Value {
    std::fs::create_dir_all(dir).unwrap();
    let repos: Vec<PathBuf> = if w.workload == "repositories" {
        let mut v = Vec::new();
        for i in 0..w.agents {
            let d = dir.join(format!("repo-{i}"));
            std::fs::create_dir_all(&d).unwrap();
            v.push(origin(&d, w).await);
        }
        v
    } else {
        vec![origin(dir, w).await]
    };
    let bare_of = |i: u64| {
        repos[if w.workload == "repositories" {
            i as usize
        } else {
            0
        }]
        .clone()
    };
    // Setup, not measured: one clone per agent.
    let started = Instant::now();
    let clones: Vec<PathBuf> = stream::iter(0..w.agents)
        .map(|i| {
            let bare = bare_of(i);
            let clone = dir.join(format!("agent-{i}"));
            async move {
                git(
                    Path::new("."),
                    &[
                        "clone",
                        "-q",
                        &format!("file://{}", bare.display()),
                        clone.to_str().unwrap(),
                    ],
                )
                .await
                .unwrap();
                clone
            }
        })
        .buffered(w.concurrency)
        .collect()
        .await;
    let clone_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let outcomes: Vec<Outcome> = stream::iter(0..w.agents)
        .map(|i| {
            let bare = bare_of(i);
            let clone = clones[i as usize].clone();
            async move { agent(i, &bare, &clone, w).await.unwrap() }
        })
        .buffer_unordered(w.concurrency)
        .collect()
        .await;
    let seconds = started.elapsed().as_secs_f64();
    let mut samples = Samples::default();
    let mut attempts = 0;
    let mut max_attempts = 0;
    let mut conflicts = 0;
    for o in &outcomes {
        attempts += o.attempts;
        max_attempts = max_attempts.max(o.attempts);
        if o.integrated {
            samples.ms.push(o.ms);
        }
        if o.conflict {
            conflicts += 1;
        }
    }
    let integrated = samples.ms.len();
    // Check what the remote holds.
    let check = dir.join("check");
    let _ = std::fs::remove_dir_all(&check);
    git(
        Path::new("."),
        &[
            "clone",
            "-q",
            &format!("file://{}", repos[0].display()),
            check.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    let exact = if w.line_workload() {
        let text = std::fs::read_to_string(check.join("base.txt")).unwrap();
        let changed = text
            .lines()
            .enumerate()
            .filter(|(n, l)| *l != expected_line(*n as u64).trim_end())
            .count();
        changed
            == if w.workload == "overlap" {
                1
            } else {
                integrated
            }
    } else if w.workload == "files" {
        std::fs::read_dir(&check)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("agent-")
            })
            .count()
            == integrated
    } else {
        true
    };
    json!({
        "system": "git", "git": "push; on rejection pull --rebase and push again; a rebase conflict stops the agent",
        "transport": "file://", "clone_seconds": clone_seconds,
        "integrate": samples.report(seconds),
        "integrated": integrated, "unintegrated_conflicts": conflicts,
        "push_attempts": attempts, "max_push_attempts": max_attempts,
        "integrated_per_second": integrated as f64 / seconds,
        "remote_content_matches_integrated_changes": exact,
    })
}
