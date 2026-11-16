//! OIDC/JWT authentication against a mock issuer (needs the `openssl` CLI).

mod common;
use common::*;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

fn b64url(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// RSA key: (PEM private key, JWK n).
fn rsa_key(dir: &std::path::Path, name: &str) -> Option<(Vec<u8>, String)> {
    let k = dir.join(format!("{name}.pem"));
    Command::new("openssl")
        .args(["genrsa", "-out"])
        .arg(&k)
        .arg("2048")
        .output()
        .ok()?
        .status
        .success()
        .then_some(())?;
    let m = Command::new("openssl").args(["rsa", "-noout", "-modulus", "-in"]).arg(&k).output().ok()?;
    let hexs = String::from_utf8_lossy(&m.stdout).trim().strip_prefix("Modulus=")?.to_string();
    let n: Vec<u8> = (0..hexs.len()).step_by(2).map(|i| u8::from_str_radix(&hexs[i..i + 2], 16).unwrap()).collect();
    Some((std::fs::read(&k).ok()?, b64url(&n)))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn sign(pem: &[u8], kid: &str, claims: serde_json::Value) -> String {
    let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = Some(kid.into());
    jsonwebtoken::encode(&h, &claims, &jsonwebtoken::EncodingKey::from_rsa_pem(pem).unwrap()).unwrap()
}

#[test]
fn oidc_tokens() {
    let d = tempfile::tempdir().unwrap();
    let (Some((pem1, n1)), Some((pem2, n2))) = (rsa_key(d.path(), "k1"), rsa_key(d.path(), "k2")) else {
        eprintln!("skipping: openssl not available");
        return;
    };
    // Mock issuer: discovery + JWKS (which we can rotate).
    let jwks = Arc::new(Mutex::new(
        serde_json::json!({"keys": [{"kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig", "n": n1, "e": "AQAB"}]}),
    ));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let issuer = rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let iss = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let iss2 = iss.clone();
        let j = jwks.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(move || {
                    let iss = iss2.clone();
                    async move { axum::Json(serde_json::json!({"issuer": iss, "jwks_uri": format!("{iss}/jwks")})) }
                }),
            )
            .route(
                "/jwks",
                axum::routing::get(move || {
                    let j = j.clone();
                    async move { axum::Json(j.lock().unwrap().clone()) }
                }),
            );
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        iss
    });
    let iss = issuer.clone();
    let s = TestServer::start(move |c| {
        c.auth.oidc = Some(o5s_server::config::OidcSection {
            issuer: iss.clone(),
            audience: "o5s".into(),
            jwks_url: None,
            principal_claim: "email".into(),
            principal_prefix: "oidc:".into(),
            min_refetch_secs: 0,
        });
        c.policy.push(grant("oidc:dev@example.com", "dev", Access::Write));
    });
    let claims = |aud: &str, exp: u64| serde_json::json!({"iss": issuer, "aud": aud, "exp": exp, "sub": "123", "email": "dev@example.com"});
    let who = |tok: &str| http().get(format!("{}/v1/whoami", s.url())).bearer_auth(tok).send().unwrap();

    let good = sign(&pem1, "k1", claims("o5s", now() + 600));
    let r = who(&good);
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<o5s_client::wire::WhoAmI>().unwrap().principal, "oidc:dev@example.com");
    // Authorization works with the mapped principal.
    let c = RemoteRepo::new(&s.url(), Some(good.clone()), ClientConfig::default()).unwrap();
    let h = c.put(b"from oidc").unwrap();
    c.replace(&n("dev/x"), &[(n("dev/x"), Observed::Absent)], h).unwrap();
    assert!(c.resolve(&n("alice/x")).is_err());

    assert_eq!(who(&sign(&pem1, "k1", claims("other-aud", now() + 600))).status(), 401, "wrong audience");
    assert_eq!(who(&sign(&pem1, "k1", claims("o5s", now() - 600))).status(), 401, "expired");
    assert_eq!(who(&sign(&pem2, "k1", claims("o5s", now() + 600))).status(), 401, "bad signature");
    let mut t = good.clone();
    t.push('x');
    assert_eq!(who(&t).status(), 401, "tampered");
    // Rotation: a token with a new kid is accepted once the JWKS publishes it.
    let rotated = sign(&pem2, "k2", claims("o5s", now() + 600));
    jwks.lock().unwrap()["keys"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"kty": "RSA", "kid": "k2", "alg": "RS256", "n": n2, "e": "AQAB"}));
    assert_eq!(who(&rotated).status(), 200, "unknown kid triggers a JWKS refetch");
    // A kid the issuer never published stays rejected.
    assert_eq!(who(&sign(&pem2, "nope", claims("o5s", now() + 600))).status(), 401);
    // Static tokens keep working alongside OIDC.
    assert_eq!(who(ALICE).status(), 200);
}
