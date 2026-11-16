//! The PyPI front against real, unmodified clients: twine (upload), pip and uv (install),
//! plus the upstream proxy-cache (a second server proxying the first).
//!
//! Needs `O5S_PKG_PYTHON` (a Python with pip and twine; default: the shared tools venv)
//! and optionally `O5S_UV`. Skipped when absent.

mod common;

use common::*;
use o5s_server::config::{Access, Upstream};
use std::path::Path;
use std::process::{Command, Output};

fn python() -> Option<String> {
    let p =
        std::env::var("O5S_PKG_PYTHON").unwrap_or_else(|_| "/mnt/scratch/pico2peta/tools/pkgvenv/bin/python".into());
    let ok = Command::new(&p).args(["-c", "import pip, twine"]).output().is_ok_and(|o| o.status.success());
    if ok {
        Some(p)
    } else {
        eprintln!("skipping: no python with pip and twine at {p} (set O5S_PKG_PYTHON)");
        None
    }
}

fn uv() -> Option<String> {
    let p = std::env::var("O5S_UV").unwrap_or_else(|_| "/mnt/scratch/pico2peta/tools/uv/uv".into());
    Command::new(&p).arg("--version").output().is_ok_and(|o| o.status.success()).then_some(p)
}

fn run(c: &mut Command) -> Output {
    // pip/twine/uv temp files stay inside this test's own directory.
    let tmp = std::env::temp_dir().join(format!("o5s-pypi-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let o = c.env("TMPDIR", &tmp).output().unwrap();
    if !o.status.success() {
        eprintln!("{:?} failed:\n{}\n{}", c, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    }
    o
}

fn wheel(py: &str, dir: &Path, name: &str, version: &str, value: &str) -> String {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pypi/make_wheel.py");
    let o = run(Command::new(py).arg("-I").arg(script).arg(dir).args([name, version, value]));
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn twine(py: &str, url: &str, tok: &str, files: &[&str], extra: &[&str]) -> Output {
    run(Command::new(py)
        .args([
            "-m",
            "twine",
            "upload",
            "--non-interactive",
            "--disable-progress-bar",
            "--repository-url",
            url,
            "-u",
            "token",
            "-p",
            tok,
        ])
        .args(extra)
        .args(files))
}

fn index_url(srv: &TestServer, tok: &str, index: &str) -> String {
    format!("http://token:{tok}@{}/pypi/{index}/simple/", srv.addr)
}

fn pip_install(py: &str, index: &str, target: &Path, req: &str) -> Output {
    run(Command::new(py)
        .args([
            "-m",
            "pip",
            "install",
            "--no-input",
            "--disable-pip-version-check",
            "--no-cache-dir",
            "--no-deps",
            "-q",
            "--index-url",
            index,
            "--target",
        ])
        .arg(target)
        .arg(req))
}

fn import_value(py: &str, target: &Path, module: &str) -> String {
    let o = run(Command::new(py)
        .env("PYTHONPATH", target)
        .args(["-c", &format!("import {module}; print({module}.VALUE)")]));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn twine_upload_pip_and_uv_install_and_proxy_cache() {
    let Some(py) = python() else { return };
    let w = tempfile::tempdir().unwrap();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("o5s-pypi-test-{}", std::process::id())));
        }
    }
    let _cleanup = Cleanup;
    let mut up = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "pypi/local", Access::Write));
        cfg.policy.push(grant("reader", "pypi/local", Access::Read));
    });
    let upload_url = format!("{}/pypi/local/", up.url());
    let v1 = wheel(&py, &w.path().join("d1"), "demo-pkg", "1.0", "1");
    let v2 = wheel(&py, &w.path().join("d2"), "demo-pkg", "1.1", "2");
    assert!(twine(&py, &upload_url, ALICE, &[&v1, &v2], &[]).status.success());
    // Readers can't upload; identical re-upload is fine; same filename, other bytes is refused.
    assert!(!twine(&py, &upload_url, READER, &[&v1], &[]).status.success());
    assert!(twine(&py, &upload_url, ALICE, &[&v1], &[]).status.success(), "identical re-upload");
    let other = wheel(&py, &w.path().join("d3"), "demo-pkg", "1.0", "999");
    assert!(!twine(&py, &upload_url, ALICE, &[&other], &[]).status.success(), "file replacement must fail");

    // PEP 658/714: uploaded wheels advertise their core metadata, served as `<file>.metadata`.
    let http = reqwest::blocking::Client::new();
    let page: serde_json::Value = http
        .get(format!("{}/pypi/local/simple/demo-pkg/", up.url()))
        .bearer_auth(READER)
        .header("accept", "application/vnd.pypi.simple.v1+json")
        .send()
        .unwrap()
        .json()
        .unwrap();
    let f =
        page["files"].as_array().unwrap().iter().find(|f| f["filename"] == "demo_pkg-1.0-py3-none-any.whl").unwrap();
    let md_sha = f["core-metadata"]["sha256"].as_str().expect("core-metadata advertised").to_string();
    let md = http
        .get(format!("{}/pypi/local/files/demo-pkg/demo_pkg-1.0-py3-none-any.whl.metadata", up.url()))
        .bearer_auth(READER)
        .send()
        .unwrap();
    assert_eq!(md.status(), 200);
    let md = md.bytes().unwrap();
    assert!(String::from_utf8_lossy(&md).contains("Name: demo-pkg"));
    {
        use sha2::Digest;
        assert_eq!(sha2::Sha256::digest(&md).iter().map(|b| format!("{b:02x}")).collect::<String>(), md_sha);
    }

    // pip: latest, pinned, and with a non-normalized name.
    let t = w.path().join("t1");
    assert!(pip_install(&py, &index_url(&up, READER, "local"), &t, "demo-pkg").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "2");
    let t = w.path().join("t2");
    assert!(pip_install(&py, &index_url(&up, READER, "local"), &t, "Demo_Pkg==1.0").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "1");
    // Without credentials: 401 Basic challenge, so pip fails cleanly.
    let anon = format!("http://{}/pypi/local/simple/", up.addr);
    assert!(!pip_install(&py, &anon, &w.path().join("t3"), "demo-pkg").status.success());

    // uv (its own resolver and PEP 691 client).
    if let Some(uv) = uv() {
        let t = w.path().join("t4");
        let o = run(Command::new(&uv)
            .args([
                "pip",
                "install",
                "--no-cache",
                "--no-deps",
                "--python",
                &py,
                "--index-url",
                &index_url(&up, READER, "local"),
                "--target",
            ])
            .arg(&t)
            .arg("demo-pkg==1.1"));
        assert!(o.status.success());
        assert_eq!(import_value(&py, &t, "demo_pkg"), "2");
    }

    // PEP 592 yank: pip skips a yanked file unless it's pinned exactly.
    let yank_url = format!("{}/pypi/local/-/yank/demo-pkg/demo_pkg-1.1-py3-none-any.whl", up.url());
    assert_eq!(http.put(&yank_url).bearer_auth(READER).body("x").send().unwrap().status(), 403, "readers can't yank");
    assert!(http.put(&yank_url).bearer_auth(ALICE).body("broken build").send().unwrap().status().is_success());
    let page: serde_json::Value = http
        .get(format!("{}/pypi/local/simple/demo-pkg/", up.url()))
        .bearer_auth(READER)
        .header("accept", "application/vnd.pypi.simple.v1+json")
        .send()
        .unwrap()
        .json()
        .unwrap();
    let y =
        page["files"].as_array().unwrap().iter().find(|f| f["filename"] == "demo_pkg-1.1-py3-none-any.whl").unwrap();
    assert_eq!(y["yanked"], "broken build");
    let t = w.path().join("ty1");
    assert!(pip_install(&py, &index_url(&up, READER, "local"), &t, "demo-pkg").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "1", "unpinned install skips the yanked 1.1");
    let t = w.path().join("ty2");
    assert!(pip_install(&py, &index_url(&up, READER, "local"), &t, "demo-pkg==1.1").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "2", "an exact pin still installs it");
    assert!(http.delete(&yank_url).bearer_auth(ALICE).send().unwrap().status().is_success());
    let t = w.path().join("ty3");
    assert!(pip_install(&py, &index_url(&up, READER, "local"), &t, "demo-pkg").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "2", "unyanked");

    // Proxy-cache: a second server whose index "mirror" proxies the first.
    let upstream = format!("{}/pypi/local/simple", up.url());
    let proxy = TestServer::start(move |cfg| {
        cfg.policy.push(grant("reader", "pypi/mirror", Access::Read));
        cfg.policy.push(grant("alice", "pypi/mirror", Access::Write));
        cfg.pypi.upstream.insert(
            "mirror".into(),
            Upstream { url: upstream, token: Some(READER.into()), ttl_secs: 600, file_hosts: vec![] },
        );
    });
    // A local upload to the mirror index sits next to the proxied files.
    let own = wheel(&py, &w.path().join("d4"), "mine", "0.1", "7");
    assert!(twine(&py, &format!("{}/pypi/mirror/", proxy.url()), ALICE, &[&own], &[]).status.success());
    let t = w.path().join("t5");
    assert!(pip_install(&py, &index_url(&proxy, READER, "mirror"), &t, "demo-pkg==1.0").status.success());
    assert_eq!(import_value(&py, &t, "demo_pkg"), "1");
    let t = w.path().join("t6");
    assert!(pip_install(&py, &index_url(&proxy, READER, "mirror"), &t, "mine").status.success());
    assert_eq!(import_value(&py, &t, "mine"), "7");
    // The proxy passes the upstream's core metadata through (and caches it on first use).
    let mpage: serde_json::Value = http
        .get(format!("{}/pypi/mirror/simple/demo-pkg/", proxy.url()))
        .bearer_auth(READER)
        .header("accept", "application/vnd.pypi.simple.v1+json")
        .send()
        .unwrap()
        .json()
        .unwrap();
    let mf =
        mpage["files"].as_array().unwrap().iter().find(|f| f["filename"] == "demo_pkg-1.0-py3-none-any.whl").unwrap();
    assert_eq!(mf["core-metadata"]["sha256"].as_str(), Some(md_sha.as_str()));
    let pm = http
        .get(format!("{}/pypi/mirror/files/demo-pkg/demo_pkg-1.0-py3-none-any.whl.metadata", proxy.url()))
        .bearer_auth(READER)
        .send()
        .unwrap();
    assert_eq!(pm.status(), 200);
    assert_eq!(pm.bytes().unwrap(), md);

    // The upstream goes away: the cached file is still installable.
    up.stop.take().map(|s| s.send(()));
    drop(up);
    // Zero-grace GC on the proxy first: cached files are reachable through their records.
    let repo = o5s_repo::LocalRepo::open(proxy.dir.path().join("repo")).unwrap();
    let fronts = o5s_server::fronts::Fronts::open(repo).unwrap();
    let opts = o5s_repo::GcOptions {
        grace: std::time::Duration::ZERO,
        allow_short_grace: true,
        repack_all: true,
        ..Default::default()
    };
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let prov = o5s_prov::ProvTracer { inner: o5s_server::fronts::Leaves };
        fronts.repo.gc_with_fronts(&opts, Some(&prov), false).unwrap();
    }
    let t = w.path().join("t7");
    let o = pip_install(&py, &index_url(&proxy, READER, "mirror"), &t, "demo-pkg==1.0");
    assert!(o.status.success(), "cached file must install with the upstream down");
    assert_eq!(import_value(&py, &t, "demo_pkg"), "1");
    // Never fetched: 1.1 is not available offline.
    let o = pip_install(&py, &index_url(&proxy, READER, "mirror"), &w.path().join("t8"), "demo-pkg==1.1");
    assert!(!o.status.success());
}
