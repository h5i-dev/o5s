//! The Maven front against the real, unmodified `mvn`: Maven fetches its own plugins through
//! a o5s proxy of Maven Central, deploys (release and snapshot) to a hosted repo, and
//! resolves through a second server that proxies the first, including after the first is
//! gone and GC ran. Needs `mvn` (`O5S_MVN`) and network access to Maven Central;
//! skipped otherwise.

mod common;

use common::*;
use o5s_server::config::{Access, Upstream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CENTRAL: &str = "https://repo.maven.apache.org/maven2";

fn mvn_bin() -> Option<String> {
    let candidates = match std::env::var("O5S_MVN") {
        Ok(p) => vec![p],
        Err(_) => vec!["/mnt/scratch/pico2peta/tools/maven/bin/mvn".into(), "mvn".into()],
    };
    let Some(p) = candidates.into_iter().find(|p| Command::new(p).arg("-v").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("skipping: no mvn (set O5S_MVN)");
        return None;
    };
    let up = reqwest::blocking::Client::new()
        .head(format!("{CENTRAL}/org/apache/maven/plugins/maven-deploy-plugin/maven-metadata.xml"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .is_ok_and(|r| r.status().is_success());
    if !up {
        eprintln!("skipping: Maven Central unreachable");
        return None;
    }
    Some(p)
}

struct Mvn {
    bin: String,
    work: PathBuf,
}

impl Mvn {
    /// settings.xml: mirror Central through `central_url`, credentials for both server ids.
    fn settings(&self, name: &str, central_url: &str, tok: &str) -> PathBuf {
        let p = self.work.join(format!("{name}-settings.xml"));
        std::fs::write(
            &p,
            format!(
                r#"<settings>
  <servers>
    <server><id>o5s-central</id><username>token</username><password>{tok}</password></server>
    <server><id>o5s</id><username>token</username><password>{tok}</password></server>
  </servers>
  <mirrors>
    <mirror><id>o5s-central</id><mirrorOf>central</mirrorOf><url>{central_url}</url></mirror>
  </mirrors>
</settings>"#
            ),
        )
        .unwrap();
        p
    }
    /// Runs mvn with its own local repository `local` (fresh dir = nothing cached).
    fn run(&self, settings: &Path, local: &str, args: &[&str]) -> Output {
        let tmp = self.work.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let o = Command::new(&self.bin)
            .current_dir(&self.work)
            .env("MAVEN_OPTS", format!("-Djava.io.tmpdir={}", tmp.display()))
            .args(["-B", "-q", "-ntp", "-s"])
            .arg(settings)
            .arg(format!("-Dmaven.repo.local={}", self.work.join(local).display()))
            .args(args)
            .output()
            .unwrap();
        if !o.status.success() {
            eprintln!(
                "mvn {args:?} failed:\n{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
        o
    }
    fn deploy(&self, settings: &Path, local: &str, url: &str, version: &str, content: &[u8]) -> Output {
        let f = self.work.join(format!("demo-{version}-{}.jar", fastrand::u64(..)));
        std::fs::write(&f, content).unwrap();
        self.run(
            settings,
            local,
            &[
                "deploy:deploy-file",
                &format!("-Dfile={}", f.display()),
                "-DgroupId=org.example.o5s",
                "-DartifactId=demo",
                &format!("-Dversion={version}"),
                "-Dpackaging=jar",
                "-DgeneratePom=true",
                "-DrepositoryId=o5s",
                &format!("-Durl={url}"),
            ],
        )
    }
    /// Resolves the artifact from `url` into a fresh local repo; returns its bytes.
    fn fetch(&self, settings: &Path, local: &str, url: &str, version: &str) -> Option<Vec<u8>> {
        let o = self.run(
            settings,
            local,
            &[
                "dependency:get",
                &format!("-Dartifact=org.example.o5s:demo:{version}"),
                "-Dtransitive=false",
                &format!("-DremoteRepositories=o5s::default::{url}"),
            ],
        );
        if !o.status.success() {
            return None;
        }
        let dir = self.work.join(local).join("org/example/o5s/demo").join(version);
        let jar = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "jar"))?;
        std::fs::read(jar).ok()
    }
}

#[test]
fn mvn_deploy_resolve_and_proxy_cache() {
    let Some(bin) = mvn_bin() else { return };
    let w = tempfile::tempdir().unwrap();
    let mvn = Mvn { bin, work: w.path().to_path_buf() };
    let mut a = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "maven/releases", Access::Write));
        cfg.policy.push(grant("alice", "maven/central", Access::Read));
        cfg.policy.push(grant("reader", "maven", Access::Read));
        cfg.maven
            .upstream
            .insert("central".into(), Upstream { url: CENTRAL.into(), token: None, ttl_secs: 600, file_hosts: vec![] });
    });
    let a_central = format!("{}/maven/central", a.url());
    let a_releases = format!("{}/maven/releases", a.url());
    let alice = mvn.settings("alice", &a_central, ALICE);
    let reader = mvn.settings("reader", &a_central, READER);

    // Deploy a release and two snapshots; Maven's plugins come through A's Central proxy.
    assert!(mvn.deploy(&alice, "m2-deploy", &a_releases, "1.0", b"release one").status.success());
    assert!(mvn.deploy(&alice, "m2-deploy", &a_releases, "1.1-SNAPSHOT", b"snap a").status.success());
    assert!(
        mvn.deploy(&alice, "m2-deploy", &a_releases, "1.1-SNAPSHOT", b"snap b").status.success(),
        "snapshots are mutable"
    );
    // A release can't be overwritten; readers can't deploy.
    assert!(!mvn.deploy(&alice, "m2-deploy", &a_releases, "1.0", b"tampered").status.success());
    assert!(!mvn.deploy(&reader, "m2-deploy", &a_releases, "2.0", b"x").status.success());

    assert_eq!(mvn.fetch(&reader, "m2-r1", &a_releases, "1.0").as_deref(), Some(&b"release one"[..]));
    assert_eq!(
        mvn.fetch(&reader, "m2-r2", &a_releases, "1.1-SNAPSHOT").as_deref(),
        Some(&b"snap b"[..]),
        "latest snapshot"
    );

    // B proxies A: its "central" proxies A's central proxy, its "mirror" proxies A's releases.
    let (ac, ar) = (a_central.clone(), a_releases.clone());
    let b = TestServer::start(move |cfg| {
        cfg.policy.push(grant("reader", "maven", Access::Read));
        let up = |url: String| Upstream { url, token: Some(READER.into()), ttl_secs: 600, file_hosts: vec![] };
        cfg.maven.upstream.insert("central".into(), up(ac));
        cfg.maven.upstream.insert("mirror".into(), up(ar));
    });
    let b_reader = mvn.settings("b-reader", &format!("{}/maven/central", b.url()), READER);
    let b_mirror = format!("{}/maven/mirror", b.url());
    assert_eq!(mvn.fetch(&b_reader, "m2-b1", &b_mirror, "1.0").as_deref(), Some(&b"release one"[..]));

    // A goes away; B runs a zero-grace GC; a fresh client still resolves everything (plugins
    // and the artifact) from B's caches.
    a.stop.take().map(|s| s.send(()));
    drop(a);
    let repo = o5s_repo::LocalRepo::open(b.dir.path().join("repo")).unwrap();
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
    assert_eq!(
        mvn.fetch(&b_reader, "m2-b2", &b_mirror, "1.0").as_deref(),
        Some(&b"release one"[..]),
        "offline from cache"
    );
}
