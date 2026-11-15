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
    // Deleting a mutable branch works; an immutable tag cannot be deleted.
    ok(&a, &["push", "-q", &u, ":side"]);
    let (pushed, out) = git(&a, &["push", &u, ":refs/tags/v1"]);
    assert!(!pushed, "{out}");
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

fn dir_bytes(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let m = e.metadata().unwrap();
            total += if m.is_dir() { dir_bytes(&e.path()) } else { m.len() };
        }
    }
    total
}

/// Measurement (opt-in): import a real mirror, run a grouped aggressive repack,
/// compare with git's pack size, and verify the repo still clones identically.
///   P2P_GIT_MIRROR=/mnt/scratch/pico2peta/bench-data/mirrors/ripgrep.git \
///   cargo test --release -p pico2peta-server --test git grouped_repack -- --ignored --nocapture
#[test]
#[ignore]
fn grouped_repack_vs_git() {
    let Ok(mirror) = std::env::var("P2P_GIT_MIRROR") else { return };
    let mirror = std::path::PathBuf::from(mirror);
    let s = git_server();
    let u = url(&s, ALICE, "team/m");
    // P2P_GIT_ALL_REFS=1: every ref (incl. refs/pull/*), i.e. the Experiment 1 corpus.
    let all = std::env::var("P2P_GIT_ALL_REFS").is_ok_and(|v| v == "1");
    let specs: Vec<&str> = if all { vec!["refs/*:refs/*"] } else { vec!["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"] };
    let mut args = vec!["push", "-q", &u];
    args.extend(specs.iter().copied());
    ok(&mirror, &args);
    let objects = s.dir.path().join("repo/objects");
    let before = dir_bytes(&objects);
    let repo = pico2peta_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
    repo.flush().unwrap();
    let shared: std::sync::Arc<dyn pico2peta_core::Repository> = std::sync::Arc::new(repo.clone());
    let tracer = pico2peta_prov::ProvTracer {
        inner: pico2peta_server::api::AdapterTracer {
            git: pico2peta_server::git::GitTracer::new(shared.clone()).unwrap(),
            oci: pico2peta_server::oci::OciTracer::new(shared),
        },
    };
    let opts = pico2peta_repo::GcOptions {
        grace: Duration::ZERO,
        allow_short_grace: true,
        repack_all: true,
        ..Default::default()
    };
    let t = std::time::Instant::now();
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(1100));
        let r = repo.gc_with(&opts, &tracer, Some(&pico2peta_server::git::GitGrouper)).unwrap();
        assert!(r.dangling_refs.is_empty(), "{r:?}");
        eprintln!("gc phase: {r:?}");
    }
    let after = dir_bytes(&objects);
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut stack = vec![objects.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                files.push((
                    e.path().strip_prefix(&objects).unwrap().display().to_string(),
                    e.metadata().unwrap().len(),
                ));
            }
        }
    }
    files.sort();
    for (f, n) in &files {
        eprintln!("  {f} {n}");
    }
    // Fair baseline: the same refs (heads + tags) packed by `git gc --aggressive`.
    let b = tempfile::tempdir().unwrap();
    ok(b.path(), &["init", "-q", "--bare", "base"]);
    let base = b.path().join("base");
    let mut fetch = vec!["fetch", "-q", mirror.to_str().unwrap()];
    fetch.extend(specs.iter().copied());
    ok(&base, &fetch);
    ok(&base, &["-c", "pack.threads=4", "gc", "-q", "--aggressive", "--prune=now"]);
    let sum = |ext: &str| -> u64 {
        std::fs::read_dir(base.join("objects/pack"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == ext))
            .map(|e| e.metadata().unwrap().len())
            .sum()
    };
    let (gp, gi) = (sum("pack"), sum("idx"));
    let ours_pack: u64 = files.iter().filter(|f| f.0.ends_with(".pack")).map(|f| f.1).sum();
    eprintln!(
        "RESULT import={before} grouped={after} (packs {ours_pack}) git-aggressive pack={gp} idx={gi} total={} ratio={:.3} payload-ratio={:.3} repack={:.1}s",
        gp + gi,
        after as f64 / (gp + gi) as f64,
        ours_pack as f64 / gp as f64,
        t.elapsed().as_secs_f64()
    );
    // Everything still clones identically.
    let w = tempfile::tempdir().unwrap();
    ok(w.path(), &["clone", "-q", "--mirror", &u, "c"]);
    let list = |d: &Path| {
        let mut a = vec!["for-each-ref", "--format=%(objectname) %(refname)"];
        if !all {
            a.extend(["refs/heads", "refs/tags"]);
        }
        ok(d, &a)
    };
    assert_eq!(list(&mirror), list(&w.path().join("c")));
    ok(&w.path().join("c"), &["fsck", "--full"]);
}

#[test]
fn branch_deletion() {
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
    let u = url(&s, ALICE, "team/del");
    ok(&a, &["branch", "tmp"]);
    ok(&a, &["push", "-q", &u, "main", "tmp"]);
    ok(&a, &["push", "-q", &u, ":tmp"]);
    assert!(s.client(ALICE).resolve(&n("git/team/del/refs/heads/tmp")).unwrap().is_none());
    // Re-create after delete.
    ok(&a, &["push", "-q", &u, "tmp"]);
    assert!(s.client(ALICE).resolve(&n("git/team/del/refs/heads/tmp")).unwrap().is_some());
}
