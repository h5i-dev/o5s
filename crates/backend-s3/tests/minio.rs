//! Integration tests against a real S3-compatible server (MinIO in CI and dev).
//! Skipped unless `O5S_S3_ENDPOINT` is set; see `scripts/minio-test.sh`.

mod common;

use common::{Fault, FaultProxy, backend, config, data, upstream_hostport};
use o5s_backend_s3::S3Config;
use o5s_core::Error;
use o5s_core::backend::{Backend, PutOutcome, PutSource, conformance};
use std::io::Read;
use std::time::{Duration, Instant};

fn read_all(b: &dyn Backend, key: &str) -> Vec<u8> {
    let mut v = Vec::new();
    b.open(key).expect("open").read_to_end(&mut v).expect("read");
    v
}

/// Small multipart settings so tests exercise multipart with modest data.
fn small_mp(mut c: S3Config) -> S3Config {
    c.part_size = 5 * 1024 * 1024;
    c.multipart_threshold = 5 * 1024 * 1024;
    c.multipart_concurrency = 3;
    c
}

#[test]
fn conformance_suite() {
    let env = require_s3!();
    let b = backend(&env, config(&env, &env.endpoint, "conformance"));
    assert!(b.atomic_create());
    conformance::run_all(&b);
}

#[test]
fn conformance_suite_multipart() {
    // Same suite with multipart forced for anything >= 5 MiB (file_source uses 3 MiB, so
    // also lower nothing else); plus explicit multipart checks below.
    let env = require_s3!();
    let b = backend(&env, small_mp(config(&env, &env.endpoint, "conformance-mp")));
    conformance::run_all(&b);
}

#[test]
fn conformance_without_conditional_writes() {
    let env = require_s3!();
    let mut c = config(&env, &env.endpoint, "conformance-nocond");
    c.conditional_writes = false;
    let b = backend(&env, c);
    // HEAD-then-PUT is not race-free: it says so, and `concurrent_creators` is skipped.
    assert!(!b.atomic_create());
    conformance::create_only(&b);
    conformance::ranged_reads(&b);
    conformance::list_and_head(&b);
    conformance::file_source(&b);
    conformance::delete_idempotent(&b);
    conformance::missing_keys(&b);
}

#[test]
fn multipart_bytes_and_file() {
    let env = require_s3!();
    let b = backend(&env, small_mp(config(&env, &env.endpoint, "multipart")));
    let d = data(12 * 1024 * 1024 + 12345, 1);
    assert_eq!(b.put_if_absent("mp/bytes.pack", PutSource::Bytes(&d)).unwrap(), PutOutcome::Created);
    assert_eq!(read_all(&b, "mp/bytes.pack"), d);
    // Range spanning the part 1/2 boundary.
    let p = 5 * 1024 * 1024u64;
    assert_eq!(b.get_range("mp/bytes.pack", p - 100..p + 100).unwrap(), &d[(p - 100) as usize..(p + 100) as usize]);
    assert_eq!(b.head("mp/bytes.pack").unwrap().unwrap().len, d.len() as u64);

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("src.pack");
    let d2 = data(11 * 1024 * 1024, 2);
    std::fs::write(&f, &d2).unwrap();
    assert_eq!(b.put_if_absent("mp/file.pack", PutSource::File(&f)).unwrap(), PutOutcome::Created);
    assert_eq!(read_all(&b, "mp/file.pack"), d2);
    // Create-only holds for multipart too.
    assert_eq!(b.put_if_absent("mp/file.pack", PutSource::Bytes(&d)).unwrap(), PutOutcome::AlreadyExists);
    assert_eq!(read_all(&b, "mp/file.pack"), d2);
}

#[test]
fn multipart_concurrent_creators() {
    let env = require_s3!();
    let b = backend(&env, small_mp(config(&env, &env.endpoint, "mp-race")));
    let bodies: Vec<Vec<u8>> = (0..4u64).map(|i| data(6 * 1024 * 1024, 100 + i)).collect();
    let outcomes: Vec<PutOutcome> = std::thread::scope(|s| {
        let hs: Vec<_> = bodies
            .iter()
            .map(|body| s.spawn(|| b.put_if_absent("race.pack", PutSource::Bytes(body)).unwrap()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outcomes.iter().filter(|o| **o == PutOutcome::Created).count(), 1, "{outcomes:?}");
    assert!(bodies.contains(&read_all(&b, "race.pack")));
}

#[test]
fn list_paginates() {
    let env = require_s3!();
    let b = backend(&env, config(&env, &env.endpoint, "paginate"));
    // MinIO/S3 return at most 1000 keys per page.
    let n = 1100;
    std::thread::scope(|s| {
        for t in 0..8 {
            let b = &b;
            s.spawn(move || {
                for i in (t..n).step_by(8) {
                    b.put_if_absent(&format!("k/{i:05}"), PutSource::Bytes(b"x")).unwrap();
                }
            });
        }
    });
    let l = b.list("k/").unwrap();
    assert_eq!(l.len(), n);
    assert!(l.windows(2).all(|w| w[0].key < w[1].key));
    assert_eq!(l[0].key, "k/00000");
}

#[test]
fn large_stream_bounded_by_chunks() {
    let env = require_s3!();
    let b = backend(&env, small_mp(config(&env, &env.endpoint, "stream")));
    let d = data(20 * 1024 * 1024, 3);
    b.put_if_absent("big.pack", PutSource::Bytes(&d)).unwrap();
    let mut r = b.open("big.pack").unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    let mut hasher = Vec::with_capacity(d.len());
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.extend_from_slice(&buf[..n]);
    }
    assert_eq!(hasher, d);
}

// ---------------------------------------------------------------------------
// Failure injection
// ---------------------------------------------------------------------------

fn proxied(env: &common::Env, test: &str) -> (FaultProxy, S3Config) {
    let proxy = FaultProxy::start(&upstream_hostport(&env.endpoint));
    let mut c = config(env, &proxy.endpoint(), test);
    // No connection reuse: every request (and retry) is a new connection, so the
    // per-connection fault plan maps 1:1 onto requests.
    c.pool_max_idle = 0;
    (proxy, c)
}

#[test]
fn retries_through_5xx_and_throttling() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-5xx");
    let b = backend(&env, c);
    let s0 = b.client().stats().snapshot();
    proxy.push(Fault::Respond(503, "SlowDown"), 2);
    proxy.push(Fault::Respond(500, "InternalError"), 1);
    proxy.push(Fault::Respond(429, "TooManyRequests"), 1);
    assert_eq!(b.put_if_absent("a", PutSource::Bytes(b"hello")).unwrap(), PutOutcome::Created);
    let s = b.client().stats().snapshot();
    assert_eq!(s.retries - s0.retries, 4);
    assert!(s.throttled - s0.throttled >= 3);
    proxy.push(Fault::Respond(502, ""), 2);
    assert_eq!(b.get_range("a", 1..4).unwrap(), b"ell");
}

#[test]
fn retries_through_dropped_connections() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-reset");
    let b = backend(&env, c);
    proxy.push(Fault::Reset, 3);
    assert_eq!(b.put_if_absent("a", PutSource::Bytes(b"x")).unwrap(), PutOutcome::Created);
    proxy.push(Fault::Reset, 2);
    assert_eq!(b.head("a").unwrap().unwrap().len, 1);
    proxy.push(Fault::Reset, 2);
    assert_eq!(b.list("").unwrap().len(), 1);
    assert_eq!(proxy.faults_applied.load(std::sync::atomic::Ordering::SeqCst), 7);
}

#[test]
fn lost_put_response_is_reported_as_created() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-lost");
    let b = backend(&env, c);
    proxy.push(Fault::LoseResponse, 1);
    // The first PUT lands but its response is lost; the retry sees 412 and
    // verifies via ETag that the stored object is ours.
    assert_eq!(b.put_if_absent("a", PutSource::Bytes(b"payload")).unwrap(), PutOutcome::Created);
    assert_eq!(proxy.faults_applied.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(b.client().stats().snapshot().retries, 1);
    assert_eq!(read_all(&b, "a"), b"payload");
    // A different body under the same key is still AlreadyExists even after a lost response.
    proxy.push(Fault::LoseResponse, 1);
    assert_eq!(b.put_if_absent("a", PutSource::Bytes(b"other")).unwrap(), PutOutcome::AlreadyExists);
}

#[test]
fn lost_multipart_complete_is_reported_as_created() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-lost-mp");
    let b = backend(&env, small_mp(c));
    let d = data(6 * 1024 * 1024, 9);
    // HEAD, Create, 2 parts are forwarded; then lose the Complete response.
    proxy.push(Fault::Forward, 4);
    proxy.push(Fault::LoseResponse, 1);
    assert_eq!(b.put_if_absent("mp.pack", PutSource::Bytes(&d)).unwrap(), PutOutcome::Created);
    assert_eq!(proxy.faults_applied.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(b.client().stats().snapshot().retries, 1);
    assert_eq!(read_all(&b, "mp.pack"), d);
}

#[test]
fn truncated_ranged_read_is_retried() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-trunc-range");
    let b = backend(&env, c);
    let d = data(1 << 20, 4);
    b.put_if_absent("a", PutSource::Bytes(&d)).unwrap();
    proxy.push(Fault::TruncateResponse(4096), 2);
    assert_eq!(b.get_range("a", 100..900_000).unwrap(), &d[100..900_000]);
}

#[test]
fn truncated_stream_resumes() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-trunc-stream");
    let b = backend(&env, c);
    let d = data(3 << 20, 5);
    b.put_if_absent("a", PutSource::Bytes(&d)).unwrap();
    // The first GET dies after ~1 MiB; two resume attempts fail too.
    proxy.push(Fault::TruncateResponse(1 << 20), 1);
    proxy.push(Fault::Reset, 2);
    assert_eq!(read_all(&b, "a"), d);
}

#[test]
fn persistent_failure_gives_up_transiently() {
    let env = require_s3!();
    let (proxy, mut c) = proxied(&env, "fault-persistent");
    c.retry.max_attempts = 4;
    let b = backend(&env, c);
    proxy.set_default(Fault::Respond(503, "SlowDown"));
    let t = Instant::now();
    match b.put_if_absent("a", PutSource::Bytes(b"x")) {
        Err(Error::Backend { transient: true, msg }) => assert!(msg.contains("4 attempts"), "{msg}"),
        other => panic!("expected transient backend error, got {other:?}"),
    }
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(proxy.connections.load(std::sync::atomic::Ordering::SeqCst), 4);
}

#[test]
fn permanent_errors_are_not_retried() {
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "fault-403");
    let b = backend(&env, c);
    proxy.push(Fault::Respond(403, "AccessDenied"), 1);
    match b.put_if_absent("a", PutSource::Bytes(b"x")) {
        Err(Error::Backend { transient: false, msg }) => assert!(msg.contains("AccessDenied"), "{msg}"),
        other => panic!("expected permanent error, got {other:?}"),
    }
    assert_eq!(proxy.connections.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn stalled_server_times_out() {
    let env = require_s3!();
    let (proxy, mut c) = proxied(&env, "fault-stall");
    c.request_timeout = Duration::from_millis(300);
    c.retry.max_attempts = 2;
    let b = backend(&env, c);
    proxy.set_default(Fault::Stall);
    let t = Instant::now();
    match b.head("a") {
        Err(Error::Backend { transient: true, msg }) => assert!(msg.contains("timed out"), "{msg}"),
        other => panic!("expected timeout, got {other:?}"),
    }
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[test]
fn wrong_credentials_fail_cleanly() {
    let env = require_s3!();
    common::ensure_bucket(&env);
    let c = config(&env, &env.endpoint, "badcreds");
    let b = o5s_backend_s3::S3Backend::new(
        c,
        std::sync::Arc::new(o5s_backend_s3::Credentials::new("nope", "wrong-secret")),
        1,
    )
    .unwrap();
    assert!(matches!(b.head("a"), Err(Error::Backend { transient: false, .. })));
}

/// The ref log's commit point is an S3 conditional create. A commit whose PUT
/// lands but whose response is lost must be reported exactly once (no Conflict,
/// no duplicate or skipped version), and other hosts must see it.
#[test]
fn ref_log_commit_survives_lost_response() {
    use o5s_core::{Hash, Name, Observed, ReplaceOutcome};
    let env = require_s3!();
    let (proxy, c) = proxied(&env, "reflog-lost");
    let b: std::sync::Arc<dyn Backend> = std::sync::Arc::new(backend(&env, c.clone()));
    assert!(b.atomic_create());
    let rs = o5s_refs::RefStore::open_backend(b.clone(), "refs/").unwrap();
    let n = Name::new("team/main").unwrap();
    let yes = |_: &Hash| Ok(true);
    let (h1, h2, h3) = (Hash::of(b"1"), Hash::of(b"2"), Hash::of(b"3"));
    assert_eq!(rs.replace(&n, &[(n.clone(), Observed::Absent)], h1, &yes).unwrap(), ReplaceOutcome::Ok { version: 1 });
    // Lose the response of the next ref-log record PUT.
    proxy.lose_responses_matching("refs/log/", 1);
    let r = rs.replace(&n, &[(n.clone(), Observed::Version(1))], h2, &yes).unwrap();
    assert_eq!(proxy.faults_applied.load(std::sync::atomic::Ordering::SeqCst), 1, "the fault must have hit a log PUT");
    assert_eq!(r, ReplaceOutcome::Ok { version: 2 }, "a landed commit is reported as committed");
    assert_eq!(rs.resolve(&n).unwrap().map(|v| (v.hash, v.version)), Some((h2, 2)));
    // Another host (fresh handle, direct connection) agrees.
    let other = o5s_refs::RefStore::open_backend(
        std::sync::Arc::new(backend(&env, S3Config { endpoint: env.endpoint.clone(), ..c })),
        "refs/",
    )
    .unwrap();
    assert_eq!(other.resolve(&n).unwrap().map(|v| (v.hash, v.version)), Some((h2, 2)));
    // The sequence continues without a gap or duplicate.
    assert_eq!(
        other.replace(&n, &[(n.clone(), Observed::Version(2))], h3, &yes).unwrap(),
        ReplaceOutcome::Ok { version: 3 }
    );
    assert_eq!(rs.resolve(&n).unwrap().map(|v| v.version), Some(3));
}
