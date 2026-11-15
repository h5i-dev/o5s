//! The npm front against the real, unmodified npm CLI: publish (plain and scoped), install,
//! create-once versions, and the upstream proxy-cache (a second server proxying the first).
//! Skipped when `npm`/`node` are absent (`PICO2PETA_NPM` overrides the npm path).

mod common;

use common::*;
use pico2peta_server::config::{Access, Upstream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn npm_bin() -> Option<String> {
    let p = std::env::var("PICO2PETA_NPM").unwrap_or_else(|_| "npm".into());
    let ok = Command::new(&p).arg("--version").output().is_ok_and(|o| o.status.success())
        && Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success());
    if ok {
        Some(p)
    } else {
        eprintln!("skipping: npm/node not installed");
        None
    }
}

struct Npm {
    bin: String,
    work: PathBuf,
}

impl Npm {
    /// `.npmrc` for `registry` with `tok`.
    fn rc(&self, name: &str, srv: &TestServer, reg: &str, tok: &str) -> PathBuf {
        let p = self.work.join(format!("{name}.npmrc"));
        let r = format!("//{}/npm/{reg}/", srv.addr);
        std::fs::write(&p, format!("registry=http:{r}\n{r}:_authToken={tok}\n")).unwrap();
        p
    }
    fn run(&self, dir: &Path, rc: &Path, args: &[&str]) -> Output {
        let cache = self.work.join(format!("cache-{}", fastrand::u64(..)));
        let tmp = self.work.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let o = Command::new(&self.bin)
            .current_dir(dir)
            .env("TMPDIR", &tmp)
            .env("npm_config_update_notifier", "false")
            .arg("--userconfig")
            .arg(rc)
            .arg("--cache")
            .arg(&cache)
            .args(["--no-audit", "--no-fund", "--loglevel", "warn", "--fetch-retries", "0"])
            .args(args)
            .output()
            .unwrap();
        if !o.status.success() {
            eprintln!(
                "npm {args:?} failed:\n{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
        o
    }
    fn package(&self, name: &str, version: &str, value: &str) -> PathBuf {
        let d = self.work.join(format!("src-{}-{version}-{value}", name.replace('/', "_")));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("package.json"),
            format!(r#"{{"name":"{name}","version":"{version}","main":"index.js","license":"MIT"}}"#),
        )
        .unwrap();
        std::fs::write(d.join("index.js"), format!("module.exports = {value};\n")).unwrap();
        d
    }
    /// Installs `spec` into a fresh project and returns what `require(module)` exports.
    fn install(&self, rc: &Path, spec: &str, module: &str) -> Option<String> {
        let d = self.work.join(format!("proj-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("package.json"), r#"{"name":"proj","version":"1.0.0","private":true}"#).unwrap();
        if !self.run(&d, rc, &["install", spec]).status.success() {
            return None;
        }
        let o = Command::new("node")
            .current_dir(&d)
            .args(["-e", &format!("console.log(require('{module}'))")])
            .output()
            .unwrap();
        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
}

#[test]
fn npm_publish_install_and_proxy_cache() {
    let Some(bin) = npm_bin() else { return };
    let w = tempfile::tempdir().unwrap();
    let npm = Npm { bin, work: w.path().to_path_buf() };
    let mut up = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "npm/reg", Access::Write));
        cfg.policy.push(grant("reader", "npm/reg", Access::Read));
    });
    let alice = npm.rc("alice", &up, "reg", ALICE);
    let reader = npm.rc("reader", &up, "reg", READER);
    for (n, v, x) in [("demo-npm", "1.0.0", "1"), ("demo-npm", "1.1.0", "2"), ("@team/util", "0.1.0", "7")] {
        let d = npm.package(n, v, x);
        assert!(npm.run(&d, &alice, &["publish"]).status.success(), "publish {n}@{v}");
    }
    // Readers can't publish; a published version can't be replaced.
    assert!(!npm.run(&npm.package("demo-npm", "2.0.0", "3"), &reader, &["publish"]).status.success());
    assert!(!npm.run(&npm.package("demo-npm", "1.0.0", "999"), &alice, &["publish"]).status.success());

    assert_eq!(npm.install(&reader, "demo-npm", "demo-npm").as_deref(), Some("2"), "latest");
    assert_eq!(npm.install(&reader, "demo-npm@1.0.0", "demo-npm").as_deref(), Some("1"));
    assert_eq!(npm.install(&reader, "@team/util", "@team/util").as_deref(), Some("7"), "scoped");

    // Proxy-cache: a second server whose registry "mirror" proxies the first.
    let upstream = format!("{}/npm/reg", up.url());
    let proxy = TestServer::start(move |cfg| {
        cfg.policy.push(grant("reader", "npm/mirror", Access::Read));
        cfg.policy.push(grant("alice", "npm/mirror", Access::Write));
        cfg.npm.upstream.insert("mirror".into(), Upstream { url: upstream, token: Some(READER.into()), ttl_secs: 1 });
    });
    let m_alice = npm.rc("m-alice", &proxy, "mirror", ALICE);
    let m_reader = npm.rc("m-reader", &proxy, "mirror", READER);
    assert!(npm.run(&npm.package("mine", "0.0.1", "5"), &m_alice, &["publish"]).status.success());
    assert_eq!(npm.install(&m_reader, "demo-npm@1.0.0", "demo-npm").as_deref(), Some("1"));
    assert_eq!(npm.install(&m_reader, "@team/util", "@team/util").as_deref(), Some("7"));
    assert_eq!(npm.install(&m_reader, "mine", "mine").as_deref(), Some("5"));

    // The upstream goes away; GC (zero grace) runs; cached versions still install.
    up.stop.take().map(|s| s.send(()));
    drop(up);
    let repo = pico2peta_repo::LocalRepo::open(proxy.dir.path().join("repo")).unwrap();
    let fronts = pico2peta_server::fronts::Fronts::open(repo).unwrap();
    let opts = pico2peta_repo::GcOptions {
        grace: std::time::Duration::ZERO,
        allow_short_grace: true,
        repack_all: true,
        ..Default::default()
    };
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let prov = pico2peta_prov::ProvTracer { inner: pico2peta_server::fronts::Leaves };
        fronts.repo.gc_with_fronts(&opts, Some(&prov), false).unwrap();
    }
    // `latest` offline is the newest cached version (1.1.0 was never fetched).
    assert_eq!(npm.install(&m_reader, "demo-npm", "demo-npm").as_deref(), Some("1"));
    assert_eq!(npm.install(&m_reader, "@team/util@0.1.0", "@team/util").as_deref(), Some("7"));
    assert_eq!(npm.install(&m_reader, "demo-npm@1.1.0", "demo-npm"), None, "never cached");
}
