//! End-to-end tests: a real server on a loopback port, driven by the Rust
//! client (`RemoteRepo`) and by raw HTTP.

mod common;
use common::*;
#[test]
fn object_roundtrip_through_client() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let d = data(100_000, 1);
    let h = c.put(&d).unwrap();
    assert_eq!(h, Hash::of(&d));
    assert_eq!(c.get(&h, None).unwrap(), d);
    assert_eq!(c.get(&h, Some(10..20)).unwrap(), &d[10..20]);
    assert_eq!(c.get(&h, Some(5..5)).unwrap(), b"");
    assert_eq!(c.size(&h).unwrap(), d.len() as u64);
    assert!(c.contains(&h).unwrap());
    let missing = Hash::of(b"nope");
    assert!(!c.contains(&missing).unwrap());
    assert!(matches!(c.get(&missing, None), Err(Error::NotFound(x)) if x == missing));
    assert!(matches!(c.get(&h, Some(0..100_001)), Err(Error::InvalidRange { len: 100_000, .. })));
    // Empty object.
    let e = c.put(b"").unwrap();
    assert_eq!(c.get(&e, None).unwrap(), b"");
    // Idempotent put.
    assert_eq!(c.put(&d).unwrap(), h);
    c.flush().unwrap();
}

#[test]
fn streaming_put_and_open_large() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let d = data(24 << 20, 2);
    let h = c.put_stream(&mut &d[..]).unwrap();
    assert_eq!(h, Hash::of(&d));
    let mut r = c.open(&h, None).unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got.len(), d.len());
    assert!(got == d);
    let mut r = c.open(&h, Some(1_000_000..9_000_000)).unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert!(got == d[1_000_000..9_000_000]);
}

#[test]
fn names_rules_and_typed_errors() {
    let s = TestServer::start(|_| {});
    let admin = s.client(ADMIN);
    let alice = s.client(ALICE);
    let h1 = alice.put(b"v1").unwrap();
    let h2 = alice.put(b"v2").unwrap();
    let main = n("alice/main");
    assert_eq!(alice.resolve(&main).unwrap(), None);
    let v1 = match alice.replace(&main, &[(main.clone(), Observed::Absent)], h1).unwrap() {
        ReplaceOutcome::Ok { version } => version,
        o => panic!("{o:?}"),
    };
    assert_eq!(alice.resolve(&main).unwrap().unwrap().hash, h1);
    // A stale read set conflicts and reports the current version.
    match alice.replace(&main, &[(main.clone(), Observed::Absent)], h2).unwrap() {
        ReplaceOutcome::Conflict { stale } => assert_eq!(stale, vec![(main.clone(), Observed::Version(v1))]),
        o => panic!("{o:?}"),
    }
    assert!(matches!(
        alice.replace(&main, &[(main.clone(), Observed::Version(v1))], h2).unwrap(),
        ReplaceOutcome::Ok { .. }
    ));
    // Typed errors survive the wire.
    assert!(matches!(alice.replace(&main, &[], h1), Err(Error::TargetNotInReadSet(x)) if x == main));
    let ghost = Hash::of(b"never stored");
    assert!(matches!(
        alice.replace(&n("alice/x"), &[(n("alice/x"), Observed::Absent)], ghost),
        Err(Error::MissingObject(x)) if x == ghost
    ));
    // Namespace rules (admin only).
    admin.set_rule("alice/releases", Rule::ImmutableAfterPublish).unwrap();
    assert!(admin.rules().unwrap().contains(&("alice/releases".to_string(), Rule::ImmutableAfterPublish)));
    let rel = n("alice/releases/1.0");
    let v = match alice.replace(&rel, &[(rel.clone(), Observed::Absent)], h1).unwrap() {
        ReplaceOutcome::Ok { version } => version,
        o => panic!("{o:?}"),
    };
    assert!(matches!(
        alice.replace(&rel, &[(rel.clone(), Observed::Version(v))], h2),
        Err(Error::Forbidden { rule: Rule::ImmutableAfterPublish, .. })
    ));
    assert!(matches!(admin.set_rule("alice/releases", Rule::Mutable), Err(Error::RuleChangeOnNonEmpty(_))));
    let refs = alice.list_refs("alice").unwrap();
    assert_eq!(refs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["alice/main", "alice/releases/1.0"]);
}

#[test]
fn authentication() {
    let s = TestServer::start(|_| {});
    let r = http().get(format!("{}/v1/whoami", s.url())).send().unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers().get("www-authenticate").is_some());
    let r = http().get(format!("{}/v1/whoami", s.url())).bearer_auth("wrong").send().unwrap();
    assert_eq!(r.status(), 401);
    let body: wire::ErrorBody = r.json().unwrap();
    assert_eq!(body.error.code, wire::codes::UNAUTHENTICATED);
    assert_eq!(s.client(ALICE).whoami().unwrap(), wire::WhoAmI { principal: "alice".into(), authenticated: true });
    assert!(matches!(RemoteRepo::connect(&s.url(), Some("wrong".into())), Err(Error::Unauthorized(_))));
    // Ops endpoints need no credentials.
    assert_eq!(http().get(format!("{}/healthz", s.url())).send().unwrap().status(), 200);
    assert_eq!(http().get(format!("{}/readyz", s.url())).send().unwrap().status(), 200);
}

#[test]
fn anonymous_access_when_enabled() {
    let s = TestServer::start(|c| {
        c.auth.allow_anonymous = true;
        c.policy.push(grant("anonymous", "pub", Access::Read));
    });
    let admin = s.client(ADMIN);
    let h = admin.put(b"hello").unwrap();
    admin.replace(&n("pub/x"), &[(n("pub/x"), Observed::Absent)], h).unwrap();
    let anon = RemoteRepo::new(&s.url(), None, ClientConfig::default()).unwrap();
    assert_eq!(anon.whoami().unwrap().principal, "anonymous");
    assert_eq!(anon.resolve(&n("pub/x")).unwrap().unwrap().hash, h);
    assert_eq!(anon.get(&h, None).unwrap(), b"hello");
    assert!(anon.put(b"spam").is_err());
    assert!(anon.resolve(&n("private/x")).is_err());
}

#[test]
fn authorization_per_namespace() {
    let s = TestServer::start(|_| {});
    let admin = s.client(ADMIN);
    let alice = s.client(ALICE);
    let reader = s.client(READER);
    let h = alice.put(b"x").unwrap();
    alice.replace(&n("alice/a"), &[(n("alice/a"), Observed::Absent)], h).unwrap();
    alice.replace(&n("alice/public/b"), &[(n("alice/public/b"), Observed::Absent)], h).unwrap();
    admin.replace(&n("shared/c"), &[(n("shared/c"), Observed::Absent)], h).unwrap();
    admin.replace(&n("secret/d"), &[(n("secret/d"), Observed::Absent)], h).unwrap();

    let denied = |r: pico2peta_core::Result<_>| matches!(r, Err(Error::Unauthorized(msg)) if msg.starts_with("permission denied"));
    // Readers cannot write anything, not even objects.
    assert!(denied(reader.put(b"y").map(|_| ())));
    assert!(denied(reader.replace(&n("shared/c"), &[(n("shared/c"), Observed::Version(1))], h).map(|_| ())));
    // Alice cannot move names outside her namespace, or read secret ones.
    assert!(denied(alice.replace(&n("shared/c"), &[(n("shared/c"), Observed::Version(1))], h).map(|_| ())));
    assert!(denied(alice.resolve(&n("secret/d")).map(|_| ())));
    // Read-set entries need read access, since conflicts would reveal versions.
    assert!(denied(
        alice
            .replace(&n("alice/a"), &[(n("alice/a"), Observed::Version(1)), (n("secret/d"), Observed::Version(1))], h)
            .map(|_| ())
    ));
    // Prefix grants are segment-aligned.
    assert!(denied(alice.resolve(&n("alicex/a")).map(|_| ())));
    // set_rule needs admin.
    assert!(denied(alice.set_rule("alice/r", Rule::ImmutableAfterPublish)));
    // Listing is filtered to readable names.
    let names: Vec<String> = reader.list_refs("").unwrap().into_iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(names, ["alice/public/b", "shared/c"]);
    assert_eq!(admin.list_refs("").unwrap().len(), 4);
    assert_eq!(reader.get(&h, None).unwrap(), b"x");
}

#[test]
fn audit_log_records_and_chains() {
    let mut s = TestServer::start(|_| {});
    let alice = s.client(ALICE);
    let h = alice.put(b"audited").unwrap();
    alice.replace(&n("alice/a"), &[(n("alice/a"), Observed::Absent)], h).unwrap();
    let _ = alice.replace(&n("alice/a"), &[(n("alice/a"), Observed::Absent)], h).unwrap(); // conflict
    let _ = alice.resolve(&n("secret/x")); // denied
    let _ = alice.get(&h, None).unwrap(); // read (log_reads = true)
    let _ = http().get(format!("{}/v1/whoami", s.url())).bearer_auth("forged").send().unwrap();
    s.shutdown();
    let n_records = pico2peta_server::audit::verify(&s.audit_path()).unwrap();
    assert!(n_records >= 6, "{n_records}");
    let text = std::fs::read_to_string(s.audit_path()).unwrap();
    let recs: Vec<pico2peta_server::audit::AuditRecord> =
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let has = |action: &str, outcome: &str| recs.iter().any(|r| r.action == action && r.outcome == outcome);
    assert!(has("put", "ok"));
    assert!(has("replace", "ok"));
    assert!(has("replace", "conflict"));
    assert!(has("resolve", "denied"));
    assert!(has("get", "ok"));
    assert!(has("auth", "denied"));
    assert!(recs.iter().filter(|r| r.action == "replace").all(|r| r.principal == "alice" && r.target == "alice/a"));
    assert!(recs.iter().all(|r| r.request_id.len() == 32), "every record carries the request id");
    assert!(
        recs.iter().all(|r| r.remote.as_deref().is_some_and(|a| a.starts_with("127.0.0.1:"))),
        "every record carries the client address"
    );
}

#[test]
fn http_ranges_and_headers() {
    let s = TestServer::start(|_| {});
    let h = s.client(ALICE).put(b"0123456789").unwrap();
    let url = format!("{}/v1/objects/{}", s.url(), h.to_hex());
    let get = |range: Option<&str>| {
        let mut rb = http().get(&url).bearer_auth(ALICE);
        if let Some(r) = range {
            rb = rb.header("range", r);
        }
        rb.send().unwrap()
    };
    let r = get(None);
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["accept-ranges"], "bytes");
    assert_eq!(r.headers()["etag"], format!("\"{}\"", h.to_hex()).as_str());
    assert!(r.headers()[wire::REQUEST_ID_HEADER].to_str().unwrap().len() == 32);
    assert_eq!(r.text().unwrap(), "0123456789");
    let r = get(Some("bytes=2-4"));
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 2-4/10");
    assert_eq!(r.text().unwrap(), "234");
    assert_eq!(get(Some("bytes=-3")).text().unwrap(), "789");
    assert_eq!(get(Some("bytes=7-")).text().unwrap(), "789");
    let r = get(Some("bytes=10-"));
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], "bytes */10");
    let r = http().head(&url).bearer_auth(ALICE).send().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-length"], "10");
    // Bad inputs are 400s, not 500s.
    let r = http().get(format!("{}/v1/objects/zzz", s.url())).bearer_auth(ALICE).send().unwrap();
    assert_eq!(r.status(), 400);
    let r = http().get(format!("{}/v1/refs/alice/a%20b", s.url())).bearer_auth(ALICE).send().unwrap();
    assert_eq!(r.status(), 400);
    let r = http().post(format!("{}/v1/replace", s.url())).bearer_auth(ALICE).body("{not json").send().unwrap();
    assert_eq!(r.status(), 400);
}

#[test]
fn upload_limits_and_hash_check() {
    let s = TestServer::start(|c| c.server.max_upload_bytes = 1000);
    let c = s.client(ALICE);
    // Declared too large: rejected before reading.
    assert!(matches!(c.put(&[0u8; 1001]), Err(Error::Backend { msg, .. }) if msg.contains("too large")));
    // Chunked (unknown length) and too large: rejected mid-stream.
    let big = vec![7u8; 5000];
    assert!(c.put_stream(&mut &big[..]).is_err());
    assert!(c.put(&[1u8; 1000]).is_ok());
    // Expected-hash mismatch.
    let r = http()
        .put(format!("{}/v1/objects", s.url()))
        .bearer_auth(ALICE)
        .header(wire::EXPECT_HASH_HEADER, Hash::of(b"other").to_hex())
        .body("data")
        .send()
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<wire::ErrorBody>().unwrap().error.code, wire::codes::HASH_MISMATCH);
}

#[test]
fn metrics_endpoint() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let h = c.put(b"m").unwrap();
    c.get(&h, None).unwrap();
    let _ = http().get(format!("{}/v1/whoami", s.url())).send().unwrap();
    let text = http().get(format!("{}/metrics", s.url())).send().unwrap().text().unwrap();
    assert!(text.contains("p2p_http_requests_total{method=\"PUT\",route=\"/v1/objects\",status=\"200\"} 1"), "{text}");
    assert!(text.contains("p2p_http_request_duration_seconds_bucket"));
    assert!(text.contains("p2p_object_bytes_uploaded_total 1"));
    assert!(text.contains("p2p_auth_failures_total{outcome=\"missing\"} 1"));
    assert!(text.ends_with("# EOF\n"));
}

#[test]
fn overload_is_shed_with_503() {
    let s = TestServer::start(|c| c.server.max_concurrent_requests = 1);
    let c = s.client(ALICE);
    let d = data(32 << 20, 3);
    let h = c.put(&d).unwrap();
    // Hold one request open by not reading its (large) body.
    let held = http().get(format!("{}/v1/objects/{}", s.url(), h.to_hex())).bearer_auth(ALICE).send().unwrap();
    assert_eq!(held.status(), 200);
    let r = http().get(format!("{}/v1/whoami", s.url())).bearer_auth(ALICE).send().unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers()["retry-after"], "1");
    drop(held);
    // The slot frees once the held body is dropped.
    let mut ok = false;
    for _ in 0..50 {
        if http().get(format!("{}/v1/whoami", s.url())).bearer_auth(ALICE).send().unwrap().status() == 200 {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ok);
}

#[test]
fn graceful_shutdown_drains_and_flushes() {
    let mut s = TestServer::start(|c| c.server.shutdown_grace_secs = 10);
    let c = s.client(ALICE);
    let d = data(16 << 20, 4);
    let h = c.put(&d).unwrap();
    c.replace(&n("alice/keep"), &[(n("alice/keep"), Observed::Absent)], h).unwrap();
    let small = c.put(b"written just before shutdown").unwrap();
    // Start a download, then request shutdown while it is in flight.
    let mut r = c.open(&h, None).unwrap();
    let mut first = vec![0u8; 1024];
    r.read_exact(&mut first).unwrap();
    let stop = s.stop.take().unwrap();
    stop.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // In-flight request completes.
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).unwrap();
    assert_eq!(first.len() + rest.len(), d.len());
    s.shutdown();
    // Everything (including the unreferenced object) is durable after exit.
    let repo = pico2peta_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
    assert_eq!(repo.resolve(&n("alice/keep")).unwrap().unwrap().hash, h);
    assert!(repo.contains(&small).unwrap());
}

#[test]
fn s3_backend_end_to_end() {
    let Ok(endpoint) = std::env::var("P2P_S3_ENDPOINT") else {
        eprintln!("skipping: set P2P_S3_ENDPOINT (see scripts/minio-test.sh)");
        return;
    };
    let bucket = std::env::var("P2P_S3_BUCKET").unwrap_or_else(|_| "p2p-test".into());
    let region = std::env::var("P2P_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let creds = pico2peta_backend_s3::Credentials::from_env().expect("AWS_* credentials");
    let client = pico2peta_backend_s3::S3Client::new(
        pico2peta_backend_s3::S3Config::new(&endpoint, &region, &bucket),
        std::sync::Arc::new(creds),
    )
    .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(client.create_bucket()).unwrap();
    let prefix = format!("server-e2e-{}-{}", std::process::id(), fastrand_u64());
    let p2 = prefix.clone();
    let s = TestServer::start(move |c| {
        c.backend = pico2peta_server::config::BackendSection::S3(pico2peta_server::config::S3Section {
            endpoint: endpoint.clone(),
            region: region.clone(),
            bucket: bucket.clone(),
            prefix: p2,
            path_style: true,
            conditional_writes: true,
            max_in_flight: Some(8),
        })
    });
    let c = s.client(ALICE);
    let d = data(3 << 20, 9);
    let h = c.put(&d).unwrap();
    // replace seals the pack, which uploads it to S3.
    c.replace(&n("alice/s3"), &[(n("alice/s3"), Observed::Absent)], h).unwrap();
    assert_eq!(c.get(&h, Some(1000..2000)).unwrap(), &d[1000..2000]);
    let listed = tokio::runtime::Runtime::new().unwrap().block_on(client.list(&format!("{prefix}/"))).unwrap();
    assert!(listed.iter().any(|(k, _)| k.ends_with(".pack")), "{listed:?}");
    let text = http().get(format!("{}/metrics", s.url())).send().unwrap().text().unwrap();
    assert!(text.contains("p2p_s3_requests_total"));
}

fn fastrand_u64() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

#[test]
fn admin_gc_endpoint() {
    let mut s = TestServer::start(|_| {});
    let admin = s.client(ADMIN);
    let alice = s.client(ALICE);
    let h = alice.put(b"live").unwrap();
    alice.replace(&n("alice/live"), &[(n("alice/live"), Observed::Absent)], h).unwrap();
    alice.put(b"garbage").unwrap();
    alice.flush().unwrap();
    assert!(matches!(alice.gc(false), Err(Error::Unauthorized(_))));
    let dry = admin.gc(true).unwrap();
    assert!(dry.dry_run);
    let r = admin.gc(false).unwrap();
    // First invocation only writes a plan; later phases need the grace period.
    assert!(r.planned.is_some(), "{r:?}");
    assert_eq!(r.deleted_packs, 0);
    assert!(r.dangling_refs.is_empty());
    assert_eq!(alice.get(&h, None).unwrap(), b"live");
    let text = http().get(format!("{}/metrics", s.url())).send().unwrap().text().unwrap();
    assert!(text.contains("p2p_gc_runs_total{outcome=\"ok\"} 2"), "{text}");
    s.shutdown();
    let audit = std::fs::read_to_string(s.audit_path()).unwrap();
    assert!(audit.lines().any(|l| l.contains("\"action\":\"gc\"") && l.contains("\"principal\":\"admin\"")));
    assert!(audit.lines().any(|l| l.contains("\"action\":\"gc\"") && l.contains("\"outcome\":\"denied\"")));
}

#[test]
fn replace_many_is_atomic_over_http() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let h1 = c.put(b"one").unwrap();
    let h2 = c.put(b"two").unwrap();
    let (a, b) = (n("alice/a"), n("alice/b"));
    let out = c
        .replace_many(
            &[(a.clone(), h1), (b.clone(), h2)],
            &[(a.clone(), Observed::Absent), (b.clone(), Observed::Absent)],
        )
        .unwrap();
    assert!(matches!(out, pico2peta_core::ReplaceManyOutcome::Ok { ref versions } if versions.len() == 2), "{out:?}");
    // One stale entry: nothing changes.
    let out = c
        .replace_many(
            &[(a.clone(), h2), (b.clone(), h1)],
            &[(a.clone(), Observed::Version(1)), (b.clone(), Observed::Absent)],
        )
        .unwrap();
    assert!(matches!(out, pico2peta_core::ReplaceManyOutcome::Conflict { .. }));
    assert_eq!(c.resolve(&a).unwrap().unwrap().hash, h1);
    assert_eq!(c.resolve(&b).unwrap().unwrap().hash, h2);
    // Authz is per target.
    assert!(matches!(
        c.replace_many(
            &[(a.clone(), h2), (n("shared/x"), h1)],
            &[(a.clone(), Observed::Version(1)), (n("shared/x"), Observed::Absent)]
        ),
        Err(Error::Unauthorized(_))
    ));
    assert_eq!(c.resolve(&a).unwrap().unwrap().hash, h1);
}

#[test]
fn delete_ref_over_http() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let h = c.put(b"d").unwrap();
    let a = n("alice/d");
    c.replace(&a, &[(a.clone(), Observed::Absent)], h).unwrap();
    // Stale read set conflicts; current one deletes.
    assert!(matches!(c.delete_ref(&a, &[(a.clone(), Observed::Absent)]).unwrap(), ReplaceOutcome::Conflict { .. }));
    assert!(matches!(c.delete_ref(&a, &[(a.clone(), Observed::Version(1))]).unwrap(), ReplaceOutcome::Ok { .. }));
    assert_eq!(c.resolve(&a).unwrap(), None);
    // Version sequence continues after re-creation.
    match c.replace(&a, &[(a.clone(), Observed::Absent)], h).unwrap() {
        ReplaceOutcome::Ok { version } => assert!(version > 1, "{version}"),
        o => panic!("{o:?}"),
    }
    assert!(matches!(c.delete_ref(&n("shared/x"), &[(n("shared/x"), Observed::Absent)]), Err(Error::Unauthorized(_))));
    // update_many: atomic set + delete.
    let b = n("alice/e");
    let va = c.resolve(&a).unwrap().unwrap().version;
    let out = c
        .update_many(
            &[(a.clone(), None), (b.clone(), Some(h))],
            &[(a.clone(), Observed::Version(va)), (b.clone(), Observed::Absent)],
        )
        .unwrap();
    assert!(matches!(out, pico2peta_core::ReplaceManyOutcome::Ok { .. }), "{out:?}");
    assert!(c.resolve(&a).unwrap().is_none());
    assert!(c.resolve(&b).unwrap().is_some());
}
