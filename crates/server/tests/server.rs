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
    keep(&c, "alice", &h);
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
    keep(&c, "alice", &e);
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
    keep(&c, "alice", &h);
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
    // A trailing `/` names the same prefix, for /v1 listing and search alike.
    assert_eq!(alice.list_refs("alice/").unwrap(), refs);
    assert_eq!(alice.list_refs("alice/releases/").unwrap().len(), 1);
    let hits = alice.search(o5s_client::Search::Prefix("alice/".into())).unwrap();
    let mut found: Vec<String> = hits.into_iter().flat_map(|h| h.names).collect();
    found.sort();
    assert_eq!(found, ["alice/main", "alice/releases/1.0"]);
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

    let denied =
        |r: o5s_core::Result<_>| matches!(r, Err(Error::Unauthorized(msg)) if msg.starts_with("permission denied"));
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
    let n_records = o5s_server::audit::verify(&s.audit_path()).unwrap();
    assert!(n_records >= 6, "{n_records}");
    let text = std::fs::read_to_string(s.audit_path()).unwrap();
    let recs: Vec<o5s_server::audit::AuditRecord> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
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
    keep(&s.client(ALICE), "alice", &h);
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
    keep(&c, "alice", &h);
    c.get(&h, None).unwrap();
    let _ = http().get(format!("{}/v1/whoami", s.url())).send().unwrap();
    let text = http().get(format!("{}/metrics", s.url())).send().unwrap().text().unwrap();
    assert!(text.contains("o5s_http_requests_total{method=\"PUT\",route=\"/v1/objects\",status=\"200\"} 1"), "{text}");
    assert!(text.contains("o5s_http_request_duration_seconds_bucket"));
    assert!(text.contains("o5s_object_bytes_uploaded_total 1"));
    assert!(text.contains("o5s_auth_failures_total{outcome=\"missing\"} 1"));
    assert!(text.ends_with("# EOF\n"));
}

#[test]
fn overload_is_shed_with_503() {
    let s = TestServer::start(|c| c.server.max_concurrent_requests = 1);
    let c = s.client(ALICE);
    let d = data(32 << 20, 3);
    let h = c.put(&d).unwrap();
    keep(&c, "alice", &h);
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
    let repo = o5s_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
    assert_eq!(repo.resolve(&n("alice/keep")).unwrap().unwrap().hash, h);
    assert!(repo.contains(&small).unwrap());
}

#[test]
fn s3_backend_end_to_end() {
    let Ok(endpoint) = std::env::var("O5S_S3_ENDPOINT") else {
        eprintln!("skipping: set O5S_S3_ENDPOINT (see scripts/minio-test.sh)");
        return;
    };
    let bucket = std::env::var("O5S_S3_BUCKET").unwrap_or_else(|_| "o5s-test".into());
    let region = std::env::var("O5S_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let creds = o5s_backend_s3::Credentials::from_env().expect("AWS_* credentials");
    let client = o5s_backend_s3::S3Client::new(
        o5s_backend_s3::S3Config::new(&endpoint, &region, &bucket),
        std::sync::Arc::new(creds),
    )
    .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(client.create_bucket()).unwrap();
    let prefix = format!("server-e2e-{}-{}", std::process::id(), fastrand_u64());
    let p2 = prefix.clone();
    let s = TestServer::start(move |c| {
        c.backend = o5s_server::config::BackendSection::S3(o5s_server::config::S3Section {
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
    assert!(text.contains("o5s_s3_requests_total"));
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
    assert!(text.contains("o5s_gc_runs_total{outcome=\"ok\"} 2"), "{text}");
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
    assert!(matches!(out, o5s_core::ReplaceManyOutcome::Ok { ref versions } if versions.len() == 2), "{out:?}");
    // One stale entry: nothing changes.
    let out = c
        .replace_many(
            &[(a.clone(), h2), (b.clone(), h1)],
            &[(a.clone(), Observed::Version(1)), (b.clone(), Observed::Absent)],
        )
        .unwrap();
    assert!(matches!(out, o5s_core::ReplaceManyOutcome::Conflict { .. }));
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
    assert!(matches!(out, o5s_core::ReplaceManyOutcome::Ok { .. }), "{out:?}");
    assert!(c.resolve(&a).unwrap().is_none());
    assert!(c.resolve(&b).unwrap().is_some());
}

#[test]
fn per_principal_rate_limit() {
    let s =
        TestServer::start(|c| c.server.rate_limit = Some(o5s_server::config::RateLimit { per_sec: 1.0, burst: 3.0 }));
    let who = |t: &str| http().get(format!("{}/v1/whoami", s.url())).bearer_auth(t).send().unwrap();
    for _ in 0..3 {
        assert_eq!(who(ALICE).status(), 200);
    }
    let r = who(ALICE);
    assert_eq!(r.status(), 429);
    assert!(r.headers()["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    // Other principals have their own bucket.
    assert_eq!(who(READER).status(), 200);
    // Ops endpoints are not limited.
    assert_eq!(http().get(format!("{}/healthz", s.url())).send().unwrap().status(), 200);
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(who(ALICE).status(), 200, "tokens refill");
    let m = http().get(format!("{}/metrics", s.url())).send().unwrap().text().unwrap();
    assert!(m.contains("o5s_rate_limited_total 1"), "{m}");
}

#[test]
fn separate_metrics_listener() {
    let s = TestServer::start(|c| c.server.metrics_listen = Some("127.0.0.1:0".parse().unwrap()));
    assert_eq!(http().get(format!("{}/metrics", s.url())).send().unwrap().status(), 404);
    // The ops router serves them instead.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let st = s.state.clone();
    let addr = rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(o5s_server::serve_ops(l, st, std::future::pending()));
        a
    });
    let r = http().get(format!("http://{addr}/metrics")).send().unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().unwrap().contains("o5s_http_requests_total"));
}

fn tree_bytes(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let m = e.metadata().unwrap();
                    if m.is_dir() { tree_bytes(&e.path()) } else { m.len() }
                })
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn put_delta_over_http() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let base = data(4 << 20, 11);
    let hb = c.put(&base).unwrap();
    keep(&c, "alice", &hb);
    c.flush().unwrap();
    let repo_dir = s.dir.path().join("repo");
    let before = tree_bytes(&repo_dir);
    // A "fine-tune": a few scattered byte changes at the same offsets.
    let mut v2 = base.clone();
    for i in (0..v2.len()).step_by(65_537) {
        v2[i] ^= 0x5a;
    }
    let h2 = c.put_delta(&hb, &mut &v2[..]).unwrap();
    keep(&c, "alice", &h2);
    assert_eq!(h2, Hash::of(&v2), "identity is the content hash");
    c.flush().unwrap();
    let delta_cost = tree_bytes(&repo_dir) - before;
    assert!(c.get(&h2, None).unwrap() == v2);
    assert_eq!(c.get(&h2, Some(65_537..65_540)).unwrap(), &v2[65_537..65_540]);
    // Random data is incompressible, so a full copy would cost ~4 MiB.
    assert!(delta_cost < 1 << 20, "delta stored {delta_cost} bytes for a 4 MiB object");
    // Unknown base is an error, not a silent full store.
    let missing = Hash::of(b"no such base");
    assert!(c.put_delta(&missing, &mut &v2[..]).is_err());
}

#[test]
fn token_admin_api() {
    let s = TestServer::start(|c| {
        c.auth.token_file = Some(c.repo.path.parent().unwrap().join("tokens.toml"));
    });
    let admin = |m: reqwest::Method, p: &str| http().request(m, format!("{}{p}", s.url())).bearer_auth(ADMIN);
    // Create.
    let r = admin(reqwest::Method::POST, "/v1/admin/tokens")
        .json(&serde_json::json!({"principal": "robot"}))
        .send()
        .unwrap();
    assert_eq!(r.status(), 201);
    let v: serde_json::Value = r.json().unwrap();
    let tok = v["token"].as_str().unwrap().to_string();
    let sha = v["sha256"].as_str().unwrap().to_string();
    let who = |t: &str| http().get(format!("{}/v1/whoami", s.url())).bearer_auth(t).send().unwrap();
    assert_eq!(who(&tok).status(), 200, "new token works immediately");
    assert_eq!(who(ALICE).status(), 200, "inline tokens unaffected");
    // List shows hashes, never secrets.
    let list = admin(reqwest::Method::GET, "/v1/admin/tokens").send().unwrap().text().unwrap();
    assert!(list.contains(&sha) && !list.contains(&tok) && list.contains("\"source\":\"config\""), "{list}");
    // The file holds only the hash and survives a reload.
    let file = std::fs::read_to_string(s.dir.path().join("tokens.toml")).unwrap();
    assert!(file.contains(&sha) && !file.contains(&tok));
    // Non-admins cannot manage tokens.
    let r = http()
        .post(format!("{}/v1/admin/tokens", s.url()))
        .bearer_auth(ALICE)
        .json(&serde_json::json!({"principal": "x"}))
        .send()
        .unwrap();
    assert_eq!(r.status(), 403);
    // Inline tokens cannot be revoked through the API.
    let alice_sha = hex(&o5s_server::auth::sha256(ALICE.as_bytes()));
    assert_eq!(admin(reqwest::Method::DELETE, &format!("/v1/admin/tokens/{alice_sha}")).send().unwrap().status(), 409);
    // Revoke.
    assert_eq!(admin(reqwest::Method::DELETE, &format!("/v1/admin/tokens/{sha}")).send().unwrap().status(), 204);
    assert_eq!(who(&tok).status(), 401, "revoked token stops working");
    assert_eq!(admin(reqwest::Method::DELETE, &format!("/v1/admin/tokens/{sha}")).send().unwrap().status(), 404);
    assert_eq!(who(ALICE).status(), 200);
}

/// Two independent servers (separate local dirs, nothing shared but S3) serve one
/// repository with refs on the backend: OCC holds across hosts.
#[test]
fn two_servers_share_a_repo_via_s3() {
    let Ok(endpoint) = std::env::var("O5S_S3_ENDPOINT") else {
        eprintln!("skipping: set O5S_S3_ENDPOINT (see scripts/minio-test.sh)");
        return;
    };
    let bucket = std::env::var("O5S_S3_BUCKET").unwrap_or_else(|_| "o5s-test".into());
    let region = std::env::var("O5S_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let creds = o5s_backend_s3::Credentials::from_env().expect("AWS_* credentials");
    let admin = o5s_backend_s3::S3Client::new(
        o5s_backend_s3::S3Config::new(&endpoint, &region, &bucket),
        std::sync::Arc::new(creds),
    )
    .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(admin.create_bucket()).unwrap();
    let prefix = format!("multihost-{}-{}", std::process::id(), fastrand_u64());
    let start = || {
        let (e, b, r, p) = (endpoint.clone(), bucket.clone(), region.clone(), prefix.clone());
        TestServer::start(move |c| {
            c.repo.refs = o5s_repo::RefsStore::Backend;
            c.backend = o5s_server::config::BackendSection::S3(o5s_server::config::S3Section {
                endpoint: e,
                region: r,
                bucket: b,
                prefix: p,
                path_style: true,
                conditional_writes: true,
                max_in_flight: Some(8),
            })
        })
    };
    let (s1, s2) = (start(), start());
    let (c1, c2) = (s1.client(ALICE), s2.client(ALICE));
    // An object written through server 1 can be referenced through server 2.
    let h = c1.put(b"shared content").unwrap();
    c1.flush().unwrap();
    let name = n("alice/shared");
    assert!(matches!(c2.replace(&name, &[(name.clone(), Observed::Absent)], h).unwrap(), ReplaceOutcome::Ok { .. }));
    assert_eq!(c1.resolve(&name).unwrap().unwrap().hash, h, "server 1 sees server 2's commit");
    assert_eq!(c1.get(&h, None).unwrap(), b"shared content");
    // Contention across hosts: no lost updates, no double commits.
    let hot = n("alice/counter");
    let per = 15;
    let (commits, conflicts) = std::thread::scope(|sc| {
        let hs: Vec<_> = [&s1, &s2, &s1, &s2]
            .iter()
            .map(|s| {
                let c = s.client(ALICE);
                let hot = hot.clone();
                sc.spawn(move || {
                    let (mut ok, mut conflicts) = (0, 0);
                    while ok < per {
                        let obs = o5s_core::RefValue::observed(c.resolve(&hot).unwrap());
                        match c.replace(&hot, &[(hot.clone(), obs)], h).unwrap() {
                            ReplaceOutcome::Ok { .. } => ok += 1,
                            ReplaceOutcome::Conflict { .. } => conflicts += 1,
                        }
                    }
                    (ok, conflicts)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    });
    let v1 = c1.resolve(&hot).unwrap().unwrap().version;
    let v2 = c2.resolve(&hot).unwrap().unwrap().version;
    eprintln!("multi-host: commits={commits} conflicts={conflicts} final version={v1}");
    assert_eq!(v1, v2, "both servers agree");
    assert_eq!(v1 as usize, commits, "every commit is exactly one version");
}

#[test]
fn refs_engine_mismatch_is_refused() {
    let d = tempfile::tempdir().unwrap();
    o5s_repo::LocalRepo::init(d.path().join("r"), &Default::default()).unwrap();
    let mut cfg: o5s_server::config::Config = toml::from_str(&format!(
        "[repo]\npath = {:?}\nrefs = \"backend\"\n[audit]\npath = {:?}\n",
        d.path().join("r"),
        d.path().join("a.jsonl")
    ))
    .unwrap();
    cfg.auth.allow_anonymous = true;
    let e = o5s_server::build_state(&cfg).err().expect("must refuse");
    assert!(e.to_string().contains("created with refs"), "{e}");
}

#[test]
fn backend_refs_require_conditional_writes() {
    let cfg: Result<o5s_server::config::Config, _> = toml::from_str(
        "[repo]\npath = \"/tmp/x\"\nrefs = \"backend\"\n[backend]\ntype = \"s3\"\nendpoint = \"http://127.0.0.1:9\"\nregion = \"r\"\nbucket = \"b\"\nconditional_writes = false\n[audit]\npath = \"/tmp/a\"\n",
    );
    let e = cfg.unwrap().validate().unwrap_err();
    assert!(e.to_string().contains("conditional_writes"), "{e}");
}

/// Named uploads use the namespace's chunker (API v1.11): Xet for `alice/models/`,
/// FastCDC otherwise. Verified by counting the chunk records the pack store wrote.
#[test]
fn put_for_name_uses_namespace_chunker() {
    use o5s_chunk::{FastCdcChunker, XetChunker, chunk_lengths};
    let s = TestServer::start(|c| {
        let mut prefixes = std::collections::BTreeMap::new();
        prefixes.insert("alice/models/".to_string(), o5s_repo::ChunkerChoice::Xet);
        let rc = o5s_repo::RepoConfig {
            format: o5s_repo::FORMAT_VERSION,
            pack: (&o5s_pack::PackConfig::default()).into(),
            refs: Default::default(),
            chunking: o5s_repo::ChunkingSection { default: o5s_repo::ChunkerChoice::Fastcdc, prefixes },
            profiles: Default::default(),
            namespaces: Default::default(),
            read: Default::default(),
            gc: Default::default(),
        };
        o5s_repo::LocalRepo::init_config(&c.repo.path, &rc).unwrap();
    });
    let c = s.client(ALICE);
    let chunk_records = || {
        let r = o5s_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
        r.pack_stats().unwrap().blobs
    };
    let d1 = data(4 << 20, 21);
    let d2 = data(4 << 20, 22);
    let before = chunk_records();
    let h1 = c.put_stream_for(&n("alice/models/m1"), &mut &d1[..]).unwrap();
    keep(&c, "alice", &h1);
    c.flush().unwrap();
    let xet = chunk_records() - before;
    let h2 = c.put_stream_for(&n("alice/code/x"), &mut &d2[..]).unwrap();
    keep(&c, "alice", &h2);
    c.flush().unwrap();
    let fast = chunk_records() - before - xet;
    assert_eq!(xet, chunk_lengths(&mut XetChunker::new(), &d1).len(), "Xet chunk count for alice/models/");
    assert_eq!(fast, chunk_lengths(&mut FastCdcChunker::pack_default(), &d2).len(), "FastCDC elsewhere");
    assert!(c.get(&h1, None).unwrap() == d1 && c.get(&h2, None).unwrap() == d2);
    // Writing for a name needs write access to that name.
    assert!(matches!(s.client(READER).put_stream_for(&n("shared/x"), &mut &b"x"[..]), Err(Error::Unauthorized(_))));
}

/// `group_width` stores chunks byte-grouped (API v1.12): bf16-like data takes less space
/// than the same kind of data stored plain, and reads back exactly.
#[test]
fn grouped_upload_saves_space_on_bf16_like_data() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    // bf16-like: low-entropy high bytes (sign/exponent), random low bytes.
    let bf16 = |seed: u64| -> Vec<u8> {
        let r = data(4 << 20, seed);
        r.chunks(2).flat_map(|p| [p[0], 0x3c + (p[1] & 0x03)]).collect()
    };
    let size = || tree_bytes(&s.dir.path().join("repo"));
    let (a, b) = (bf16(31), bf16(32));
    c.flush().unwrap();
    let t0 = size();
    let ha = c.put_stream(&mut &a[..]).unwrap();
    c.flush().unwrap();
    let plain = size() - t0;
    let hb = o5s_core::ObjectStore::put_stream_grouped(&c, &mut &b[..], 2).unwrap();
    c.flush().unwrap();
    let grouped = size() - t0 - plain;
    eprintln!("plain {plain} B, grouped {grouped} B");
    // Deterministic data: ~7.5% measured; assert a margin that proves the codec ran.
    assert!(grouped < plain * 96 / 100, "grouping should save > 4% here: plain {plain}, grouped {grouped}");
    keep(&c, "alice", &ha);
    keep(&c, "alice", &hb);
    assert!(c.get(&ha, None).unwrap() == a && c.get(&hb, None).unwrap() == b);
    assert_eq!(hb, Hash::of(&b), "identity unaffected by the codec");
    // Bad widths are rejected.
    let r = http().put(format!("{}/v1/objects?group_width=0", s.url())).bearer_auth(ALICE).body("x").send().unwrap();
    assert_eq!(r.status(), 400);
}

/// An early rejection (here: authentication, before the body is read) must tell HTTP/1
/// clients not to reuse the connection: hyper closes it because of the unread body.
#[test]
fn early_rejection_closes_the_connection() {
    let s = TestServer::start(|_| {});
    let http = reqwest::blocking::Client::new();
    // 64 KiB: unread by the server, but small enough for the client to finish writing (a
    // bigger body races the server's close and surfaces as a broken pipe instead).
    let r = http.put(format!("{}/v1/objects", s.url())).bearer_auth("wrong").body(vec![7u8; 64 << 10]).send().unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.headers().get("connection").map(|v| v.to_str().unwrap()), Some("close"));
    // A request whose body was read keeps the connection.
    let r = http.put(format!("{}/v1/objects", s.url())).bearer_auth(ADMIN).body(vec![7u8; 1 << 16]).send().unwrap();
    assert!(r.status().is_success());
    assert!(r.headers().get("connection").is_none());
}

/// `?verify=strict|eof` on object reads (API v1.17): strict always works, eof only when the
/// repository's `[read]` config allows it, and anything else is a 400.
#[test]
fn read_verify_parameter_follows_repo_policy() {
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let get = |s: &TestServer, h: &o5s_core::Hash, q: &str| {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/objects/{}{q}", s.url(), h.to_hex()))
            .bearer_auth(ADMIN)
            .send()
            .unwrap()
    };
    // Default repository: eof refused.
    let s = TestServer::start(|_| {});
    let h = s.client(ADMIN).put(&data).unwrap();
    keep(&s.client(ADMIN), "admin", &h);
    assert_eq!(get(&s, &h, "?verify=strict").bytes().unwrap().as_ref(), data.as_slice());
    assert_eq!(get(&s, &h, "?verify=eof").status(), 403);
    assert_eq!(get(&s, &h, "?verify=fast").status(), 400);
    let mut out = Vec::new();
    let r = s.client(ADMIN).open_verified(&h, None, o5s_core::ReadVerify::Eof);
    assert!(r.is_err(), "RemoteRepo surfaces the refusal");
    s.client(ADMIN).open_verified(&h, None, o5s_core::ReadVerify::Strict).unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
    // A repository that allows eof reads.
    let s = TestServer::start(|cfg| {
        let mut rc = o5s_repo::RepoConfig::default();
        rc.read.allow_eof = true;
        o5s_repo::LocalRepo::init_config(&cfg.repo.path, &rc).unwrap();
    });
    let h = s.client(ADMIN).put(&data).unwrap();
    keep(&s.client(ADMIN), "admin", &h);
    let r = get(&s, &h, "?verify=eof");
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().unwrap().as_ref(), data.as_slice());
    let mut out = Vec::new();
    s.client(ADMIN).open_verified(&h, None, o5s_core::ReadVerify::Eof).unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
}

/// Slowloris: a client that never finishes its request headers is disconnected after
/// `HEADER_READ_TIMEOUT` (30 s); so is an idle keep-alive connection.
#[test]
fn slow_headers_and_idle_connections_are_closed() {
    use std::io::{Read, Write};
    let s = TestServer::start(|_| {});
    let started = std::time::Instant::now();
    let mut a = std::net::TcpStream::connect(s.addr).unwrap();
    a.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").unwrap();
    let mut b = std::net::TcpStream::connect(s.addr).unwrap();
    b.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut buf = [0u8; 512];
    assert!(b.read(&mut buf).unwrap() > 0, "first request answered");
    for c in [&mut a, &mut b] {
        c.set_read_timeout(Some(std::time::Duration::from_secs(45))).unwrap();
        let mut rest = Vec::new();
        // Reads until the server closes (EOF) or the 45 s safety timeout (an error).
        let r = c.read_to_end(&mut rest);
        assert!(r.is_ok(), "connection still open: {r:?}");
    }
    let e = started.elapsed();
    assert!(e >= std::time::Duration::from_secs(25) && e < std::time::Duration::from_secs(40), "{e:?}");
}

/// `PUT /v1/objects?for_name=` applies the namespace's full profile (here `derive = "previous"`),
/// the same contract as `LocalRepo::put_named`; `RemoteRepo::put_named` sends it.
#[test]
fn put_named_applies_the_profile_remotely() {
    use o5s_core::{Name, ObjectStore, Observed, Repository};
    let s = TestServer::start(|cfg| {
        let mut rc = o5s_repo::RepoConfig::default();
        rc.profiles.insert(
            "ckpt".into(),
            o5s_repo::Profile { derive: o5s_repo::DerivePolicy::Previous, ..Default::default() },
        );
        rc.namespaces.insert("alice/ckpt".into(), "ckpt".into());
        o5s_repo::LocalRepo::init_config(&cfg.repo.path, &rc).unwrap();
    });
    let c = s.client(ALICE);
    let n = Name::new("alice/ckpt/model").unwrap();
    let v1: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    let h1 = c.put_named(&n, &mut &v1[..]).unwrap();
    keep(&c, "alice", &h1);
    c.replace(&n, &[(n.clone(), Observed::Absent)], h1).unwrap();
    let mut v2 = v1.clone();
    v2[1_000_000] ^= 0xff;
    let h2 = c.put_named(&n, &mut &v2[..]).unwrap();
    keep(&c, "alice", &h2);
    assert_eq!(h2, o5s_core::Hash::of(&v2), "identity is the content hash");
    assert_eq!(c.get(&h2, None).unwrap(), v2);
    // No write access on the name: refused.
    assert!(s.client(READER).put_named(&n, &mut &v2[..]).is_err());
}

/// `[[limits]]`: a namespace rate shared by all callers, and a storage quota measured by the
/// background scan (writes refused over it; reads, deletes and admins are not).
#[test]
fn namespace_rate_and_quota() {
    use o5s_server::config::{LimitRule, RateLimit};
    let srv = TestServer::start(|cfg| {
        cfg.limits.push(LimitRule {
            prefix: "alice/hot".into(),
            rate: Some(RateLimit { per_sec: 0.01, burst: 3.0 }),
            max_stored_bytes: None,
        });
        cfg.limits.push(LimitRule { prefix: "alice/small".into(), rate: None, max_stored_bytes: Some(1000) });
        cfg.server.limits_scan_secs = 1;
    });
    let alice = srv.client(ALICE);
    let admin = srv.client(ADMIN);
    // Rate: 3 requests against alice/hot, then 429 (other namespaces are unaffected).
    let hot = Name::new("alice/hot/x").unwrap();
    for _ in 0..3 {
        assert!(alice.resolve(&hot).unwrap().is_none());
    }
    let http = reqwest::blocking::Client::new();
    let r = http.get(format!("{}/v1/refs/alice/hot/x", srv.url())).bearer_auth(ALICE).send().unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().get("retry-after").is_some());
    assert!(alice.resolve(&Name::new("alice/other").unwrap()).unwrap().is_none());

    // Quota: 1000 bytes under alice/small. A 4000-byte object pushes it over at the next scan.
    let big = alice.put(&vec![7u8; 4000]).unwrap();
    let n1 = Name::new("alice/small/a").unwrap();
    alice.replace(&n1, &[(n1.clone(), Observed::Absent)], big).unwrap();
    let small = alice.put(b"tiny").unwrap();
    let n2 = Name::new("alice/small/b").unwrap();
    let mut refused = false;
    for _ in 0..100 {
        match alice.replace(&n2, &[(n2.clone(), Observed::Absent)], small) {
            Err(e) if e.to_string().contains("quota") => {
                refused = true;
                break;
            }
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => {}
        }
        // Not measured yet: undo and retry after the next scan.
        let v = alice.resolve(&n2).unwrap().unwrap();
        alice.delete_ref(&n2, &[(n2.clone(), Observed::Version(v.version))]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(refused, "quota never applied");
    // Reads and deletes still work; an admin may write (cleanup).
    assert_eq!(alice.resolve(&n1).unwrap().unwrap().hash, big);
    admin.replace(&n2, &[(n2.clone(), Observed::Absent)], small).unwrap();
    let v = alice.resolve(&n1).unwrap().unwrap();
    alice.delete_ref(&n1, &[(n1.clone(), Observed::Version(v.version))]).unwrap();
    // Under the quota again after the next scan.
    let n3 = Name::new("alice/small/c").unwrap();
    let mut ok = false;
    for _ in 0..100 {
        if alice.replace(&n3, &[(n3.clone(), Observed::Absent)], small).is_ok() {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ok, "quota never lifted");
}

/// Object reads by hash are authorized by reachability: a principal reads an object only if
/// a name it may read reaches it. Unauthorized and nonexistent look the same.
#[test]
fn object_reads_follow_reachability() {
    let s = TestServer::start(|cfg| {
        cfg.policy.push(grant("alice", "team-a", Access::Write));
        cfg.policy.push(grant("reader", "team-b", Access::Write));
    });
    let a = s.client(ALICE);
    let b = s.client(READER);
    let private = a.put(b"team a's private data").unwrap();
    let na = Name::new("team-a/secret").unwrap();
    a.replace(&na, &[(na.clone(), Observed::Absent)], private).unwrap();
    let shared = a.put(b"a blob both teams use").unwrap();
    let sa = Name::new("team-a/shared").unwrap();
    a.replace(&sa, &[(sa.clone(), Observed::Absent)], shared).unwrap();
    let sb = Name::new("team-b/shared").unwrap();
    b.replace(&sb, &[(sb.clone(), Observed::Absent)], shared).unwrap();
    let get = |tok: &str, h: &Hash| {
        let r = http().get(format!("{}/v1/objects/{}", s.url(), h.to_hex())).bearer_auth(tok).send().unwrap();
        (r.status(), r.text().unwrap())
    };
    // A cross-team read is denied, exactly like a missing object.
    let denied = get(READER, &private);
    let missing = get(READER, &Hash::of(b"never stored"));
    assert_eq!(denied.0, 404);
    assert_eq!(missing.0, 404);
    assert_eq!(
        denied.1.replace(&private.to_hex(), "H"),
        missing.1.replace(&Hash::of(b"never stored").to_hex(), "H"),
        "unauthorized and nonexistent must be indistinguishable"
    );
    let head = http().head(format!("{}/v1/objects/{}", s.url(), private.to_hex())).bearer_auth(READER).send().unwrap();
    assert_eq!(head.status(), 404);
    assert_eq!(get(ALICE, &private).0, 200);
    // A shared blob reachable from both teams is readable by both.
    assert_eq!(get(ALICE, &shared).0, 200);
    assert_eq!(get(READER, &shared).0, 200);
    // After its last readable name is deleted, an object is unreadable at once.
    let v = b.resolve(&sb).unwrap().unwrap();
    b.delete_ref(&sb, &[(sb.clone(), Observed::Version(v.version))]).unwrap();
    assert_eq!(get(READER, &shared).0, 404);
    assert_eq!(get(ALICE, &shared).0, 200, "still reachable from team-a");
    // Unreferenced objects are readable by no one, admins included.
    let loose = a.put(b"not named yet").unwrap();
    assert_eq!(get(ALICE, &loose).0, 404);
    assert_eq!(get(ADMIN, &loose).0, 404);
    // An admin reads anything named.
    assert_eq!(get(ADMIN, &private).0, 200);
}

/// With `auth.token_key_file`, the token API issues h5i-app tokens: parsed by the extracted,
/// proven `h5i_app_token::parse`, signed with HMAC-SHA256, and still revocable through the table.
#[test]
fn issued_tokens_are_h5i_app_tokens() {
    let s = TestServer::start(|c| {
        let dir = c.repo.path.parent().unwrap().to_path_buf();
        c.auth.token_file = Some(dir.join("tokens.toml"));
        std::fs::write(dir.join("token.key"), [7u8; 32]).unwrap();
        c.auth.token_key_file = Some(dir.join("token.key"));
    });
    let admin = |m: reqwest::Method, p: &str| http().request(m, format!("{}{p}", s.url())).bearer_auth(ADMIN);
    let v: serde_json::Value = admin(reqwest::Method::POST, "/v1/admin/tokens")
        .json(&serde_json::json!({"principal": "robot", "ttl_secs": 3600}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let tok = v["token"].as_str().unwrap().to_string();
    let sha = v["sha256"].as_str().unwrap().to_string();
    assert!(tok.starts_with("v1.0."), "{tok}");
    let who = |t: &str| http().get(format!("{}/v1/whoami", s.url())).bearer_auth(t).send().unwrap();
    let r = who(&tok);
    assert_eq!(r.status(), 200);
    assert!(r.text().unwrap().contains("robot"));
    // A tampered signature, user or expiry is refused.
    let mut bad = tok.clone().into_bytes();
    let last = bad.len() - 1;
    bad[last] = if bad[last] == b'0' { b'1' } else { b'0' };
    assert_eq!(who(std::str::from_utf8(&bad).unwrap()).status(), 401);
    let parts: Vec<&str> = tok.split('.').collect();
    let forged = format!("v1.0.{}.{}.{}", parts[2].parse::<u64>().unwrap() + 1, parts[3], parts[4]);
    assert_eq!(who(&forged).status(), 401);
    // Static tokens keep working; revocation still works for issued ones.
    assert_eq!(who(ALICE).status(), 200);
    assert_eq!(admin(reqwest::Method::DELETE, &format!("/v1/admin/tokens/{sha}")).send().unwrap().status(), 204);
    assert_eq!(who(&tok).status(), 401);
}
