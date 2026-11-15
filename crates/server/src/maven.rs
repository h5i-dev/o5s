//! Maven-compatible repositories: `mvn deploy` and dependency resolution work against them
//! unchanged (the plain HTTP repository layout: GET, HEAD, PUT).
//!
//! - `/maven/<repo>/<group path>/<artifact>/<version>/<file>`.
//! - Checksums (`.md5`, `.sha1`, `.sha256`, `.sha512` of any file) are computed from the
//!   stored file, not stored separately. A checksum PUT is verified against the file.
//! - Release files are create-once (an identical re-deploy is fine; other bytes get 409).
//!   `maven-metadata.xml` and everything under a `-SNAPSHOT` version are mutable, since
//!   Maven rewrites them on every deploy.
//! - Upstream proxy-cache (`[maven.upstream.<repo>]`, e.g. Maven Central): a miss is fetched
//!   from the upstream, checked against the upstream's `.sha1`, stored, and served from
//!   then on, including while the upstream is down. Mutable files from the upstream
//!   (metadata, snapshots) are refreshed after the upstream's TTL, and the cached copy is
//!   served if that fails.
//!
//! **Storage.** `maven/<repo>/<path>` → a `P2PMVN01` record (content hash, digests). The
//! `maven` front claims `maven/`, and GC follows record → content. Authorization is on
//! `maven/<repo>`.

// Handlers short-circuit with a ready `Response` as the error value.
#![allow(clippy::result_large_err)]

use crate::api::{AppState, Caller};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::{Access, Upstream};
use crate::pkg::{Store, Stored, blocking, core_err, err};
use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Observed, ReplaceOutcome, Repository, Result};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const RECORD_MAGIC: &[u8; 8] = b"P2PMVN01";
const CHECKSUMS: [&str; 4] = ["md5", "sha1", "sha256", "sha512"];

pub struct MavenState {
    repo: Arc<dyn Repository>,
    store: Store,
    upstreams: BTreeMap<String, Upstream>,
    http: reqwest::Client,
}

impl MavenState {
    pub fn new(
        repo: Arc<dyn Repository>,
        local: Option<pico2peta_repo::LocalRepo>,
        upstreams: BTreeMap<String, Upstream>,
    ) -> std::result::Result<MavenState, String> {
        Ok(MavenState { store: Store { repo: repo.clone(), local }, repo, upstreams, http: crate::pkg::http_client()? })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Meta {
    md5: String,
    sha1: String,
    sha256: String,
    sha512: String,
    size: u64,
    /// `deploy` or `upstream`.
    source: String,
    /// Unix seconds when stored (upstream refresh age).
    stored: u64,
}

impl Meta {
    fn of(x: &Stored, source: &str) -> Meta {
        Meta {
            md5: crate::git::hex(&x.md5),
            sha1: x.sha1_hex(),
            sha256: x.sha256_hex(),
            sha512: crate::git::hex(&x.sha512),
            size: x.size,
            source: source.into(),
            stored: now_secs(),
        }
    }
    fn digest(&self, alg: &str) -> &str {
        match alg {
            "md5" => &self.md5,
            "sha1" => &self.sha1,
            "sha256" => &self.sha256,
            _ => &self.sha512,
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn encode_record(content: &Hash, m: &Meta) -> Vec<u8> {
    let mut v = RECORD_MAGIC.to_vec();
    v.extend_from_slice(content.as_bytes());
    v.extend_from_slice(&serde_json::to_vec(m).unwrap_or_default());
    v
}

fn decode_record(b: &[u8]) -> Option<(Hash, Meta)> {
    if !b.starts_with(RECORD_MAGIC) || b.len() < 40 {
        return None;
    }
    Some((Hash(b[8..40].try_into().ok()?), serde_json::from_slice(&b[40..]).ok()?))
}

/// GC: the content a Maven record references.
pub fn record_child(h: &Hash, store: &dyn ObjectStore) -> Result<Option<Hash>> {
    let size = store.size(h)?;
    if !(40..=1 << 16).contains(&size) {
        return Ok(None);
    }
    let head = store.get(h, Some(0..40))?;
    if !head.starts_with(RECORD_MAGIC) {
        return Ok(None);
    }
    Ok(Some(Hash(head[8..40].try_into().map_err(|_| Error::Corrupt("maven record".into()))?)))
}

/// Maven rewrites these on every deploy.
fn is_mutable(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.starts_with("maven-metadata") || path.split('/').any(|s| s.ends_with("-SNAPSHOT"))
}

/// `(base path, algorithm)` if `path` names a checksum of another file.
fn checksum_of(path: &str) -> Option<(&str, &'static str)> {
    CHECKSUMS.iter().find_map(|a| path.strip_suffix(&format!(".{a}")).map(|b| (b, *a)))
}

/// Paths are stored as core names, so they must fit the name grammar (they do for Maven
/// coordinates in practice).
fn record_name(repo: &str, path: &str) -> Option<Name> {
    if path.split('/').count() < 2 || path.len() > 900 {
        return None;
    }
    Name::new(format!("maven/{repo}/{path}")).ok()
}

fn check(st: &AppState, p: &Principal, c: &Caller, repo: &str, need: Access) -> std::result::Result<(), Response> {
    let ns = format!("maven/{repo}");
    if st.authz.allows(p, &ns, need) {
        return Ok(());
    }
    st.metrics.denied.inc();
    st.audit.record_nowait(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "maven",
        target: ns.clone(),
        outcome: "denied".into(),
        detail: None,
        remote: c.remote.clone(),
    });
    let mut r = if p.name == crate::auth::ANONYMOUS {
        let mut r = err(StatusCode::UNAUTHORIZED, "authentication required");
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"pico2peta\""));
        r
    } else {
        err(
            StatusCode::FORBIDDEN,
            format!("{} may not {} {ns}", p.name, if need == Access::Read { "read" } else { "write" }),
        )
    };
    r.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
    Err(r)
}

/// `/maven/{*rest}`.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    method: Method,
    Path(rest): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(mvn) = st.maven.clone() else { return err(StatusCode::NOT_FOUND, "Maven front disabled") };
    let Some((repo, path)) = rest.split_once('/') else { return err(StatusCode::NOT_FOUND, "no path") };
    if repo.is_empty() || !repo.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) {
        return err(StatusCode::NOT_FOUND, "invalid repository name");
    }
    match method {
        Method::GET | Method::HEAD => {
            if let Err(r) = check(&st, &p, &c, repo, Access::Read) {
                return r;
            }
            get(&st, &mvn, repo, path, &headers, method == Method::HEAD).await
        }
        Method::PUT => {
            if let Err(r) = check(&st, &p, &c, repo, Access::Write) {
                return r;
            }
            put(&st, &mvn, &p, &c, repo, path, body).await
        }
        _ => err(StatusCode::METHOD_NOT_ALLOWED, "GET, HEAD or PUT"),
    }
}

async fn load(mvn: &Arc<MavenState>, n: &Name) -> std::result::Result<Option<(Hash, Meta)>, Response> {
    let (m2, n2) = (mvn.clone(), n.clone());
    blocking(move || {
        Ok(match m2.repo.resolve(&n2)? {
            Some(v) => decode_record(&m2.repo.get(&v.hash, None)?),
            None => None,
        })
    })
    .await
}

/// The file at `path`, from the store or (on a miss, or a stale mutable upstream file) from
/// the upstream.
async fn resolve_file(
    st: &Arc<AppState>,
    mvn: &Arc<MavenState>,
    repo: &str,
    path: &str,
) -> std::result::Result<Option<(Hash, Meta)>, Response> {
    let Some(n) = record_name(repo, path) else { return Ok(None) };
    let cached = load(mvn, &n).await?;
    let Some(up) = mvn.upstreams.get(repo) else { return Ok(cached) };
    let stale = match &cached {
        None => true,
        Some((_, m)) => m.source == "upstream" && is_mutable(path) && now_secs().saturating_sub(m.stored) > up.ttl_secs,
    };
    if !stale {
        return Ok(cached);
    }
    match fetch_upstream(st, mvn, up, &n, path).await {
        Ok(Some(x)) => Ok(Some(x)),
        Ok(None) => Ok(cached),
        Err(e) => {
            tracing::warn!(repo, path, error = %e, "upstream fetch failed");
            match cached {
                Some(c) => Ok(Some(c)), // stale but available
                None => Err(err(StatusCode::BAD_GATEWAY, e)),
            }
        }
    }
}

async fn fetch_upstream(
    st: &Arc<AppState>,
    mvn: &Arc<MavenState>,
    up: &Upstream,
    n: &Name,
    path: &str,
) -> std::result::Result<Option<(Hash, Meta)>, String> {
    let url = format!("{}/{path}", up.url.trim_end_matches('/'));
    let get = |u: String| {
        let mut rq = mvn.http.get(u).timeout(Duration::from_secs(300));
        if let Some(t) = &up.token {
            rq = rq.bearer_auth(t);
        }
        rq
    };
    let resp = get(url.clone()).send().await.map_err(|e| format!("upstream: {e}"))?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("upstream {url}: {}", resp.status()));
    }
    st.metrics.upstream_fetches.get_or_create(&crate::metrics::FrontLabels { front: "maven".into() }).inc();
    let ns = Name::new(format!("maven/{}", n.as_str().split('/').nth(1).unwrap_or("_"))).map_err(|e| e.to_string())?;
    let x = mvn
        .store
        .put_stream(ns, Box::pin(resp.bytes_stream()), st.max_upload_bytes)
        .await
        .map_err(|r| format!("storing upstream file failed ({})", r.status()))?;
    // Verify against the upstream's published sha1 when there is one.
    if let Ok(r) = get(format!("{url}.sha1")).send().await
        && r.status().is_success()
        && let Ok(t) = r.text().await
        && let Some(want) = t.split_whitespace().next()
        && !want.eq_ignore_ascii_case(&x.sha1_hex())
    {
        return Err(format!("upstream {url}: sha1 {} does not match its .sha1 {want}", x.sha1_hex()));
    }
    let meta = Meta::of(&x, "upstream");
    let (m2, n2, rec, mutable) = (mvn.clone(), n.clone(), encode_record(&x.hash, &meta), is_mutable(path));
    let sha = meta.sha1.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<()> {
        if mutable {
            replace_any(&*m2.repo, &n2, &rec)
        } else {
            crate::pkg::create_once(&*m2.repo, &n2, &rec, &|b| decode_record(b).is_some_and(|(_, m)| m.sha1 == sha))
                .map(|_| ())
        }
    })
    .await;
    match r {
        Ok(Ok(())) => Ok(Some((x.hash, meta))),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Points `name` at `record` whatever it pointed at (mutable files).
fn replace_any(repo: &dyn Repository, n: &Name, record: &[u8]) -> Result<()> {
    let h = repo.put(record)?;
    for _ in 0..16 {
        let obs = repo.resolve(n)?.map_or(Observed::Absent, |v| Observed::Version(v.version));
        if let ReplaceOutcome::Ok { .. } = repo.replace(n, &[(n.clone(), obs)], h)? {
            return Ok(());
        }
    }
    Err(Error::Corrupt(format!("{n}: update did not settle")))
}

async fn get(
    st: &Arc<AppState>,
    mvn: &Arc<MavenState>,
    repo: &str,
    path: &str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    if let Some((base, alg)) = checksum_of(path) {
        return match resolve_file(st, mvn, repo, base).await {
            Ok(Some((_, m))) => {
                let d = m.digest(alg).to_string();
                let len = d.len();
                let mut r = if head { Response::new(Body::empty()) } else { Response::new(Body::from(d)) };
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                r
            }
            Ok(None) => err(StatusCode::NOT_FOUND, "not found"),
            Err(r) => r,
        };
    }
    match resolve_file(st, mvn, repo, path).await {
        Ok(Some((h, m))) => {
            let mutable = is_mutable(path);
            crate::hf::serve_object(st, h, headers, head, |hm| {
                if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", m.sha1)) {
                    hm.insert(header::ETAG, v);
                }
                if let Ok(v) = HeaderValue::from_str(&m.sha1) {
                    hm.insert("x-checksum-sha1", v);
                }
                let cc = if mutable { "no-cache" } else { "public, max-age=31536000, immutable" };
                hm.insert(header::CACHE_CONTROL, HeaderValue::from_static(cc));
            })
            .await
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "not found"),
        Err(r) => r,
    }
}

async fn put(
    st: &Arc<AppState>,
    mvn: &Arc<MavenState>,
    p: &Principal,
    c: &Caller,
    repo: &str,
    path: &str,
    body: Body,
) -> Response {
    if !st.audit.is_healthy() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable; refusing deploys");
    }
    if let Some((base, alg)) = checksum_of(path) {
        // Checksums are derived; a PUT must agree with the stored file.
        let Ok(raw) = axum::body::to_bytes(body, 1024).await else {
            return err(StatusCode::BAD_REQUEST, "bad checksum");
        };
        let got = String::from_utf8_lossy(&raw).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let Some(n) = record_name(repo, base) else { return err(StatusCode::BAD_REQUEST, "invalid path") };
        return match load(mvn, &n).await {
            Ok(Some((_, m))) if m.digest(alg) == got => StatusCode::CREATED.into_response(),
            Ok(Some(_)) => err(StatusCode::BAD_REQUEST, format!("{alg} does not match the stored {base}")),
            Ok(None) => err(StatusCode::CONFLICT, format!("upload {base} before its checksum")),
            Err(r) => r,
        };
    }
    let Some(n) = record_name(repo, path) else {
        return err(StatusCode::BAD_REQUEST, "path does not fit the repository layout");
    };
    let ns = match Name::new(format!("maven/{repo}")) {
        Ok(x) => x,
        Err(e) => return core_err(e),
    };
    let x = match mvn.store.put_stream(ns, Box::pin(body.into_data_stream()), st.max_upload_bytes).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let meta = Meta::of(&x, "deploy");
    let (m2, n2, rec, mutable, sha) =
        (mvn.clone(), n.clone(), encode_record(&x.hash, &meta), is_mutable(path), meta.sha1.clone());
    let res = blocking(move || {
        if mutable {
            replace_any(&*m2.repo, &n2, &rec).map(|_| Ok(true))
        } else {
            crate::pkg::create_once(&*m2.repo, &n2, &rec, &|b| decode_record(b).is_some_and(|(_, m)| m.sha1 == sha))
        }
    })
    .await;
    let outcome = match &res {
        Ok(Ok(_)) => "ok",
        Ok(Err(_)) => "conflict",
        Err(_) => "error",
    };
    let _ = st
        .audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "maven_deploy",
            target: n.to_string(),
            outcome: outcome.into(),
            detail: Some(format!("sha1={} size={}", meta.sha1, meta.size)),
            remote: c.remote.clone(),
        })
        .await;
    match res {
        Ok(Ok(_)) => StatusCode::CREATED.into_response(),
        Ok(Err(_)) => {
            err(StatusCode::CONFLICT, format!("{path} is a release file and already exists with other content"))
        }
        Err(r) => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(is_mutable("org/x/demo/maven-metadata.xml"));
        assert!(is_mutable("org/x/demo/1.1-SNAPSHOT/demo-1.1-20261115.1-1.jar"));
        assert!(!is_mutable("org/x/demo/1.0/demo-1.0.jar"));
        assert_eq!(checksum_of("a/b.jar.sha1"), Some(("a/b.jar", "sha1")));
        assert_eq!(checksum_of("a/b.pom.sha512"), Some(("a/b.pom", "sha512")));
        assert_eq!(checksum_of("a/b.jar"), None);
        assert!(record_name("r", "org/x/demo/1.0/demo-1.0.jar").is_some());
        assert!(record_name("r", "../etc").is_none() && record_name("r", "a//b").is_none());
    }
}
