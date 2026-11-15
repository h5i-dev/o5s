//! OCI distribution adapter tests beyond the official conformance suite
//! (which CI runs via scripts/oci-conformance.sh).

mod common;
use common::*;
use sha2::Digest as _;

fn sha(b: &[u8]) -> String {
    format!("sha256:{}", hex(&sha2::Sha256::digest(b)))
}

fn basic(tok: &str) -> String {
    use base64::Engine as _;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("user:{tok}")))
}

struct Oci<'a> {
    s: &'a TestServer,
    auth: String,
}

impl Oci<'_> {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.s.url())
    }
    fn req(&self, m: reqwest::Method, p: &str) -> reqwest::blocking::RequestBuilder {
        http().request(m, self.url(p)).header("authorization", &self.auth)
    }
    /// Monolithic upload; returns the status.
    fn push_blob(&self, repo: &str, data: &[u8]) -> u16 {
        self.req(reqwest::Method::POST, &format!("/v2/{repo}/blobs/uploads/?digest={}", sha(data)))
            .body(data.to_vec())
            .send()
            .unwrap()
            .status()
            .as_u16()
    }
    fn push_manifest(&self, repo: &str, reference: &str, body: &str) -> reqwest::blocking::Response {
        self.req(reqwest::Method::PUT, &format!("/v2/{repo}/manifests/{reference}"))
            .header("content-type", "application/vnd.oci.image.manifest.v1+json")
            .body(body.to_string())
            .send()
            .unwrap()
    }
}

fn manifest(config: &[u8], layers: &[&[u8]]) -> String {
    let layers: Vec<String> = layers
        .iter()
        .map(|l| {
            format!(
                r#"{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{}","size":{}}}"#,
                sha(l),
                l.len()
            )
        })
        .collect();
    format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[{}]}}"#,
        sha(config),
        config.len(),
        layers.join(",")
    )
}

fn oci_server() -> TestServer {
    TestServer::start(|c| {
        c.policy.push(grant("alice", "oci/team", Access::Write));
        c.policy.push(grant("reader", "oci/team", Access::Read));
    })
}

#[test]
fn auth_challenge_and_basic_auth() {
    let s = oci_server();
    let r = http().get(format!("{}/v2/", s.url())).send().unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.headers()["www-authenticate"], "Basic realm=\"pico2peta\"");
    let r = http().get(format!("{}/v2/", s.url())).header("authorization", basic(ALICE)).send().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["docker-distribution-api-version"], "registry/2.0");
}

#[test]
fn push_pull_roundtrip_with_chunked_upload() {
    let s = oci_server();
    let o = Oci { s: &s, auth: basic(ALICE) };
    let layer = data(3 << 20, 1);
    // Chunked upload: POST, 2x PATCH, PUT.
    let r = o.req(reqwest::Method::POST, "/v2/team/app/blobs/uploads/").send().unwrap();
    assert_eq!(r.status(), 202);
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    let (a, b) = layer.split_at(1 << 20);
    let r = o
        .req(reqwest::Method::PATCH, &loc)
        .header("content-range", format!("0-{}", a.len() - 1))
        .body(a.to_vec())
        .send()
        .unwrap();
    assert_eq!(r.status(), 202);
    assert_eq!(r.headers()["range"], format!("0-{}", a.len() - 1).as_str());
    // Out-of-order chunk is rejected with the current range.
    let r = o.req(reqwest::Method::PATCH, &loc).header("content-range", "5-9").body("xxxxx").send().unwrap();
    assert_eq!(r.status(), 416);
    let r = o.req(reqwest::Method::PATCH, &loc).body(b.to_vec()).send().unwrap();
    assert_eq!(r.status(), 202);
    let r = o.req(reqwest::Method::GET, &loc).send().unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(r.headers()["range"], format!("0-{}", layer.len() - 1).as_str());
    let r = o.req(reqwest::Method::PUT, &format!("{loc}?digest={}", sha(&layer))).send().unwrap();
    assert_eq!(r.status(), 201, "{:?}", r.text());
    // The finished session is gone.
    assert_eq!(o.req(reqwest::Method::GET, &loc).send().unwrap().status(), 404);

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    assert_eq!(o.push_blob("team/app", config), 201);
    let m = manifest(config, &[&layer]);
    let r = o.push_manifest("team/app", "v1", &m);
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["docker-content-digest"], sha(m.as_bytes()).as_str());

    // Pull as a read-only principal.
    let ro = Oci { s: &s, auth: format!("Bearer {READER}") };
    let r = ro.req(reqwest::Method::GET, "/v2/team/app/manifests/v1").send().unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/vnd.oci.image.manifest.v1+json");
    assert_eq!(r.text().unwrap(), m);
    let r = ro.req(reqwest::Method::GET, &format!("/v2/team/app/blobs/{}", sha(&layer))).send().unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.bytes().unwrap() == layer);
    let r = ro
        .req(reqwest::Method::GET, &format!("/v2/team/app/blobs/{}", sha(&layer)))
        .header("range", "bytes=10-19")
        .send()
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(&r.bytes().unwrap()[..], &layer[10..20]);
    // Readers cannot push.
    assert_eq!(ro.push_blob("team/app", b"nope"), 403);
    // Tags list.
    let r = ro.req(reqwest::Method::GET, "/v2/team/app/tags/list").send().unwrap();
    assert_eq!(r.json::<serde_json::Value>().unwrap()["tags"], serde_json::json!(["v1"]));
}

#[test]
fn integrity_and_isolation() {
    let s = oci_server();
    let o = Oci { s: &s, auth: basic(ALICE) };
    // Wrong digest is rejected.
    let r = o
        .req(reqwest::Method::POST, &format!("/v2/team/a/blobs/uploads/?digest={}", sha(b"other")))
        .body("data")
        .send()
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().unwrap().contains("DIGEST_INVALID"));
    // Manifests must reference blobs linked into *this* repo.
    let config = b"{}";
    assert_eq!(o.push_blob("team/a", config), 201);
    let m = manifest(config, &[]);
    assert_eq!(o.push_manifest("team/a", "x", &m).status(), 201);
    let r = o.push_manifest("team/b", "x", &m);
    assert_eq!(r.status(), 400);
    assert!(r.text().unwrap().contains("MANIFEST_BLOB_UNKNOWN"));
    // A blob in team/a is not visible from team/b until mounted.
    let path = format!("/v2/team/b/blobs/{}", sha(config));
    assert_eq!(o.req(reqwest::Method::HEAD, &path).send().unwrap().status(), 404);
    let r = o
        .req(reqwest::Method::POST, &format!("/v2/team/b/blobs/uploads/?mount={}&from=team/a", sha(config)))
        .send()
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(o.req(reqwest::Method::HEAD, &path).send().unwrap().status(), 200);
    assert_eq!(o.push_manifest("team/b", "x", &m).status(), 201);
    // Outside the granted namespace.
    assert_eq!(o.push_blob("other/repo", b"x"), 403);
    // Tag deletion works; blob deletion is left to GC.
    assert_eq!(o.req(reqwest::Method::DELETE, "/v2/team/a/manifests/x").send().unwrap().status(), 202);
    assert_eq!(o.req(reqwest::Method::GET, "/v2/team/a/manifests/x").send().unwrap().status(), 404);
    assert_eq!(o.req(reqwest::Method::DELETE, "/v2/team/a/manifests/x").send().unwrap().status(), 404);
    assert_eq!(
        o.req(reqwest::Method::DELETE, &format!("/v2/team/a/blobs/{}", sha(config))).send().unwrap().status(),
        405
    );
    // Deleting by digest removes the manifest and every tag on it.
    assert_eq!(o.push_manifest("team/b", "y", &m).status(), 201);
    let r = o.req(reqwest::Method::DELETE, &format!("/v2/team/b/manifests/{}", sha(m.as_bytes()))).send().unwrap();
    assert_eq!(r.status(), 202);
    assert_eq!(o.req(reqwest::Method::GET, "/v2/team/b/manifests/x").send().unwrap().status(), 404);
    assert_eq!(o.req(reqwest::Method::GET, "/v2/team/b/manifests/y").send().unwrap().status(), 404);
    // Adapter namespaces cannot be rewritten through /v1 by non-admins.
    let c = s.client(ALICE);
    let h = c.put(b"evil").unwrap();
    let r = c.replace(&n("oci/team/a/tags/x"), &[(n("oci/team/a/tags/x"), Observed::Version(1))], h);
    assert!(matches!(r, Err(Error::Unauthorized(_))), "{r:?}");
}

#[test]
fn gc_tracer_follows_manifests_through_the_digest_index() {
    let s = oci_server();
    let o = Oci { s: &s, auth: basic(ALICE) };
    let config = br#"{"c":1}"#;
    let layer = data(10_000, 2);
    assert_eq!(o.push_blob("team/g", config), 201);
    assert_eq!(o.push_blob("team/g", &layer), 201);
    let m = manifest(config, &[&layer]);
    assert_eq!(o.push_manifest("team/g", "t", &m).status(), 201);
    let repo = pico2peta_repo::LocalRepo::open(s.dir.path().join("repo")).unwrap();
    let tracer = pico2peta_server::oci::OciTracer::new(std::sync::Arc::new(repo.clone()));
    let mh = repo.resolve(&n("oci/team/g/tags/t")).unwrap().unwrap().hash;
    let mut kids = tracer.children_of(&mh, &repo).unwrap().unwrap();
    kids.sort();
    let mut want = vec![Hash::of(config), Hash::of(&layer)];
    want.sort();
    assert_eq!(kids, want);
    // A layer is a leaf; the digest-index root keeps its segments alive.
    assert_eq!(tracer.children_of(&Hash::of(&layer), &repo).unwrap(), None);
    let root = repo.resolve(&n("oci/_digest")).unwrap().unwrap().hash;
    assert!(!tracer.children_of(&root, &repo).unwrap().unwrap().is_empty());
}
