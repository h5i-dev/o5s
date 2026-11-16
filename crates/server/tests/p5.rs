//! Cross-front provenance, search, attestations, promotion and the audit query (ROADMAP P5).

mod common;

use common::*;
use o5s_core::{Name, ObjectStore, Observed, Repository};
use o5s_server::config::{Access, PromotionRule};
use serde_json::{Value, json};
use std::process::Command;

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn provenance_attestation_promotion_and_audit_across_fronts() {
    if !Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: git not installed");
        return;
    }
    let signer = o5s_attest::Ed25519Signer::from_seed(&[7u8; 32]);
    let pk = hexs(&signer.verifier().public());
    let srv = TestServer::start(move |cfg| {
        cfg.policy.push(grant("alice", "git/team", Access::Write));
        cfg.policy.push(grant("alice", "dev", Access::Write));
        cfg.policy.push(grant("alice", "prod", Access::Write));
        cfg.policy.push(grant("alice", "staging", Access::Write));
        cfg.policy.push(grant("reader", "git/other", Access::Read));
        cfg.promotion.push(PromotionRule {
            dst: "prod".into(),
            required_keys: vec![pk.clone()],
            predicate_type: Some("https://slsa.dev/provenance/v1".into()),
            require_provenance: true,
        });
    });
    let http = reqwest::blocking::Client::new();
    let get = |path: &str| -> Value {
        http.get(format!("{}{path}", srv.url())).bearer_auth(ALICE).send().unwrap().json().unwrap()
    };

    // A git push records `git.publish` provenance for the pushed commit.
    let w = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        let o = Command::new("git")
            .current_dir(w.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    g(&["init", "-q", "-b", "main"]);
    std::fs::write(w.path().join("setup.py"), "print('hi')\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "release 1.0"]);
    let oid = g(&["rev-parse", "HEAD"]);
    g(&["push", "-q", &format!("http://u:{ALICE}@{}/git/team/app.git", srv.addr), "main"]);
    let git_ref = format!("git:team/app@{oid}");
    // Provenance records are written asynchronously after the publish: poll briefly.
    let mut prov = Value::Null;
    for _ in 0..50 {
        prov = get(&format!("/v1/provenance?ref={git_ref}"));
        if prov["producers"].as_array().is_some_and(|a| !a.is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let p0 = &prov["producers"][0];
    assert_eq!(p0["op"], "git.publish", "{prov}");
    assert_eq!(p0["meta"]["ref"], "refs/heads/main");
    assert_eq!(p0["meta"]["oid"], oid.as_str());
    let commit_hash = prov["object"].as_str().unwrap().to_string();
    // Git oids are global, but resolving one needs a readable name reaching it: a reader of
    // another repository can't resolve team/app's commit through its own repo's prefix.
    let r = http.get(format!("{}/v1/provenance?ref=git:other/x@{oid}", srv.url())).bearer_auth(READER).send().unwrap();
    assert_eq!(r.status(), 404, "cross-repo git id must not resolve");
    let r =
        http.get(format!("{}/v1/provenance?ref=blake3:{commit_hash}", srv.url())).bearer_auth(READER).send().unwrap();
    assert_eq!(r.status(), 404, "an unreadable object by hash must not resolve");

    // A "wheel" built from that commit (a plain object under dev/), with declared lineage.
    let client = srv.client(ALICE);
    let wheel = client.put(b"pretend wheel bytes").unwrap();
    let n = Name::new("dev/app/app-1.0-py3-none-any.whl").unwrap();
    client.replace(&n, &[(n.clone(), Observed::Absent)], wheel).unwrap();
    let r = http
        .post(format!("{}/v1/provenance", srv.url()))
        .bearer_auth(ALICE)
        .json(&json!({ "op": "build", "inputs": { "source": git_ref }, "outputs": { "wheel": format!("name:{n}") },
                       "meta": { "builder": "ci-runner-7" } }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 201, "{}", r.text().unwrap_or_default());
    let wp = get(&format!("/v1/provenance?ref=name:{n}"));
    assert!(wp["lineage"].as_array().unwrap().iter().any(|h| h == commit_hash.as_str()), "{wp}");
    let ip = get(&format!("/v1/provenance?ref={git_ref}"));
    assert!(ip["impact"].as_array().unwrap().iter().any(|h| *h == json!(wheel.to_hex())), "{ip}");
    // Outputs need write access: a reader can't claim lineage for alice's wheel.
    let r = http
        .post(format!("{}/v1/provenance", srv.url()))
        .bearer_auth(READER)
        .json(&json!({ "op": "x", "outputs": { "o": format!("name:{n}") } }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 403);

    // Search finds the build by its metadata, and names the wheel.
    let s = get("/v1/search?q=ci-runner-7");
    assert!(
        s["results"].as_array().unwrap().iter().any(|r| r["names"].as_array().unwrap().iter().any(|x| x == n.as_str())),
        "{s}"
    );

    // Promotion: refused without a trusted signature, allowed after one.
    let promote = |dst: &str| {
        http.post(format!("{}/v1/promote", srv.url()))
            .bearer_auth(ALICE)
            .json(&json!({ "src": n.as_str(), "dst": dst }))
            .send()
            .unwrap()
    };
    let r = promote("prod/app/app-1.0-py3-none-any.whl");
    assert_eq!(r.status(), 403);
    assert!(r.text().unwrap().contains("missing a valid signature"));
    let stmt = o5s_attest::Statement {
        subjects: vec![o5s_attest::Subject { name: n.to_string(), hash: wheel }],
        predicate_type: "https://slsa.dev/provenance/v1".into(),
        predicate: json!({ "builder": { "id": "ci-runner-7" } }),
    };
    let env = o5s_attest::Envelope::sign(&stmt, &[&signer]);
    let r = http.post(format!("{}/v1/attestations", srv.url())).bearer_auth(ALICE).body(env.to_json()).send().unwrap();
    assert_eq!(r.status(), 201);
    let a: Value = r.json().unwrap();
    assert_eq!(a["trusted_keys"].as_array().unwrap().len(), 1, "{a}");
    let at = get(&format!("/v1/attestations?ref=name:{n}"));
    assert_eq!(at["attestations"].as_array().unwrap().len(), 1);
    let r = promote("prod/app/app-1.0-py3-none-any.whl");
    assert_eq!(r.status(), 200, "{}", r.text().unwrap_or_default());
    let pn = Name::new("prod/app/app-1.0-py3-none-any.whl").unwrap();
    assert_eq!(client.resolve(&pn).unwrap().unwrap().hash, wheel);
    assert_eq!(client.get(&wheel, None).unwrap(), b"pretend wheel bytes");
    // No rule covers staging/: refused.
    assert_eq!(promote("staging/app/x.whl").status(), 403);

    // One audit query across fronts.
    let au: Value = http
        .get(format!("{}/v1/admin/audit?action=promote", srv.url()))
        .bearer_auth(ADMIN)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let outcomes: Vec<&str> =
        au["records"].as_array().unwrap().iter().map(|r| r["outcome"].as_str().unwrap()).collect();
    assert!(outcomes.contains(&"ok") && outcomes.contains(&"refused"), "{au}");
    let au: Value = http
        .get(format!("{}/v1/admin/audit?action=git_push", srv.url()))
        .bearer_auth(ADMIN)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(au["records"].as_array().unwrap().len(), 1);
    assert_eq!(http.get(format!("{}/v1/admin/audit", srv.url())).bearer_auth(ALICE).send().unwrap().status(), 403);
}

/// The client's P5 helpers (used by the CLI's `--remote` attest/promote/lineage/search).
#[test]
fn remote_repo_attest_promote_lineage_search() {
    use o5s_attest::PromoteOutcome;
    use o5s_client::Search;
    let signer = o5s_attest::Ed25519Signer::from_seed(&[9u8; 32]);
    let pk = hexs(&signer.verifier().public());
    let srv = TestServer::start(move |cfg| {
        for ns in ["dev", "prod", "staging"] {
            cfg.policy.push(grant("alice", ns, Access::Write));
        }
        cfg.promotion.push(PromotionRule {
            dst: "prod".into(),
            required_keys: vec![pk.clone()],
            predicate_type: None,
            require_provenance: false,
        });
    });
    let client = srv.client(ALICE);
    let src_obj = client.put(b"source tarball").unwrap();
    let s = Name::new("dev/lib/lib-2.0.tar.gz").unwrap();
    client.replace(&s, &[(s.clone(), Observed::Absent)], src_obj).unwrap();
    let wheel = client.put(b"another wheel").unwrap();
    let n = Name::new("dev/lib/lib-2.0-py3-none-any.whl").unwrap();
    client.replace(&n, &[(n.clone(), Observed::Absent)], wheel).unwrap();
    let r = reqwest::blocking::Client::new()
        .post(format!("{}/v1/provenance", srv.url()))
        .bearer_auth(ALICE)
        .json(&json!({ "op": "build", "inputs": { "sdist": format!("name:{s}") },
                       "outputs": { "wheel": format!("name:{n}") }, "meta": { "builder": "runner-42" } }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 201);
    assert!(client.lineage(&format!("name:{n}")).unwrap().contains(&src_obj));
    assert!(client.impact(&format!("blake3:{}", src_obj.to_hex())).unwrap().contains(&wheel));
    assert_eq!(client.provenance(&format!("name:{n}")).unwrap().producers[0]["op"], "build");

    let pn = Name::new("prod/lib/lib-2.0-py3-none-any.whl").unwrap();
    match client.promote(&n, &pn).unwrap() {
        PromoteOutcome::Refused(r) => assert!(r.iter().any(|x| x.contains("signature")), "{r:?}"),
        o => panic!("expected a refusal, got {o:?}"),
    }
    let stmt = o5s_attest::Statement {
        subjects: vec![o5s_attest::Subject { name: n.to_string(), hash: wheel }],
        predicate_type: "https://example.com/reviewed".into(),
        predicate: json!({}),
    };
    let env = o5s_attest::Envelope::sign(&stmt, &[&signer]);
    let (eh, trusted) = client.attest(&env).unwrap();
    assert_eq!(trusted.len(), 1);
    let at = client.attestations(&format!("name:{n}")).unwrap();
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].0, eh);
    assert_eq!(at[0].1.to_json(), env.to_json());
    assert_eq!(at[0].2, trusted);
    match client.promote(&n, &pn).unwrap() {
        PromoteOutcome::Promoted { object, .. } => assert_eq!(object, wheel),
        o => panic!("expected promotion, got {o:?}"),
    }
    assert_eq!(client.resolve(&pn).unwrap().unwrap().hash, wheel);
    // No rule covers staging/: an error, not a policy refusal.
    assert!(client.promote(&n, &Name::new("staging/lib/x.whl").unwrap()).is_err());
    // A reader without access to dev/ can't promote from it.
    assert!(srv.client(READER).promote(&n, &Name::new("prod/lib/y.whl").unwrap()).is_err());

    let hits = client.search("runner-42").unwrap();
    assert!(hits.iter().any(|h| h.object == wheel && h.names.iter().any(|x| x == n.as_str())), "{hits:?}");
    let hits = client.search(Search::Exact(pn.to_string())).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].object, wheel);
    let hits = client.search(Search::Prefix("dev/lib/".into())).unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
}
