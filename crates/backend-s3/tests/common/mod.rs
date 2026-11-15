//! Test helpers: environment discovery and a fault-injecting TCP proxy.
#![allow(dead_code)]

use pico2peta_backend_s3::{Credentials, S3Backend, S3Client, S3Config};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct Env {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub creds: Credentials,
}

/// `None` (and a note on stderr) when no S3 endpoint is configured; tests then skip.
pub fn env() -> Option<Env> {
    let endpoint = std::env::var("P2P_S3_ENDPOINT").ok()?;
    let creds = match Credentials::from_env() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("P2P_S3_ENDPOINT set but AWS credentials missing; skipping");
            return None;
        }
    };
    Some(Env {
        endpoint,
        bucket: std::env::var("P2P_S3_BUCKET").unwrap_or_else(|_| "p2p-test".into()),
        region: std::env::var("P2P_S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
        creds,
    })
}

#[macro_export]
macro_rules! require_s3 {
    () => {
        match common::env() {
            Some(e) => e,
            None => {
                eprintln!("skipping: set P2P_S3_ENDPOINT (see scripts/minio-test.sh)");
                return;
            }
        }
    };
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A key prefix unique to this test run and call.
pub fn unique_prefix(test: &str) -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("t-{}-{t}-{}/{test}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
}

pub fn config(env: &Env, endpoint: &str, test: &str) -> S3Config {
    let mut c = S3Config::new(endpoint, &env.region, &env.bucket).with_prefix(unique_prefix(test));
    c.retry.base_delay = Duration::from_millis(5);
    c.retry.throttle_base_delay = Duration::from_millis(10);
    c.retry.max_delay = Duration::from_millis(100);
    c
}

/// Creates the test bucket once per process.
pub fn ensure_bucket(env: &Env) {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let c = S3Client::new(S3Config::new(&env.endpoint, &env.region, &env.bucket), Arc::new(env.creds.clone()))
            .expect("client");
        rt().block_on(c.create_bucket()).expect("create bucket");
    });
}

pub fn backend(env: &Env, cfg: S3Config) -> S3Backend {
    ensure_bucket(env);
    S3Backend::new(cfg, Arc::new(env.creds.clone()), 2).expect("backend")
}

/// A shared runtime for test-side async work (proxy, helpers).
pub fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("rt"))
}

// ---------------------------------------------------------------------------
// Fault-injecting proxy
// ---------------------------------------------------------------------------

/// What the proxy does with the next accepted connection.
#[derive(Clone, Debug)]
pub enum Fault {
    /// Transparent forwarding.
    Forward,
    /// Read the request, answer with this status and S3 error code, close.
    Respond(u16, &'static str),
    /// Close the connection immediately after accepting it.
    Reset,
    /// Accept and never answer.
    Stall,
    /// Forward, but cut the response after this many bytes.
    TruncateResponse(usize),
    /// Forward the request, let the upstream answer, but drop the answer.
    LoseResponse,
}

pub struct FaultProxy {
    pub addr: SocketAddr,
    plan: Arc<Mutex<VecDeque<Fault>>>,
    pub connections: Arc<AtomicUsize>,
    pub faults_applied: Arc<AtomicUsize>,
    /// Fault used once the plan is empty.
    default: Arc<Mutex<Fault>>,
}

impl FaultProxy {
    /// Starts a proxy in front of `upstream` (`host:port`).
    pub fn start(upstream: &str) -> FaultProxy {
        let upstream = upstream.to_string();
        let listener = rt().block_on(TcpListener::bind("127.0.0.1:0")).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let plan = Arc::new(Mutex::new(VecDeque::new()));
        let default = Arc::new(Mutex::new(Fault::Forward));
        let connections = Arc::new(AtomicUsize::new(0));
        let faults_applied = Arc::new(AtomicUsize::new(0));
        let (p, d, c, f) = (plan.clone(), default.clone(), connections.clone(), faults_applied.clone());
        rt().spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                c.fetch_add(1, Ordering::SeqCst);
                let fault = p.lock().unwrap().pop_front().unwrap_or_else(|| d.lock().unwrap().clone());
                if !matches!(fault, Fault::Forward) {
                    f.fetch_add(1, Ordering::SeqCst);
                }
                let up = upstream.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, &up, fault).await;
                });
            }
        });
        FaultProxy { addr, plan, connections, faults_applied, default }
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn push(&self, f: Fault, times: usize) {
        let mut p = self.plan.lock().unwrap();
        for _ in 0..times {
            p.push_back(f.clone());
        }
    }

    pub fn set_default(&self, f: Fault) {
        *self.default.lock().unwrap() = f;
    }
}

/// Reads one HTTP request head + body (by Content-Length) from `sock`.
async fn read_request(sock: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    let head_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let cl: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0);
    while buf.len() < head_end + cl {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(buf)
}

#[allow(deprecated)] // SO_LINGER=0 to send RST is exactly what we want here.
async fn handle(mut sock: TcpStream, upstream: &str, fault: Fault) -> std::io::Result<()> {
    match fault {
        Fault::Reset => {
            let _ = sock.set_linger(Some(Duration::ZERO));
            drop(sock);
            Ok(())
        }
        Fault::Stall => {
            tokio::time::sleep(Duration::from_secs(120)).await;
            Ok(())
        }
        Fault::Respond(status, code) => {
            read_request(&mut sock).await?;
            let body = format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>injected</Message></Error>");
            let resp = format!(
                "HTTP/1.1 {status} Injected\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await?;
            sock.shutdown().await
        }
        Fault::Forward => {
            let mut up = TcpStream::connect(upstream).await?;
            tokio::io::copy_bidirectional(&mut sock, &mut up).await.map(|_| ())
        }
        Fault::TruncateResponse(limit) => {
            let up = TcpStream::connect(upstream).await?;
            let (mut ur, mut uw) = up.into_split();
            let (mut cr, mut cw) = sock.into_split();
            let c2u = tokio::spawn(async move { tokio::io::copy(&mut cr, &mut uw).await });
            let mut sent = 0usize;
            let mut tmp = [0u8; 8192];
            while sent < limit {
                let n = ur.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                let n = n.min(limit - sent);
                cw.write_all(&tmp[..n]).await?;
                sent += n;
            }
            c2u.abort();
            Ok(())
        }
        Fault::LoseResponse => {
            let req = read_request(&mut sock).await?;
            let mut up = TcpStream::connect(upstream).await?;
            up.write_all(&req).await?;
            // Wait for the upstream to start answering (request processed), then drop it.
            let mut tmp = [0u8; 1024];
            let _ = up.read(&mut tmp).await?;
            let _ = sock.set_linger(Some(Duration::ZERO));
            Ok(())
        }
    }
}

pub fn upstream_hostport(endpoint: &str) -> String {
    endpoint.trim_start_matches("http://").trim_end_matches('/').to_string()
}

/// Deterministic pseudo-random bytes.
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}
