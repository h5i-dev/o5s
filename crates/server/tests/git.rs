//! Git smart HTTP adapter, driven by the real `git` CLI. Skipped if git is absent.

mod common;
use common::*;
use std::path::Path;
use std::process::Command;

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Runs git in `dir`; returns (success, combined output). Protocol version from
/// `P2P_GIT_PROTOCOL` (default 2, git's default; pushes always use v0/v1).
fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    git_v(dir, args, &std::env::var("P2P_GIT_PROTOCOL").unwrap_or_else(|_| "2".into()))
}

fn git_v(dir: &Path, args: &[&str], version: &str) -> (bool, String) {
    let pv = format!("protocol.version={version}");
    let o = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", &pv, "-c", "init.defaultBranch=main"])
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

/// The git oid a server ref points at.
fn oid_of_ref(s: &TestServer, name: &str) -> String {
    use sha1::Digest as _;
    let c = s.client(ALICE);
    let v = c.resolve(&n(name)).unwrap().unwrap();
    hex(&sha1::Sha1::digest(c.get(&v.hash, None).unwrap()))
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
    // The batched form (one oid-map lookup per wave) agrees with the per-object one, in order.
    let wave = vec![tip, canon("HEAD^{tree}"), canon("HEAD~1"), canon("HEAD:f")];
    let many = tracer.children_of_many(&wave, &repo).unwrap();
    for (h, kids) in wave.iter().zip(&many) {
        let mut one = tracer.children_of(h, &repo).unwrap().unwrap_or_default();
        let mut kids = kids.clone();
        one.sort();
        kids.sort();
        assert_eq!(kids, one, "{h}");
    }
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
    let specs: Vec<&str> =
        if all { vec!["refs/*:refs/*"] } else { vec!["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"] };
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
    // Atomic push mixing a delete and an update.
    commit(&a, "f", "2", "two");
    ok(&a, &["push", "-q", "--atomic", &u, "main", ":tmp"]);
    assert!(s.client(ALICE).resolve(&n("git/team/del/refs/heads/tmp")).unwrap().is_none());
    assert_eq!(oid_of_ref(&s, "git/team/del/refs/heads/main"), head(&a), "main moved in the same atomic update");
}

#[test]
fn protocol_v0_and_v2_agree() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q"]);
    commit(&a, "f.txt", "one", "c1");
    ok(&a, &["tag", "-a", "-m", "annotated", "v1"]);
    commit(&a, "f.txt", "two", "c2");
    ok(&a, &["branch", "feature"]);
    let u = url(&s, ALICE, "team/v2");
    ok(&a, &["push", "-q", &u, "main", "feature", "v1"]);
    for v in ["0", "2"] {
        let dir = format!("c{v}");
        let (cloned, out) = git_v(w.path(), &["clone", "-q", &u, &dir], v);
        assert!(cloned, "v{v} clone: {out}");
        let c = w.path().join(&dir);
        assert_eq!(head(&a), head(&c));
        ok(&c, &["fsck", "--full"]);
        // Incremental fetch over the same protocol.
        commit(&a, "f.txt", &format!("three-{v}"), &format!("c3-{v}"));
        ok(&a, &["push", "-q", &u, "main"]);
        let (pulled, out) = git_v(&c, &["pull", "-q", "--ff-only"], v);
        assert!(pulled, "v{v} pull: {out}");
        assert_eq!(head(&a), head(&c));
    }
    // ls-remote with ref-prefix filtering (v2) and peeled tags.
    let (okk, out) = git_v(w.path(), &["ls-remote", &u, "refs/tags/*"], "2");
    assert!(okk, "{out}");
    assert!(out.contains("refs/tags/v1") && out.contains("refs/tags/v1^{}"), "{out}");
    assert!(!out.contains("refs/heads/"), "ref-prefix must filter: {out}");
    // Prove the server really spoke v2.
    let o = Command::new("git")
        .current_dir(w.path())
        .args(["-c", "protocol.version=2", "ls-remote", &u, "refs/tags/*"])
        .env("GIT_TRACE_PACKET", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    let trace = String::from_utf8_lossy(&o.stderr);
    assert!(trace.contains("version 2"), "no v2 advertisement:\n{trace}");
    assert!(trace.contains("command=ls-refs"), "{trace}");
}

/// Shallow clone (`--depth`, `--deepen`, `--shallow-since`) and partial clone
/// (`--filter=blob:none`, with lazy blob fetches on checkout), with the real git client.
#[test]
fn shallow_and_partial_clone() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q"]);
    for i in 0..6 {
        commit(&a, "f", &format!("v{i}\n"), &format!("c{i}"));
        std::fs::write(a.join(format!("big{i}.bin")), vec![i as u8; 10_000]).unwrap();
        ok(&a, &["add", "."]);
        ok(&a, &["commit", "-q", "-m", &format!("blob {i}")]);
    }
    ok(&a, &["push", "-q", &url(&s, ALICE, "team/sh"), "main"]);
    let u = url(&s, READER, "team/sh");
    let count = |d: &Path| ok(d, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();

    // Shallow: depth 1, then deepen by 2, then unshallow.
    let c = w.path().join("shallow");
    ok(w.path(), &["clone", "-q", "--depth", "1", &u, "shallow"]);
    assert_eq!(count(&c), 1);
    assert_eq!(ok(&c, &["rev-parse", "--is-shallow-repository"]).trim(), "true");
    assert_eq!(std::fs::read_to_string(c.join("f")).unwrap(), "v5\n");
    ok(&c, &["fsck", "--no-progress"]);
    ok(&c, &["fetch", "-q", "--deepen", "2"]);
    assert_eq!(count(&c), 3);
    ok(&c, &["fetch", "-q", "--unshallow"]);
    assert_eq!(count(&c), 12);
    assert_eq!(ok(&c, &["rev-parse", "--is-shallow-repository"]).trim(), "false");
    ok(&c, &["fsck", "--no-progress"]);
    // --shallow-since: every commit here is recent, so a past date takes everything...
    ok(w.path(), &["clone", "-q", "--shallow-since=2000-01-01", &u, "since"]);
    assert_eq!(count(&w.path().join("since")), 12);

    // Partial clone: no blobs until needed.
    let p = w.path().join("partial");
    ok(w.path(), &["clone", "-q", "--no-checkout", "--filter=blob:none", &u, "partial"]);
    let missing = ok(&p, &["rev-list", "--objects", "--all", "--missing=print"]);
    let n_missing = missing.lines().filter(|l| l.starts_with('?')).count();
    assert!(n_missing >= 12, "blobs should be missing:\n{missing}");
    // Checkout fetches the needed blobs lazily (non-tip wants, reachable from this repo).
    ok(&p, &["checkout", "-q", "main"]);
    assert_eq!(std::fs::read_to_string(p.join("f")).unwrap(), "v5\n");
    assert_eq!(std::fs::read(p.join("big5.bin")).unwrap(), vec![5u8; 10_000]);
    let after = ok(&p, &["rev-list", "--objects", "--all", "--missing=print"]);
    assert!(after.lines().filter(|l| l.starts_with('?')).count() < n_missing);
    // blob:limit keeps small blobs.
    let l = w.path().join("limit");
    ok(w.path(), &["clone", "-q", "--no-checkout", "--filter=blob:limit=1k", &u, "limit"]);
    let m = ok(&l, &["rev-list", "--objects", "--all", "--missing=print"]);
    assert_eq!(m.lines().filter(|l| l.starts_with('?')).count(), 6, "only the six 10 KB blobs are omitted:\n{m}");

    // Object ids are global across repos: a want for another repo's object is refused.
    let b = w.path().join("b");
    std::fs::create_dir(&b).unwrap();
    ok(&b, &["init", "-q"]);
    commit(&b, "secret", "private\n", "other");
    ok(&b, &["push", "-q", &url(&s, ALICE, "team/other"), "main"]);
    let secret = ok(&b, &["rev-parse", "HEAD:secret"]).trim().to_string();
    let (fetched, out) = git(&p, &["fetch", "-q", "origin", &secret]);
    assert!(!fetched, "fetching another repo's blob must fail:\n{out}");
}

/// `object-format=sha256` repositories (configured by name prefix) with the real git client:
/// push, clone (v2 and v0), fsck, incremental fetch, shallow clone. A SHA-1 push into a
/// SHA-256 repository (and the reverse) is refused.
#[test]
fn sha256_repositories() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = TestServer::start(|c| {
        c.policy.push(grant("alice", "git/team", Access::Write));
        c.git.sha256_repos = vec!["team/s256".into()];
    });
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q", "--object-format=sha256"]);
    for i in 0..4 {
        commit(&a, "f", &format!("v{i}\n"), &format!("c{i}"));
    }
    ok(&a, &["tag", "-a", "-m", "release", "v1"]);
    let u = url(&s, ALICE, "team/s256/repo");
    ok(&a, &["push", "-q", "--follow-tags", &u, "main"]);
    for proto in ["2", "0"] {
        let c = w.path().join(format!("c{proto}"));
        let (cloned, out) = git_v(w.path(), &["clone", "-q", &u, c.to_str().unwrap()], proto);
        assert!(cloned, "clone v{proto}: {out}");
        assert_eq!(ok(&c, &["rev-parse", "--show-object-format"]).trim(), "sha256");
        assert_eq!(ok(&c, &["rev-parse", "HEAD"]), ok(&a, &["rev-parse", "HEAD"]));
        assert_eq!(ok(&c, &["rev-parse", "v1^{commit}"]), ok(&a, &["rev-parse", "v1^{commit}"]));
        ok(&c, &["fsck", "--strict", "--no-progress"]);
    }
    // Incremental fetch.
    commit(&a, "g", "new\n", "c4");
    ok(&a, &["push", "-q", &u, "main"]);
    let c = w.path().join("c2");
    ok(&c, &["pull", "-q", "--ff-only"]);
    assert_eq!(ok(&c, &["rev-parse", "HEAD"]), ok(&a, &["rev-parse", "HEAD"]));
    // Shallow.
    ok(w.path(), &["clone", "-q", "--depth", "1", &u, "sh"]);
    assert_eq!(ok(&w.path().join("sh"), &["rev-list", "--count", "HEAD"]).trim(), "1");
    // Format mismatches are refused.
    let b = w.path().join("b");
    std::fs::create_dir(&b).unwrap();
    ok(&b, &["init", "-q"]);
    commit(&b, "f", "x\n", "sha1 commit");
    let (pushed, out) = git(&b, &["push", "-q", &u, "main:refs/heads/other"]);
    assert!(!pushed, "SHA-1 push into a SHA-256 repo must fail:\n{out}");
    let (pushed, out) = git(&a, &["push", "-q", &url(&s, ALICE, "team/plain"), "main"]);
    assert!(!pushed, "SHA-256 push into a SHA-1 repo must fail:\n{out}");
}

/// Full clones are served from the clone pack cache once one exists, for both protocols, and
/// a push (new wants) makes the next clone encode (and cache) a fresh pack. Clones stay
/// fsck-clean throughout, including after a GC.
#[test]
fn clone_pack_cache() {
    if !have_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let s = git_server();
    let w = tempfile::tempdir().unwrap();
    let a = w.path().join("a");
    std::fs::create_dir(&a).unwrap();
    ok(&a, &["init", "-q", "-b", "main"]);
    for i in 0..5 {
        commit(&a, "f", &format!("v{i}\n"), &format!("c{i}"));
    }
    ok(&a, &["tag", "-a", "v1", "-m", "release"]);
    ok(&a, &["push", "-q", &url(&s, ALICE, "team/cp"), "main", "v1"]);
    let u = url(&s, READER, "team/cp");
    let admin = s.client(ADMIN);
    let cache = n("git/_clonepack/team/cp");
    let wait_cache = |prev: Option<Hash>| -> Hash {
        for _ in 0..100 {
            if let Some(v) = admin.resolve(&cache).unwrap()
                && Some(v.hash) != prev
            {
                return v.hash;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("clone pack never cached");
    };
    let clone_ok = |dir: &str, proto: &str| {
        let (good, out) = git_v(w.path(), &["clone", "-q", "--bare", &u, dir], proto);
        assert!(good, "{out}");
        let d = w.path().join(dir);
        ok(&d, &["fsck", "--full", "--strict", "--no-progress"]);
        ok(&d, &["for-each-ref", "--format=%(refname) %(objectname)"])
    };
    let r1 = clone_ok("c1", "2");
    let p1 = wait_cache(None);
    // The push also persisted the repository's commit graph (derived, behind its own ref).
    let graph = n("git/_commitgraph/team/cp");
    for _ in 0..100 {
        if admin.resolve(&graph).unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(&admin.get(&admin.resolve(&graph).unwrap().unwrap().hash, None).unwrap()[..8], b"P2PCGR01");
    // From the cache: same refs, same pack object afterwards (nothing re-stored).
    assert_eq!(clone_ok("c2", "2"), r1);
    assert_eq!(clone_ok("c3", "0"), r1);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(admin.resolve(&cache).unwrap().unwrap().hash, p1);
    // The cached object is the pack after a header; the clone got exactly those pack bytes.
    let cached = admin.get(&p1, None).unwrap();
    assert_eq!(&cached[..8], b"P2PCPK01");
    assert_eq!(&cached[40..44], b"PACK");
    // A push changes the wants: the next clone encodes and re-caches.
    commit(&a, "g", "new\n", "c5");
    ok(&a, &["push", "-q", &url(&s, ALICE, "team/cp"), "main"]);
    let r4 = clone_ok("c4", "2");
    assert_ne!(r4, r1);
    assert!(r4.contains(&head(&a)));
    let p2 = wait_cache(Some(p1));
    assert_ne!(p2, p1);
    // After a GC the cache still serves a correct clone.
    let _ = admin.gc(false).unwrap();
    assert_eq!(clone_ok("c5", "2"), r4);
    // Incremental fetches never use it (they have haves).
    let c4 = w.path().join("c4");
    commit(&a, "h", "newer\n", "c6");
    ok(&a, &["push", "-q", &url(&s, ALICE, "team/cp"), "main"]);
    ok(&c4, &["fetch", "-q", &u, "+refs/heads/main:refs/heads/main"]);
    ok(&c4, &["fsck", "--full", "--no-progress"]);
    assert_eq!(ok(&c4, &["rev-parse", "main"]).trim(), head(&a));
}
