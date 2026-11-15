//! npm-compatible registries: `npm install` and `npm publish` work against them unchanged.
//!
//! - Packument: `GET /npm/<reg>/<name>` (also scoped `@scope/name`, which npm sends as
//!   `@scope%2fname`). Tarballs are served at `/npm/<reg>/<name>/-/<file>.tgz`.
//! - Publish: `PUT /npm/<reg>/<name>` with the JSON npm sends (`versions`, `dist-tags`, and
//!   base64 `_attachments`). The tarball's sha512 and sha1 must match the manifest's
//!   `dist.integrity` and `dist.shasum`. Versions are create-once: republishing an existing
//!   version is a 409 unless the bytes are identical.
//! - Upstream proxy-cache (`[npm.upstream.<reg>]`): packuments merge local versions with the
//!   upstream's (local wins), and upstream tarball URLs are rewritten to ours. The first
//!   download of a tarball fetches it, checks it against the upstream's integrity, and stores
//!   it; later downloads, including while the upstream is down, come from the store.
//!
//! **Storage.** Version `v` of package `p` in registry `r` is the name `npm/<r>/<seg>/<v>`
//! → a record (`P2PNPMV1`, content hash, JSON with the version manifest and digests).
//! `<seg>` is `p`, or `scope+name` for `@scope/name` ('+' cannot occur in npm names).
//! Dist-tags are `npm/<r>/<seg>/_tags/<tag>` → a `P2PNPMT1` blob holding the version. The
//! `npm` front claims `npm/`, and its GC tracer follows record → tarball. Authorization is
//! on `npm/<r>`.

// Handlers short-circuit with a ready `Response` as the error value.
#![allow(clippy::result_large_err)]

use crate::api::{AppState, Caller};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::{Access, Upstream};
use crate::pkg::{Store, blocking, core_err};
use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pico2peta_core::{Error, Hash, Name, ObjectStore, Observed, ReplaceOutcome, Repository, Result};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RECORD_MAGIC: &[u8; 8] = b"P2PNPMV1";
const TAG_MAGIC: &[u8; 8] = b"P2PNPMT1";
const MAX_PUBLISH_BODY: usize = 512 << 20;

pub struct NpmState {
    repo: Arc<dyn Repository>,
    store: Store,
    upstreams: BTreeMap<String, Upstream>,
    http: reqwest::Client,
    /// (registry, package) → when fetched, the upstream packument.
    pages: Mutex<HashMap<(String, String), CachedDoc>>,
}

type CachedDoc = (Instant, Arc<Value>);

impl NpmState {
    pub fn new(
        repo: Arc<dyn Repository>,
        local: Option<pico2peta_repo::LocalRepo>,
        upstreams: BTreeMap<String, Upstream>,
    ) -> std::result::Result<NpmState, String> {
        Ok(NpmState {
            store: Store { repo: repo.clone(), local },
            repo,
            upstreams,
            http: crate::pkg::http_client()?,
            pages: Mutex::new(HashMap::new()),
        })
    }
}

// ---------------------------------------------------------------------------
// Names and records
// ---------------------------------------------------------------------------

/// A validated package name: `name` or `@scope/name`.
#[derive(Clone, Debug)]
struct Pkg {
    scope: Option<String>,
    name: String,
}

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 214
        && !s.starts_with('.')
        && !s.starts_with('_')
        && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'))
}

impl Pkg {
    fn parse(s: &str) -> Option<Pkg> {
        match s.strip_prefix('@') {
            Some(rest) => {
                let (scope, name) = rest.split_once('/')?;
                (valid_part(scope) && valid_part(name)).then(|| Pkg { scope: Some(scope.into()), name: name.into() })
            }
            None => valid_part(s).then(|| Pkg { scope: None, name: s.into() }),
        }
    }
    fn full(&self) -> String {
        match &self.scope {
            Some(sc) => format!("@{sc}/{}", self.name),
            None => self.name.clone(),
        }
    }
    /// One core-name segment.
    fn seg(&self) -> String {
        match &self.scope {
            Some(sc) => format!("{sc}+{}", self.name),
            None => self.name.clone(),
        }
    }
    /// How npm addresses it in URLs (scoped names keep '@' and encode '/').
    fn url_part(&self) -> String {
        match &self.scope {
            Some(sc) => format!("@{sc}%2f{}", self.name),
            None => self.name.clone(),
        }
    }
    fn tarball(&self, version: &str) -> String {
        format!("{}-{version}.tgz", self.name)
    }
}

fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 256
        && v.as_bytes()[0].is_ascii_digit()
        && v.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
}

fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && !t.as_bytes()[0].is_ascii_digit()
        && t.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

fn version_name(reg: &str, p: &Pkg, v: &str) -> Option<Name> {
    Name::new(format!("npm/{reg}/{}/{v}", p.seg())).ok()
}

fn tag_name(reg: &str, p: &Pkg, t: &str) -> Option<Name> {
    Name::new(format!("npm/{reg}/{}/_tags/{t}", p.seg())).ok()
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Meta {
    /// The version manifest as published (or as the upstream had it).
    manifest: Value,
    integrity: String,
    shasum: String,
    size: u64,
    time: String,
    source: String,
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

/// GC: the tarball an npm version record references.
pub fn record_child(h: &Hash, store: &dyn ObjectStore) -> Result<Option<Hash>> {
    let size = store.size(h)?;
    if !(40..=64 << 20).contains(&size) {
        return Ok(None);
    }
    let head = store.get(h, Some(0..40))?;
    if !head.starts_with(RECORD_MAGIC) {
        return Ok(None);
    }
    Ok(Some(Hash(head[8..40].try_into().map_err(|_| Error::Corrupt("npm record".into()))?)))
}

/// Semver-ish ordering for picking `latest` when no tag says otherwise: numeric
/// `major.minor.patch`, and a release sorts after its prereleases.
fn version_key(v: &str) -> (Vec<u64>, bool, String) {
    let core = v.split('+').next().unwrap_or(v);
    let (nums, pre) = core.split_once('-').map_or((core, None), |(a, b)| (a, Some(b)));
    let n = nums.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    (n, pre.is_none(), pre.unwrap_or("").to_string())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn check(st: &AppState, p: &Principal, c: &Caller, reg: &str, need: Access) -> std::result::Result<(), Response> {
    let ns = format!("npm/{reg}");
    if st.authz.allows(p, &ns, need) {
        return Ok(());
    }
    st.metrics.denied.inc();
    st.audit.record_nowait(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "npm",
        target: ns.clone(),
        outcome: "denied".into(),
        detail: None,
        remote: c.remote.clone(),
    });
    let r = if p.name == crate::auth::ANONYMOUS {
        npm_err(StatusCode::UNAUTHORIZED, "authentication required")
    } else {
        npm_err(
            StatusCode::FORBIDDEN,
            format!("{} may not {} {ns}", p.name, if need == Access::Read { "read" } else { "write" }),
        )
    };
    Err(r)
}

/// npm shows `error` from JSON bodies.
fn npm_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, axum::Json(json!({ "error": msg.into() }))).into_response()
}

fn base_url(headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    let proto = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    format!("{proto}://{host}")
}

/// `/npm/{*rest}`.
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
    let Some(npm) = st.npm.clone() else { return npm_err(StatusCode::NOT_FOUND, "npm front disabled") };
    // Path is percent-decoded already: `@scope%2fname` arrives as `@scope/name`.
    let Some((reg, path)) = rest.split_once('/') else { return npm_err(StatusCode::NOT_FOUND, "no package") };
    if reg.is_empty() || !reg.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) {
        return npm_err(StatusCode::NOT_FOUND, "invalid registry name");
    }
    let path = path.trim_end_matches('/');
    // `<pkg>/-/<file>` is a tarball; anything else is a package document.
    let (pkg_part, file) = match path.split_once("/-/") {
        Some((a, f)) => (a, Some(f)),
        None => (path, None),
    };
    if pkg_part.starts_with("-/") {
        // Registry meta endpoints (`/-/ping`, `/-/whoami`, audits): minimal answers.
        return match pkg_part {
            "-/ping" => axum::Json(json!({})).into_response(),
            "-/whoami" => axum::Json(json!({ "username": p.name })).into_response(),
            _ => npm_err(StatusCode::NOT_FOUND, "not supported"),
        };
    }
    let Some(pkg) = Pkg::parse(pkg_part) else { return npm_err(StatusCode::NOT_FOUND, "invalid package name") };
    match (method, file) {
        (Method::GET | Method::HEAD, None) => {
            if let Err(r) = check(&st, &p, &c, reg, Access::Read) {
                return r;
            }
            packument(&npm, reg, &pkg, &headers).await
        }
        (m @ (Method::GET | Method::HEAD), Some(f)) => {
            if let Err(r) = check(&st, &p, &c, reg, Access::Read) {
                return r;
            }
            tarball(&st, &npm, reg, &pkg, f, &headers, m == Method::HEAD).await
        }
        (Method::PUT, None) => {
            if let Err(r) = check(&st, &p, &c, reg, Access::Write) {
                return r;
            }
            publish(&st, &npm, &p, &c, reg, &pkg, body).await
        }
        _ => npm_err(StatusCode::METHOD_NOT_ALLOWED, "unsupported operation"),
    }
}

/// Local versions: version → (record meta), and local dist-tags.
type Local = (BTreeMap<String, Meta>, BTreeMap<String, String>);

async fn local_versions(npm: &Arc<NpmState>, reg: &str, pkg: &Pkg) -> std::result::Result<Local, Response> {
    let (n2, prefix) = (npm.clone(), format!("npm/{reg}/{}", pkg.seg()));
    blocking(move || {
        let (mut vs, mut tags) = (BTreeMap::new(), BTreeMap::new());
        for (n, v) in n2.repo.list_refs(&prefix)? {
            let rel = &n.as_str()[prefix.len() + 1..];
            let b = n2.repo.get(&v.hash, None)?;
            if let Some(t) = rel.strip_prefix("_tags/") {
                if let Some(ver) = b.strip_prefix(TAG_MAGIC.as_slice()) {
                    tags.insert(t.to_string(), String::from_utf8_lossy(ver).into_owned());
                }
            } else if let Some((_, m)) = decode_record(&b) {
                vs.insert(rel.to_string(), m);
            }
        }
        Ok((vs, tags))
    })
    .await
}

/// The upstream packument for `pkg` (cached for the upstream's TTL).
async fn upstream_doc(
    npm: &NpmState,
    reg: &str,
    up: &Upstream,
    pkg: &Pkg,
) -> std::result::Result<Option<Arc<Value>>, String> {
    let key = (reg.to_string(), pkg.full());
    if let Some((t, v)) = npm.pages.lock().map_err(|_| "poisoned")?.get(&key)
        && t.elapsed() < Duration::from_secs(up.ttl_secs)
    {
        return Ok(Some(v.clone()));
    }
    let url = format!("{}/{}", up.url.trim_end_matches('/'), pkg.url_part());
    let mut rq = npm.http.get(&url).header(header::ACCEPT, "application/json").timeout(Duration::from_secs(60));
    if let Some(t) = &up.token {
        rq = rq.bearer_auth(t);
    }
    let resp = rq.send().await.map_err(|e| e.to_string())?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("upstream {url}: {}", resp.status()));
    }
    let doc = Arc::new(resp.json::<Value>().await.map_err(|e| format!("upstream {url}: {e}"))?);
    if let Ok(mut m) = npm.pages.lock() {
        if m.len() > 10_000 {
            m.clear();
        }
        m.insert(key, (Instant::now(), doc.clone()));
    }
    Ok(Some(doc))
}

fn with_tarball(mut manifest: Value, url: &str, integrity: &str, shasum: &str) -> Value {
    if !manifest.is_object() {
        manifest = json!({});
    }
    let dist = manifest.as_object_mut().map(|o| o.entry("dist").or_insert_with(|| json!({})));
    if let Some(d) = dist.and_then(Value::as_object_mut) {
        d.insert("tarball".into(), json!(url));
        if !integrity.is_empty() {
            d.insert("integrity".into(), json!(integrity));
        }
        if !shasum.is_empty() {
            d.insert("shasum".into(), json!(shasum));
        }
    }
    manifest
}

async fn packument(npm: &Arc<NpmState>, reg: &str, pkg: &Pkg, headers: &HeaderMap) -> Response {
    let (local, local_tags) = match local_versions(npm, reg, pkg).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let base = format!("{}/npm/{reg}/{}/-/", base_url(headers), pkg.url_part());
    let mut versions = Map::new();
    let mut time = Map::new();
    for (v, m) in &local {
        versions.insert(
            v.clone(),
            with_tarball(m.manifest.clone(), &format!("{base}{}", pkg.tarball(v)), &m.integrity, &m.shasum),
        );
        time.insert(v.clone(), json!(m.time));
    }
    let mut tags: BTreeMap<String, String> = BTreeMap::new();
    if let Some(up) = npm.upstreams.get(reg) {
        match upstream_doc(npm, reg, up, pkg).await {
            Ok(Some(doc)) => {
                for (v, man) in doc["versions"].as_object().into_iter().flatten() {
                    if versions.contains_key(v) || !valid_version(v) {
                        continue;
                    }
                    let (i, s) =
                        (man["dist"]["integrity"].as_str().unwrap_or(""), man["dist"]["shasum"].as_str().unwrap_or(""));
                    versions.insert(v.clone(), with_tarball(man.clone(), &format!("{base}{}", pkg.tarball(v)), i, s));
                    if let Some(t) = doc["time"].get(v) {
                        time.insert(v.clone(), t.clone());
                    }
                }
                for (t, v) in doc["dist-tags"].as_object().into_iter().flatten() {
                    if let Some(v) = v.as_str() {
                        tags.insert(t.clone(), v.to_string());
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(reg, package = %pkg.full(), error = %e, "upstream unavailable; serving cached versions only")
            }
        }
    }
    if versions.is_empty() {
        return npm_err(StatusCode::NOT_FOUND, format!("{} not found", pkg.full()));
    }
    tags.extend(local_tags);
    // Offline, an upstream tag may name a version we never cached.
    tags.retain(|_, v| versions.contains_key(v));
    if !tags.contains_key("latest") {
        let best = versions
            .keys()
            .filter(|v| !v.contains('-'))
            .max_by_key(|v| version_key(v))
            .or_else(|| versions.keys().max_by_key(|v| version_key(v)));
        if let Some(b) = best {
            tags.insert("latest".into(), b.clone());
        }
    }
    axum::Json(json!({
        "_id": pkg.full(), "name": pkg.full(), "dist-tags": tags, "versions": versions, "time": time,
    }))
    .into_response()
}

fn b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}

/// Checks a stored tarball against a manifest's `dist.integrity` / `dist.shasum`.
fn verify_integrity(
    x: &crate::pkg::Stored,
    integrity: Option<&str>,
    shasum: Option<&str>,
) -> std::result::Result<(), String> {
    let mut checked = false;
    if let Some(i) = integrity {
        // SRI may list several hashes; every sha512 one we can check must match.
        for part in i.split_whitespace() {
            if let Some(h) = part.strip_prefix("sha512-") {
                checked = true;
                if b64(h).as_deref() != Some(&x.sha512[..]) {
                    return Err(format!("integrity mismatch: content is {}", x.sri_sha512()));
                }
            }
        }
    }
    if let Some(s) = shasum.filter(|s| !s.is_empty()) {
        checked = true;
        if !s.eq_ignore_ascii_case(&x.sha1_hex()) {
            return Err(format!("shasum mismatch: content is {}", x.sha1_hex()));
        }
    }
    if checked { Ok(()) } else { Err("no integrity or shasum to verify against".into()) }
}

async fn publish(
    st: &Arc<AppState>,
    npm: &Arc<NpmState>,
    p: &Principal,
    c: &Caller,
    reg: &str,
    pkg: &Pkg,
    body: Body,
) -> Response {
    if !st.audit.is_healthy() {
        return npm_err(StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable; refusing publishes");
    }
    let Ok(raw) = axum::body::to_bytes(body, MAX_PUBLISH_BODY).await else {
        return npm_err(StatusCode::PAYLOAD_TOO_LARGE, "publish body too large");
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&raw) else { return npm_err(StatusCode::BAD_REQUEST, "bad JSON") };
    if doc["name"].as_str() != Some(&pkg.full()) {
        return npm_err(StatusCode::BAD_REQUEST, "package name in body does not match the URL");
    }
    let Some(versions) = doc["versions"].as_object().filter(|v| !v.is_empty()) else {
        // Tag-only or deprecate updates (no new version) are not supported yet.
        return npm_err(StatusCode::BAD_REQUEST, "publish carries no versions (unpublish/deprecate are not supported)");
    };
    let attachments = doc["_attachments"].as_object().cloned().unwrap_or_default();
    let ns = match Name::new(format!("npm/{reg}/{}", pkg.seg())) {
        Ok(n) => n,
        Err(e) => return core_err(e),
    };
    let mut published = Vec::new();
    for (v, manifest) in versions {
        if !valid_version(v) || manifest["version"].as_str() != Some(v) {
            return npm_err(StatusCode::BAD_REQUEST, format!("invalid version {v:?}"));
        }
        let file = pkg.tarball(v);
        // npm keys scoped attachments by the full name (`@scope/name-1.0.0.tgz`).
        let att = attachments.get(&format!("{}-{v}.tgz", pkg.full())).or_else(|| attachments.get(&file));
        let Some(data) = att.and_then(|a| a["data"].as_str()).and_then(b64) else {
            return npm_err(StatusCode::BAD_REQUEST, format!("missing attachment {file}"));
        };
        if data.len() as u64 > st.max_upload_bytes {
            return npm_err(StatusCode::PAYLOAD_TOO_LARGE, "tarball too large");
        }
        let (store, ns2) = (npm.store.clone(), ns.clone());
        let x = match blocking(move || store.put_reader(&ns2, std::io::Cursor::new(data))).await {
            Ok(x) => x,
            Err(r) => return r,
        };
        if let Err(m) =
            verify_integrity(&x, manifest["dist"]["integrity"].as_str(), manifest["dist"]["shasum"].as_str())
        {
            return npm_err(StatusCode::BAD_REQUEST, format!("{file}: {m}"));
        }
        let meta = Meta {
            manifest: manifest.clone(),
            integrity: x.sri_sha512(),
            shasum: x.sha1_hex(),
            size: x.size,
            time: crate::audit::rfc3339_now(),
            source: "publish".into(),
        };
        let Some(n) = version_name(reg, pkg, v) else { return npm_err(StatusCode::BAD_REQUEST, "invalid version") };
        let (repo, rec, sha) = (npm.repo.clone(), encode_record(&x.hash, &meta), meta.shasum.clone());
        let n2 = n.clone();
        let res = blocking(move || {
            crate::pkg::create_once(&*repo, &n2, &rec, &|b| decode_record(b).is_some_and(|(_, m)| m.shasum == sha))
        })
        .await;
        match res {
            Ok(Ok(_)) => published.push(v.clone()),
            Ok(Err(_)) => {
                audit(st, p, c, &n, "conflict").await;
                return npm_err(
                    StatusCode::CONFLICT,
                    format!("cannot publish over the previously published version {v}"),
                );
            }
            Err(r) => return r,
        }
        audit(st, p, c, &n, "ok").await;
    }
    // Dist-tags from the publish (npm sends `latest` or `--tag`).
    for (t, v) in doc["dist-tags"].as_object().into_iter().flatten() {
        let (Some(v), true) = (v.as_str(), valid_tag(t)) else { continue };
        if !published.iter().any(|x| x == v) {
            continue;
        }
        let Some(n) = tag_name(reg, pkg, t) else { continue };
        let (repo, val) = (npm.repo.clone(), [TAG_MAGIC.as_slice(), v.as_bytes()].concat());
        let res = blocking(move || {
            let h = repo.put(&val)?;
            for _ in 0..8 {
                let cur = repo.resolve(&n)?;
                let obs = cur.map_or(Observed::Absent, |c| Observed::Version(c.version));
                if let ReplaceOutcome::Ok { .. } = repo.replace(&n, &[(n.clone(), obs)], h)? {
                    return Ok(());
                }
            }
            Err(Error::Corrupt("dist-tag update did not settle".into()))
        })
        .await;
        if let Err(r) = res {
            return r;
        }
    }
    (StatusCode::CREATED, axum::Json(json!({ "ok": true, "id": pkg.full() }))).into_response()
}

async fn audit(st: &AppState, p: &Principal, c: &Caller, n: &Name, outcome: &str) {
    let _ = st
        .audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "npm_publish",
            target: n.to_string(),
            outcome: outcome.into(),
            detail: None,
            remote: c.remote.clone(),
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn tarball(
    st: &Arc<AppState>,
    npm: &Arc<NpmState>,
    reg: &str,
    pkg: &Pkg,
    file: &str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let Some(version) = file.strip_prefix(&format!("{}-", pkg.name)).and_then(|f| f.strip_suffix(".tgz")) else {
        return npm_err(StatusCode::NOT_FOUND, "no such tarball");
    };
    if !valid_version(version) {
        return npm_err(StatusCode::NOT_FOUND, "no such tarball");
    }
    let Some(n) = version_name(reg, pkg, version) else { return npm_err(StatusCode::NOT_FOUND, "no such tarball") };
    let (repo, n2) = (npm.repo.clone(), n.clone());
    let found = match blocking(move || {
        Ok(match repo.resolve(&n2)? {
            Some(v) => decode_record(&repo.get(&v.hash, None)?),
            None => None,
        })
    })
    .await
    {
        Ok(x) => x,
        Err(r) => return r,
    };
    let content = match found {
        Some((h, _)) => h,
        None => match fetch_upstream(st, npm, reg, pkg, version, &n).await {
            Ok(h) => h,
            Err(r) => return r,
        },
    };
    crate::hf::serve_object(st, content, headers, head, |hm| {
        hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    })
    .await
}

async fn fetch_upstream(
    st: &Arc<AppState>,
    npm: &Arc<NpmState>,
    reg: &str,
    pkg: &Pkg,
    version: &str,
    n: &Name,
) -> std::result::Result<Hash, Response> {
    let Some(up) = npm.upstreams.get(reg) else { return Err(npm_err(StatusCode::NOT_FOUND, "no such tarball")) };
    let doc = upstream_doc(npm, reg, up, pkg)
        .await
        .map_err(|e| npm_err(StatusCode::BAD_GATEWAY, e))?
        .ok_or_else(|| npm_err(StatusCode::NOT_FOUND, "no such package upstream"))?;
    let man = doc["versions"][version].clone();
    let Some(url) = man["dist"]["tarball"].as_str() else {
        return Err(npm_err(StatusCode::NOT_FOUND, "no such version upstream"));
    };
    let mut rq = npm.http.get(url);
    if let (Some(t), Ok(a), Ok(b)) = (&up.token, reqwest::Url::parse(url), reqwest::Url::parse(&up.url))
        && a.host_str() == b.host_str()
    {
        rq = rq.bearer_auth(t);
    }
    // On failure, forget the cached packument: the next one shows only what we have.
    let forget = || {
        if let Ok(mut m) = npm.pages.lock() {
            m.remove(&(reg.to_string(), pkg.full()));
        }
    };
    let resp = match rq.send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            forget();
            return Err(npm_err(StatusCode::BAD_GATEWAY, format!("upstream {url}: {}", r.status())));
        }
        Err(e) => {
            forget();
            return Err(npm_err(StatusCode::BAD_GATEWAY, format!("upstream: {e}")));
        }
    };
    st.metrics.upstream_fetches.get_or_create(&crate::metrics::FrontLabels { front: "npm".into() }).inc();
    let ns = Name::new(format!("npm/{reg}/{}", pkg.seg())).map_err(core_err)?;
    let x = npm.store.put_stream(ns, Box::pin(resp.bytes_stream()), st.max_upload_bytes).await?;
    verify_integrity(&x, man["dist"]["integrity"].as_str(), man["dist"]["shasum"].as_str())
        .map_err(|m| npm_err(StatusCode::BAD_GATEWAY, format!("upstream tarball: {m}")))?;
    let meta = Meta {
        manifest: man,
        integrity: x.sri_sha512(),
        shasum: x.sha1_hex(),
        size: x.size,
        time: crate::audit::rfc3339_now(),
        source: "upstream".into(),
    };
    let (repo, rec, sha, n2) = (npm.repo.clone(), encode_record(&x.hash, &meta), meta.shasum.clone(), n.clone());
    match blocking(move || {
        crate::pkg::create_once(&*repo, &n2, &rec, &|b| decode_record(b).is_some_and(|(_, m)| m.shasum == sha))
    })
    .await?
    {
        Ok(_) => Ok(x.hash),
        Err(m) => Err(npm_err(StatusCode::CONFLICT, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let p = Pkg::parse("@team/util").unwrap();
        assert_eq!(
            (p.full(), p.seg(), p.url_part(), p.tarball("1.0.0")),
            ("@team/util".into(), "team+util".into(), "@team%2futil".into(), "util-1.0.0.tgz".into())
        );
        assert!(Pkg::parse("left-pad").is_some());
        for bad in ["Upper", "_x", ".x", "@a", "@/x", "a/b", "x~y", "a+b"] {
            assert!(Pkg::parse(bad).is_none(), "{bad}");
        }
        assert!(valid_version("1.2.3-beta.1+build.5") && !valid_version("latest") && !valid_version("1/2"));
        let mut vs = ["1.10.0", "1.2.0", "1.10.0-rc.1", "0.9.9"];
        vs.sort_by_key(|v| version_key(v));
        assert_eq!(vs, ["0.9.9", "1.2.0", "1.10.0-rc.1", "1.10.0"]);
    }
}
