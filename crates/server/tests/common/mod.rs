//! Shared test harness: a real server on a loopback port.
#![allow(dead_code, unused_imports)]

pub use pico2peta_client::{ClientConfig, RemoteRepo, wire};
pub use pico2peta_core::{Error, Hash, Name, ObjectStore, Observed, ReplaceOutcome, Repository, Rule};
pub use pico2peta_server::config::{Access, AuditSection, AuthSection, Config, PolicyEntry, RepoSection, TokenEntry};
pub use std::io::Read;
pub use std::net::SocketAddr;
pub use std::path::PathBuf;
pub use std::time::Duration;

pub const ADMIN: &str = "tok-admin";
pub const ALICE: &str = "tok-alice";
pub const READER: &str = "tok-reader";

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn token(principal: &str, secret: &str) -> TokenEntry {
    TokenEntry { principal: principal.into(), sha256: hex(&pico2peta_server::auth::sha256(secret.as_bytes())) }
}

pub fn grant(p: &str, ns: &str, a: Access) -> PolicyEntry {
    PolicyEntry { principal: p.into(), namespace: ns.into(), access: a }
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub dir: tempfile::TempDir,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn start(tweak: impl FnOnce(&mut Config)) -> TestServer {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            server: Default::default(),
            repo: RepoSection { path: dir.path().join("repo"), create: true },
            backend: Default::default(),
            auth: AuthSection {
                allow_anonymous: false,
                tokens: vec![token("admin", ADMIN), token("alice", ALICE), token("reader", READER)],
                token_file: None,
            },
            policy: vec![
                grant("admin", "", Access::Admin),
                grant("alice", "alice", Access::Write),
                grant("alice", "shared", Access::Read),
                grant("reader", "shared", Access::Read),
                grant("reader", "alice/public", Access::Read),
            ],
            audit: AuditSection { path: dir.path().join("audit/audit.jsonl"), log_reads: true },
            log: Default::default(),
            gc: Default::default(),
            oci: Default::default(),
            git: Default::default(),
        };
        tweak(&mut cfg);
        let state = pico2peta_server::build_state(&cfg).unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let timeout = Duration::from_secs(cfg.server.request_timeout_secs);
        let grace = Duration::from_secs(cfg.server.shutdown_grace_secs);
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(l.local_addr().unwrap()).unwrap();
                pico2peta_server::serve(l, state, timeout, grace, async {
                    let _ = stop_rx.await;
                })
                .await
                .unwrap();
            });
        });
        let addr = addr_rx.recv().unwrap();
        TestServer { addr, dir, stop: Some(stop_tx), thread: Some(thread) }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn client(&self, tok: &str) -> RemoteRepo {
        RemoteRepo::new(&self.url(), Some(tok.to_string()), ClientConfig::default()).unwrap()
    }

    pub fn audit_path(&self) -> PathBuf {
        self.dir.path().join("audit/audit.jsonl")
    }

    pub fn shutdown(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

pub fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

/// Deterministic pseudo-random bytes.
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}
