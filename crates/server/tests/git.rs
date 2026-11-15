//! Git smart HTTP adapter, driven by the real `git` CLI. Skipped if git is absent.

mod common;
use common::*;
use std::path::Path;
use std::process::Command;

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Runs git in `dir`; returns (success, combined output).
fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "protocol.version=0",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .expect("run git");
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (s, out) = git(dir, args);
    assert!(s, "git {args:?} failed:\n{out}");
    out
}

fn url(s: &TestServer, tok: &str, repo: &str) -> String {
    format!("http://u:{tok}@{}/git/{repo}.git", s.addr)
}

fn commit(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-qm", msg]);
}

fn head(dir: &Path) -> String {
    ok(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn git_server() -> TestServer {
    TestServer::start(|c| {
        c.policy.push(grant("alice", "git/team", Access::Write));
        c.policy.push(grant("reader", "git/team", Access::Read));
    })
}

#[test]
fn push_clone_fetch_and_conflicts() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q"]);
    commit(&a, "f.txt", "one\n", "c1");
    std::fs::create_dir(a.join("dir")).unwrap();
    commit(&a, "dir/g.txt", &"x".repeat(10_000), "c2");
    let u = url(&s, ALICE, "team/p");
    ok(&a, &["push", "-q", &u, "main"]);

    // Clone as a reader: identical history.
    ok(w.path(), &["clone", "-q", &url(&s, READER, "team/p"), "b"]);
    let b = w.path().join("b");
    assert_eq!(head(&a), head(&b));
    ok(&b, &["fsck", "--full"]);

    // Incremental: A pushes a delta-friendly change; B fetches (negotiation with haves).
    commit(&a, "dir/g.txt", &format!("{}y", "x".repeat(10_000)), "c3");
    ok(&a, &["push", "-q", &u, "main"]);
    ok(&b, &["pull", "-q", "--ff-only"]);
    assert_eq!(head(&a), head(&b));
    ok(&b, &["fsck", "--full"]);

    // Stale push: C diverges from an old base and is rejected.
    ok(w.path(), &["clone", "-q", &u, "c"]);
    let c = w.path().join("c");
    commit(&a, "f.txt", "from a\n", "a4");
    ok(&a, &["push", "-q", &u, "main"]);
    commit(&c, "f.txt", "from c\n", "c4");
    let (pushed, out) = git(&c, &["push", &u, "main"]);
    assert!(!pushed, "stale push must fail: {out}");
    assert!(out.contains("fetch first") || out.contains("rejected"), "{out}");
    // Force push with the right old value (lease) succeeds: refs are mutable by default.
    ok(&c, &["fetch", "-q", "origin"]);
    ok(&c, &["push", "-q", "--force-with-lease=main", "origin", "main"]);
    ok(w.path(), &["clone", "-q", &u, "f"]);
    assert_eq!(head(&c), head(&w.path().join("f")));

    // Readers cannot push; strangers cannot even clone.
    let (pushed, _) = git(&c, &["push", &url(&s, READER, "team/p"), "main:refs/heads/x"]);
    assert!(!pushed);
    let (cloned, _) = git(w.path(), &["clone", "-q", &url(&s, "bad-token", "team/p"), "d"]);
    assert!(!cloned);
    let (cloned, _) = git(w.path(), &["clone", "-q", &url(&s, ALICE, "other/p"), "e"]);
    assert!(!cloned, "outside the granted namespace");
}

#[test]
fn atomic_push_and_immutable_tags() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    s.client(ADMIN).set_rule("git/team/t/refs/tags", Rule::ImmutableAfterPublish).unwrap();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q"]);
    commit(&a, "f", "1", "one");
    let u = url(&s, ALICE, "team/t");
    ok(&a, &["tag", "v1"]);
    ok(&a, &["push", "-q", &u, "main", "v1"]);
    // Moving an immutable tag is refused even with --force.
    commit(&a, "f", "2", "two");
    ok(&a, &["tag", "-f", "v1"]);
    let (pushed, out) = git(&a, &["push", "--force", &u, "refs/tags/v1"]);
    assert!(!pushed, "{out}");
    assert!(out.contains("immutable"), "{out}");
    // Atomic: one stale ref makes the whole push fail, nothing moves.
    ok(&a, &["branch", "side"]);
    let before = s.client(ALICE).resolve(&n("git/team/t/refs/heads/main")).unwrap().unwrap();
    let (pushed, out) = git(&a, &["push", "--atomic", &u, "main", "side", "+refs/tags/v1:refs/tags/v1"]);
    assert!(!pushed, "{out}");
    assert_eq!(s.client(ALICE).resolve(&n("git/team/t/refs/heads/main")).unwrap().unwrap(), before);
    assert!(s.client(ALICE).resolve(&n("git/team/t/refs/heads/side")).unwrap().is_none());
    // Without the bad ref, an atomic push of two branches succeeds.
    ok(&a, &["push", "-q", "--atomic", &u, "main", "side"]);
    assert!(s.client(ALICE).resolve(&n("git/team/t/refs/heads/side")).unwrap().is_some());
    // Deleting refs is not supported.
    let (pushed, _) = git(&a, &["push", &u, ":side"]);
    assert!(!pushed);
}

#[test]
fn gc_tracer_follows_git_objects() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q"]);
    commit(&a, "f", "1", "one");
    commit(&a, "f", "2", "two");
    ok(&a, &["push", "-q", &url(&s, ALICE, "team/g"), "main"]);
    let repo = pico2peta_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
    let tracer = pico2peta_server::git::GitTracer::new(std::sync::Arc::new(repo.clone())).unwrap();
    let tip = repo.resolve(&n("git/team/g/refs/heads/main")).unwrap().unwrap().hash;
    // commit -> tree + parent
    let kids = tracer.children_of(&tip, &repo).unwrap().unwrap();
    assert_eq!(kids.len(), 2);
    let canon = |rev: &str| {
        let kind = ok(&a, &["cat-file", "-t", rev]).trim().to_string();
        let body = Command::new("git").current_dir(&a).args(["cat-file", &kind, rev]).output().unwrap().stdout;
        Hash::of(&[format!("{kind} {}\0", body.len()).as_bytes(), &body].concat())
    };
    assert!(kids.contains(&canon("HEAD^{tree}")));
    assert!(kids.contains(&canon("HEAD~1")));
    assert_eq!(canon("HEAD"), tip, "internal id = BLAKE3 of canonical git bytes");
    // A blob is a leaf.
    assert_eq!(tracer.children_of(&canon("HEAD:f"), &repo).unwrap(), Some(vec![]));
}
