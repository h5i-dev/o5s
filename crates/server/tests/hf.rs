//! The HF front against the real `huggingface_hub` client.
//!
//! Needs a Python with `huggingface_hub` (`PICO2PETA_HF_PYTHON`, default the shared tools
//! venv). Skipped, with a message, when it is absent. CI installs it.

mod common;

use common::*;
use std::process::Command;

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn python() -> Option<String> {
    let p =
        std::env::var("PICO2PETA_HF_PYTHON").unwrap_or_else(|_| "/mnt/scratch/pico2peta/tools/venv/bin/python".into());
    let ok = Command::new(&p).args(["-c", "import huggingface_hub"]).output().is_ok_and(|o| o.status.success());
    if ok {
        Some(p)
    } else {
        eprintln!("skipping: no python with huggingface_hub at {p} (set PICO2PETA_HF_PYTHON)");
        None
    }
}

fn run_client(py: &str, srv: &TestServer, token: &str, repo: &str, xet: bool) -> std::process::Output {
    run_phase(py, srv, token, repo, xet, "write")
}

fn run_phase(py: &str, srv: &TestServer, token: &str, repo: &str, xet: bool, phase: &str) -> std::process::Output {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/hf/client.py");
    let home = srv.dir.path().join(format!("hfhome-{}", repo.replace('/', "_")));
    // Backstop: anything the client leaves in TMPDIR goes away with the test's TempDir.
    let tmp = srv.dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut c = Command::new(py);
    c.env("TMPDIR", &tmp)
        .arg("-I")
        .arg(script)
        .env("HF_ENDPOINT", srv.url())
        .env("HF_TOKEN", token)
        .env("HF_HOME", &home)
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("HF_HUB_DISABLE_PROGRESS_BARS", "1")
        .env("HF_TEST_REPO", repo)
        .env("HF_TEST_PHASE", phase);
    if !xet {
        c.env("HF_HUB_DISABLE_XET", "1");
    }
    c.output().unwrap()
}

#[test]
fn huggingface_hub_round_trip_over_lfs() {
    let Some(py) = python() else { return };
    let srv = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "git/hf/models/alice", pico2peta_server::config::Access::Write));
    });
    let out = run_client(&py, &srv, ALICE, "alice/tiny-model", false);
    let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && so.contains("OK"), "client failed\nstdout:\n{so}\nstderr:\n{se}");

    // An LFS object uploaded but never committed: GC may collect it (negative control).
    let orphan: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let orphan_sha = sha256_hex(&orphan);
    let http = reqwest::blocking::Client::new();
    let batch = |sha: &str, size: usize| -> serde_json::Value {
        http.post(format!("{}/alice/tiny-model.git/info/lfs/objects/batch", srv.url()))
            .bearer_auth(ALICE)
            .json(&serde_json::json!({"operation": "upload", "transfers": ["basic"],
                "objects": [{"oid": sha, "size": size}], "hash_algo": "sha256"}))
            .send()
            .unwrap()
            .json()
            .unwrap()
    };
    let b = batch(&orphan_sha, orphan.len());
    let href = b["objects"][0]["actions"]["upload"]["href"].as_str().unwrap().to_string();
    // The capability URL needs no credentials, but a forged one is refused.
    let forged = format!("{}x", &href[..href.len() - 1]);
    assert_eq!(http.put(&forged).body(orphan.clone()).send().unwrap().status(), 403);
    assert!(http.put(&href).body(orphan.clone()).send().unwrap().status().is_success());
    assert!(batch(&orphan_sha, orphan.len())["objects"][0].get("actions").is_none(), "now present");

    // GC (zero grace, condemn + delete phases) must keep LFS content reachable through pointers.
    let repo = pico2peta_repo::LocalRepo::open(srv.dir.path().join("repo")).unwrap();
    let fronts = pico2peta_server::fronts::Fronts::open(repo.clone()).unwrap();
    let opts = pico2peta_repo::GcOptions {
        grace: std::time::Duration::ZERO,
        allow_short_grace: true,
        repack_all: true,
        ..Default::default()
    };
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // The server's GC path: every front's per-run snapshot tracer, plus provenance.
        let prov = pico2peta_prov::ProvTracer { inner: pico2peta_server::fronts::Leaves };
        let r = fronts.repo.gc_with_fronts(&opts, Some(&prov), false).unwrap();
        assert!(r.dangling_refs.is_empty(), "{r:?}");
    }
    assert!(batch(&orphan_sha, orphan.len())["objects"][0].get("actions").is_some(), "orphan collected");
    let out = run_phase(&py, &srv, ALICE, "alice/tiny-model", false, "verify");
    let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && so.contains("OK"), "after GC\nstdout:\n{so}\nstderr:\n{se}");
}

#[test]
fn hf_authorization_follows_the_git_namespace() {
    let Some(py) = python() else { return };
    let srv = TestServer::start(|_| {});
    // alice may write `alice/...` core names, but not `git/hf/models/alice/...`.
    let out = run_client(&py, &srv, ALICE, "alice/denied", false);
    assert!(!out.status.success(), "write without a grant on git/hf/models/alice must fail");
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(se.contains("403") || se.contains("Forbidden"), "{se}");
}

/// One store, two fronts: a repo written through the HF API clones with stock `git`
/// (LFS files appear as pointers), and a `git push` to it is visible through the HF API.
#[test]
fn hf_repo_is_a_git_repo() {
    let Some(py) = python() else { return };
    if !Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: git not installed");
        return;
    }
    let srv = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "git/hf/models/alice", pico2peta_server::config::Access::Write));
    });
    let out = run_client(&py, &srv, ALICE, "alice/both", false);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let w = tempfile::tempdir().unwrap();
    let url = format!("http://u:{ALICE}@{}/git/hf/models/alice/both.git", srv.addr);
    let git = |dir: &std::path::Path, args: &[&str]| {
        let o = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "filter.lfs.smudge=",
                "-c",
                "filter.lfs.process=",
            ])
            .args(args)
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    git(w.path(), &["clone", "-q", &url, "c"]);
    let c = w.path().join("c");
    let ptr = std::fs::read_to_string(c.join("model.safetensors")).unwrap();
    assert!(ptr.starts_with("version https://git-lfs.github.com/spec/v1\noid sha256:"), "{ptr}");
    assert_eq!(std::fs::read_to_string(c.join("sub/a.txt")).unwrap(), "hello\n");
    git(&c, &["fsck", "--strict"]);
    std::fs::write(c.join("README.md"), "# pushed with git\n").unwrap();
    git(&c, &["add", "README.md"]);
    git(&c, &["commit", "-qm", "readme via git"]);
    git(&c, &["push", "-q", "origin", "main"]);
    let r = reqwest::blocking::Client::new()
        .get(format!("{}/alice/both/resolve/main/README.md", srv.url()))
        .bearer_auth(ALICE)
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let head = git(&c, &["rev-parse", "HEAD"]);
    assert_eq!(r.headers()["x-repo-commit"].to_str().unwrap(), head.trim());
    assert_eq!(r.text().unwrap(), "# pushed with git\n");
}

/// The default client path: with `hf_xet` installed, uploads use the Xet CAS protocol
/// (xorbs + shards). Content must round-trip and survive GC like LFS uploads.
#[test]
fn huggingface_hub_round_trip_over_xet() {
    let Some(py) = python() else { return };
    let have_xet = Command::new(&py).args(["-c", "import hf_xet"]).output().is_ok_and(|o| o.status.success());
    if !have_xet {
        eprintln!("skipping: hf_xet not installed");
        return;
    }
    let srv = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "git/hf/models/alice", pico2peta_server::config::Access::Write));
    });
    let out = run_client(&py, &srv, ALICE, "alice/xet-model", true);
    let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && so.contains("OK"), "client failed\nstdout:\n{so}\nstderr:\n{se}");
    // The upload really went through Xet: xorbs were indexed.
    let repo = pico2peta_repo::LocalRepo::open(srv.dir.path().join("repo")).unwrap();
    let fronts = pico2peta_server::fronts::Fronts::open(repo.clone()).unwrap();
    assert!(
        pico2peta_core::Repository::resolve(&repo, &pico2peta_core::Name::new("git/_xet").unwrap()).unwrap().is_some(),
        "no xorbs were uploaded"
    );
    let opts = pico2peta_repo::GcOptions {
        grace: std::time::Duration::ZERO,
        allow_short_grace: true,
        repack_all: true,
        ..Default::default()
    };
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // The server's GC path: every front's per-run snapshot tracer, plus provenance.
        let prov = pico2peta_prov::ProvTracer { inner: pico2peta_server::fronts::Leaves };
        fronts.repo.gc_with_fronts(&opts, Some(&prov), false).unwrap();
    }
    let out = run_phase(&py, &srv, ALICE, "alice/xet-model", true, "verify");
    let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && so.contains("OK"), "after GC\nstdout:\n{so}\nstderr:\n{se}");
}

/// Knowing a sha256 is not a capability: another repo can't commit (and then read) an LFS
/// object that was uploaded to a repo it has no access to.
#[test]
fn lfs_objects_are_scoped_to_their_repo() {
    let Some(py) = python() else { return };
    let srv = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "git/hf/models/alice", pico2peta_server::config::Access::Write));
        cfg.policy.push(grant("reader", "git/hf/models/reader", pico2peta_server::config::Access::Write));
    });
    let out = run_client(&py, &srv, ALICE, "alice/private", false);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let http = reqwest::blocking::Client::new();
    let tree: serde_json::Value = http
        .get(format!("{}/api/models/alice/private/tree/main", srv.url()))
        .bearer_auth(ALICE)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let f = tree.as_array().unwrap().iter().find(|e| e["path"] == "model.safetensors").unwrap();
    let (sha, size) = (f["lfs"]["oid"].as_str().unwrap().to_string(), f["lfs"]["size"].as_u64().unwrap());
    let r = http
        .post(format!("{}/api/repos/create", srv.url()))
        .bearer_auth(READER)
        .json(&serde_json::json!({"name": "thief", "type": "model"}))
        .send()
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let body = [
        serde_json::json!({"key": "header", "value": {"summary": "steal"}}).to_string(),
        serde_json::json!({"key": "lfsFile", "value": {"path": "x.bin", "algo": "sha256", "oid": sha, "size": size}})
            .to_string(),
    ]
    .join("\n");
    let r = http
        .post(format!("{}/api/models/reader/thief/commit/main", srv.url()))
        .bearer_auth(READER)
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status(), 422, "commit referencing another repo's LFS object must fail");
    // The batch API asks the other repo to upload it (proving possession) instead.
    let b: serde_json::Value = http
        .post(format!("{}/reader/thief.git/info/lfs/objects/batch", srv.url()))
        .bearer_auth(READER)
        .json(&serde_json::json!({"operation": "upload", "objects": [{"oid": sha, "size": size}]}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert!(b["objects"][0]["actions"]["upload"].is_object(), "{b}");
}

/// A xorb whose content doesn't hash to its URL is refused, and a xet token is scoped.
#[test]
fn xet_rejects_bad_xorbs_and_tokens() {
    let srv = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "git/hf/models/alice", pico2peta_server::config::Access::Write));
    });
    let http = reqwest::blocking::Client::new();
    let r = http
        .post(format!("{}/api/repos/create", srv.url()))
        .bearer_auth(ALICE)
        .json(&serde_json::json!({"name": "x"}))
        .send()
        .unwrap();
    assert!(r.status().is_success());
    let tok: serde_json::Value = http
        .get(format!("{}/api/models/alice/x/xet-write-token/main", srv.url()))
        .bearer_auth(ALICE)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let (cas, at) = (tok["casUrl"].as_str().unwrap().to_string(), tok["accessToken"].as_str().unwrap().to_string());
    // One uncompressed chunk "hello" under a wrong hash.
    let mut xorb = vec![0u8, 5, 0, 0, 0, 5, 0, 0];
    xorb.extend_from_slice(b"hello");
    let r = http.post(format!("{cas}/v1/xorbs/default/{}", "0".repeat(64))).bearer_auth(&at).body(xorb).send().unwrap();
    assert_eq!(r.status(), 400);
    let r = http.post(format!("{cas}/v1/shards")).bearer_auth("forged.token").body(vec![0u8; 64]).send().unwrap();
    assert_eq!(r.status(), 401);
    // Read-only tokens can't upload.
    let tok: serde_json::Value = http
        .get(format!("{}/api/models/alice/x/xet-read-token/main", srv.url()))
        .bearer_auth(ALICE)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let r = http
        .post(format!("{cas}/v1/shards"))
        .bearer_auth(tok["accessToken"].as_str().unwrap())
        .body(vec![0u8; 64])
        .send()
        .unwrap();
    assert_eq!(r.status(), 403);
    // A reader without a grant gets no token at all.
    let r =
        http.get(format!("{}/api/models/alice/x/xet-write-token/main", srv.url())).bearer_auth(READER).send().unwrap();
    assert!(r.status() == 403 || r.status() == 404, "{}", r.status());
}
