//! OCI Distribution API (`/v2/...`) on top of the repository.
//!
//! Hash mapping (see docs/NET_PROGRESS.md, "Design proposal: Git and OCI hash mapping"):
//! * Blobs and manifests are stored **verbatim** as core objects (BLAKE3 ids),
//!   so pulls are byte-identical and `sha256:` digests hold by construction.
//! * `oci/_digest` (one ref) → a [`SegMap`]: the global sha256 → BLAKE3 digest
//!   index for blobs and manifests (aux = size).
//! * `oci/<repo>/_blobs` (one ref) → a [`SegMap`] of the blobs linked into the repo.
//!   A blob is visible in a repo only if it is linked there.
//!   Per-blob mappings are not refs, because the RefStore keeps every ref in memory.
//! * `oci/<repo>/_manifests/sha256/<hex>` → BLAKE3 of the manifest bytes.
//! * `oci/<repo>/_mediatypes/sha256/<hex>` → BLAKE3 of the manifest's media type string.
//! * `oci/<repo>/tags/<tag>` → BLAKE3 of the manifest. The namespace rule on
//!   `oci/<repo>/tags` decides whether tags are mutable or create-once.
//!
//! Mappings are create-once: a digest can never be repointed at different
//! content, and segment maps refuse a remap. For GC, the maps are weak.
//! [`OciTracer`] keeps blobs alive through the manifests that reference them
//! (tags and manifest links are refs). A map entry whose blob was collected
//! reads as a miss, and the client simply pushes the blob again.
//!
//! Authorization: pull = `read` and push = `write` on namespace `oci/<repo>`.
//! Manifests and tags can be deleted (OCC `delete_ref`, namespace rules apply);
//! blob deletion returns 405 and unreferenced blobs are reclaimed by GC.

use crate::api::{AppState, Caller, IDLE_TIMEOUT, Unsatisfiable, parse_range, stream_reader};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::Access;
use crate::segmap::{AsyncMap, Key, SegCache, SegMap, Value};
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::StreamExt;
use pico2peta_core::{Error, Hash, Name, Observed, ReplaceOutcome, Repository};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::warn;

/// Largest manifest accepted (the spec recommends at least 4 MiB).
pub const MAX_MANIFEST: usize = 4 << 20;
/// Upload sessions idle for longer than this are discarded.
pub const UPLOAD_TTL: Duration = Duration::from_secs(24 * 3600);
const API_VERSION: &str = "registry/2.0";

const MT_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MT_DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const MT_DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

/// Upload sessions and their spool directory.
pub struct OciState {
    repo: Arc<dyn Repository>,
    maps: std::sync::Mutex<HashMap<String, Arc<SegMap>>>,
    cache: Arc<SegCache>,
    dir: PathBuf,
    uploads: std::sync::Mutex<HashMap<String, Arc<Mutex<Upload>>>>,
    max_sessions: usize,
}

struct Upload {
    repo: String,
    principal: String,
    path: PathBuf,
    offset: u64,
    hasher: Sha256,
    touched: Instant,
    /// Set once completed or cancelled; later requests get BLOB_UPLOAD_UNKNOWN.
    closed: bool,
}

impl OciState {
    /// Uses (and empties) `dir` for spooling chunked uploads.
    pub fn new(repo: Arc<dyn Repository>, dir: PathBuf, max_sessions: usize) -> std::io::Result<OciState> {
        if dir.exists() {
            // Sessions do not survive restarts; drop stale spool files.
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(OciState {
            repo,
            maps: std::sync::Mutex::new(HashMap::new()),
            cache: Arc::default(),
            dir,
            uploads: std::sync::Mutex::new(HashMap::new()),
            max_sessions,
        })
    }

    fn map(&self, ref_name: String) -> OciResult<Arc<SegMap>> {
        let mut m = self.maps.lock().map_err(|_| oe(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "poisoned"))?;
        if let Some(x) = m.get(&ref_name) {
            return Ok(x.clone());
        }
        let n = name(ref_name.clone())?;
        let x = Arc::new(SegMap::new(self.repo.clone(), n, self.cache.clone()));
        m.insert(ref_name, x.clone());
        Ok(x)
    }

    fn digest_map(&self) -> OciResult<Arc<SegMap>> {
        self.map("oci/_digest".into())
    }

    fn blob_map(&self, repo: &str) -> OciResult<Arc<SegMap>> {
        self.map(format!("oci/{repo}/_blobs"))
    }

    /// The live BLAKE3 of blob `hex` linked into `repo`.
    async fn blob(&self, repo: &str, hex: &str) -> OciResult<Option<Hash>> {
        Ok(self.blob_map(repo)?.get_live(hex_key(hex)?).await.map_err(OciError::from)?.map(|v| v.hash))
    }

    /// A manifest (by its link) or blob of `repo` with this sha256 digest (provenance ids).
    pub(crate) async fn resolve_digest(&self, repo: &str, hex: &str) -> std::result::Result<Option<Hash>, String> {
        let n = manifest_link(repo, hex).map_err(|_| "invalid repository or digest".to_string())?;
        if let Some(v) = self.repo.resolve(&n).map_err(|e| e.to_string())? {
            return Ok(Some(v.hash));
        }
        self.blob(repo, hex).await.map_err(|_| "invalid repository or digest".to_string())
    }

    async fn digest(&self, hex: &str) -> OciResult<Option<Hash>> {
        Ok(self.digest_map()?.get_live(hex_key(hex)?).await.map_err(OciError::from)?.map(|v| v.hash))
    }

    async fn index(&self, hex: &str, h: Hash, size: u64) -> OciResult<()> {
        self.digest_map()?.insert(vec![(hex_key(hex)?, Value { hash: h, aux: size })]).await.map_err(OciError::from)?;
        Ok(())
    }

    async fn link(&self, repo: &str, hex: &str, h: Hash, size: u64) -> OciResult<()> {
        self.blob_map(repo)?
            .insert(vec![(hex_key(hex)?, Value { hash: h, aux: size })])
            .await
            .map_err(OciError::from)?;
        Ok(())
    }
}

/// 64 hex chars → 32-byte key.
fn hex_key(hex: &str) -> OciResult<Key> {
    let mut k = [0u8; 32];
    if hex.len() != 64 {
        return Err(oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"));
    }
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"))?;
    }
    Ok(k)
}

impl OciState {
    fn get(&self, id: &str) -> Option<Arc<Mutex<Upload>>> {
        self.uploads.lock().ok()?.get(id).cloned()
    }

    fn remove(&self, id: &str) {
        if let Ok(mut m) = self.uploads.lock() {
            m.remove(id);
        }
    }

    /// Removes sessions idle for longer than [`UPLOAD_TTL`].
    pub async fn reap(&self) {
        let all: Vec<(String, Arc<Mutex<Upload>>)> = match self.uploads.lock() {
            Ok(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => return,
        };
        for (id, u) in all {
            let Ok(u) = u.try_lock() else { continue };
            if u.touched.elapsed() > UPLOAD_TTL {
                let _ = tokio::fs::remove_file(&u.path).await;
                drop(u);
                self.remove(&id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An OCI-format error response.
pub struct OciError {
    status: StatusCode,
    code: &'static str,
    message: String,
    extra: Vec<(header::HeaderName, HeaderValue)>,
}

fn oe(status: StatusCode, code: &'static str, message: impl Into<String>) -> OciError {
    OciError { status, code, message: message.into(), extra: Vec::new() }
}

impl From<Error> for OciError {
    fn from(e: Error) -> Self {
        let api = crate::error::ApiError::from(e);
        let code = match api.status {
            StatusCode::NOT_FOUND => "BLOB_UNKNOWN",
            StatusCode::BAD_REQUEST => "NAME_INVALID",
            StatusCode::FORBIDDEN => "DENIED",
            StatusCode::RANGE_NOT_SATISFIABLE => "RANGE_INVALID",
            _ => "UNKNOWN",
        };
        OciError { status: api.status, code, message: api.message, extra: Vec::new() }
    }
}

impl From<crate::error::ApiError> for OciError {
    fn from(api: crate::error::ApiError) -> Self {
        let code = match api.status {
            StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
            StatusCode::FORBIDDEN => "DENIED",
            StatusCode::NOT_FOUND => "BLOB_UNKNOWN",
            StatusCode::PAYLOAD_TOO_LARGE => "SIZE_INVALID",
            StatusCode::SERVICE_UNAVAILABLE => "TOOMANYREQUESTS",
            _ => "UNKNOWN",
        };
        OciError { status: api.status, code, message: api.message, extra: Vec::new() }
    }
}

impl IntoResponse for OciError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({"errors": [{"code": self.code, "message": self.message}]});
        let mut r = (self.status, Json(body)).into_response();
        for (k, v) in self.extra {
            r.headers_mut().insert(k, v);
        }
        r.headers_mut().insert("docker-distribution-api-version", HeaderValue::from_static(API_VERSION));
        r
    }
}

type OciResult<T> = Result<T, OciError>;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// OCI repository name: `[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(/…)*`, ≤ 255 bytes.
pub fn valid_repo(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    name.split('/').all(|comp| {
        let b = comp.as_bytes();
        if b.is_empty() || !is_alnum(b[0]) || !is_alnum(b[b.len() - 1]) {
            return false;
        }
        let mut i = 0;
        while i < b.len() {
            if is_alnum(b[i]) {
                i += 1;
                continue;
            }
            // separator run: '.', '_', '__', or '-'+
            let start = i;
            while i < b.len() && !is_alnum(b[i]) {
                i += 1;
            }
            let sep = &comp[start..i];
            if !(sep == "." || sep == "_" || sep == "__" || sep.bytes().all(|c| c == b'-')) {
                return false;
            }
        }
        true
    })
}

fn is_alnum(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// Tag: `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}`.
pub fn valid_tag(t: &str) -> bool {
    let b = t.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && (b[0].is_ascii_alphanumeric() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// `sha256:<64 lowercase hex>` → hex. Other algorithms are unsupported.
pub fn parse_digest(d: &str) -> Option<&str> {
    let hex = d.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))).then_some(hex)
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

enum Route {
    Base,
    Blob(String, String),
    Upload(String, Option<String>),
    Manifest(String, String),
    Tags(String),
    Referrers(String, String),
}

fn route(rest: &str) -> Option<Route> {
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        return Some(Route::Base);
    }
    if let Some(name) = rest.strip_suffix("/tags/list") {
        return Some(Route::Tags(name.into()));
    }
    if let Some(i) = rest.rfind("/blobs/uploads") {
        let name = &rest[..i];
        let tail = rest[i + "/blobs/uploads".len()..].trim_start_matches('/');
        return Some(Route::Upload(name.into(), (!tail.is_empty()).then(|| tail.to_string())));
    }
    if let Some(i) = rest.rfind("/blobs/") {
        return Some(Route::Blob(rest[..i].into(), rest[i + 7..].into()));
    }
    if let Some(i) = rest.rfind("/referrers/") {
        return Some(Route::Referrers(rest[..i].into(), rest[i + 11..].into()));
    }
    if let Some(i) = rest.rfind("/manifests/") {
        return Some(Route::Manifest(rest[..i].into(), rest[i + 11..].into()));
    }
    None
}

// ---------------------------------------------------------------------------
// Name helpers
// ---------------------------------------------------------------------------

fn ns(repo: &str) -> String {
    format!("oci/{repo}")
}

fn name(s: String) -> OciResult<Name> {
    Name::new(s).map_err(|e| oe(StatusCode::BAD_REQUEST, "NAME_INVALID", e.to_string()))
}

fn manifest_link(repo: &str, hex: &str) -> OciResult<Name> {
    name(format!("oci/{repo}/_manifests/sha256/{hex}"))
}
fn mediatype_link(repo: &str, hex: &str) -> OciResult<Name> {
    name(format!("oci/{repo}/_mediatypes/sha256/{hex}"))
}
fn referrers_prefix(repo: &str, subject_hex: &str) -> String {
    format!("oci/{repo}/_referrers/sha256/{subject_hex}")
}
fn referrer_link(repo: &str, subject_hex: &str, hex: &str) -> OciResult<Name> {
    name(format!("{}/sha256/{hex}", referrers_prefix(repo, subject_hex)))
}
fn tag_name(repo: &str, tag: &str) -> OciResult<Name> {
    name(format!("oci/{repo}/tags/{tag}"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> pico2peta_core::Result<T> + Send + 'static) -> OciResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(OciError::from),
        Err(e) => Err(oe(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", format!("worker panicked: {e}"))),
    }
}

async fn resolve(repo: &Arc<dyn Repository>, n: Name) -> OciResult<Option<Hash>> {
    let r = repo.clone();
    Ok(blocking(move || r.resolve(&n)).await?.map(|v| v.hash))
}

/// Creates `n → h` unless it exists. Existing mappings must agree with `h`.
async fn ensure_mapping(repo: &Arc<dyn Repository>, n: Name, h: Hash) -> OciResult<()> {
    for _ in 0..3 {
        let r = repo.clone();
        let n2 = n.clone();
        match blocking(move || r.resolve(&n2)).await? {
            Some(v) if v.hash == h => return Ok(()),
            Some(v) => {
                return Err(oe(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    format!("mapping {n} points at {} but content hashes to {h}", v.hash),
                ));
            }
            None => {}
        }
        let r = repo.clone();
        let n2 = n.clone();
        match blocking(move || r.replace(&n2, &[(n2.clone(), Observed::Absent)], h)).await? {
            ReplaceOutcome::Ok { .. } => return Ok(()),
            ReplaceOutcome::Conflict { .. } => continue, // raced with another pusher; re-check
        }
    }
    Err(oe(StatusCode::SERVICE_UNAVAILABLE, "TOOMANYREQUESTS", format!("contention on {n}")))
}

/// Moves a tag (honouring the namespace rule on `oci/<repo>/tags`).
async fn set_tag(repo: &Arc<dyn Repository>, n: Name, h: Hash) -> OciResult<()> {
    for _ in 0..5 {
        let r = repo.clone();
        let n2 = n.clone();
        let cur = blocking(move || r.resolve(&n2)).await?;
        if cur.is_some_and(|v| v.hash == h) {
            return Ok(());
        }
        let observed = pico2peta_core::RefValue::observed(cur);
        let r = repo.clone();
        let n2 = n.clone();
        match blocking(move || r.replace(&n2, &[(n2.clone(), observed)], h)).await {
            Ok(ReplaceOutcome::Ok { .. }) => return Ok(()),
            Ok(ReplaceOutcome::Conflict { .. }) => continue,
            Err(e) if e.status == StatusCode::FORBIDDEN => {
                return Err(oe(StatusCode::FORBIDDEN, "DENIED", format!("tag is immutable: {}", e.message)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(oe(StatusCode::SERVICE_UNAVAILABLE, "TOOMANYREQUESTS", format!("contention on {n}")))
}

fn location(h: &mut HeaderMap, path: String) {
    if let Ok(v) = HeaderValue::from_str(&path) {
        h.insert(header::LOCATION, v);
    }
}

fn digest_header(h: &mut HeaderMap, hex: &str) {
    if let Ok(v) = HeaderValue::from_str(&format!("sha256:{hex}")) {
        h.insert("docker-content-digest", v);
    }
}

fn api_header(h: &mut HeaderMap) {
    h.insert("docker-distribution-api-version", HeaderValue::from_static(API_VERSION));
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// `GET /v2/`.
pub async fn base(Extension(_p): Extension<Principal>) -> Response {
    let mut r = (StatusCode::OK, Json(serde_json::json!({}))).into_response();
    api_header(r.headers_mut());
    r
}

/// Everything under `/v2/<name>/...`.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    method: Method,
    Path(rest): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let res = dispatch_inner(&st, &p, &c, &method, &rest, &q, &headers, body).await;
    match res {
        Ok(mut r) => {
            api_header(r.headers_mut());
            r
        }
        Err(e) => e.into_response(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_inner(
    st: &Arc<AppState>,
    p: &Principal,
    c: &Caller,
    method: &Method,
    rest: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Body,
) -> OciResult<Response> {
    let Some(oci) = st.oci.clone() else {
        return Err(oe(StatusCode::NOT_FOUND, "UNSUPPORTED", "OCI distribution is disabled"));
    };
    let r = route(rest).ok_or_else(|| oe(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "unknown endpoint"))?;
    let repo_name = match &r {
        Route::Base => return Ok(base(Extension(p.clone())).await),
        Route::Blob(n, _) | Route::Upload(n, _) | Route::Manifest(n, _) | Route::Tags(n) | Route::Referrers(n, _) => {
            n.clone()
        }
    };
    if !valid_repo(&repo_name) {
        return Err(oe(StatusCode::BAD_REQUEST, "NAME_INVALID", format!("invalid repository name {repo_name:?}")));
    }
    let need = match (method, &r) {
        (&Method::GET | &Method::HEAD, _) => Access::Read,
        _ => Access::Write,
    };
    let space = ns(&repo_name);
    if !st.authz.allows(p, &space, need) {
        st.metrics.denied.inc();
        st.audit.record_nowait(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "oci",
            target: space.clone(),
            outcome: "denied".into(),
            detail: Some(format!("{method} {rest}")),
            remote: c.remote.clone(),
        });
        return Err(oe(StatusCode::FORBIDDEN, "DENIED", format!("{} may not access {repo_name}", p.name)));
    }
    if need == Access::Write && !st.audit.is_healthy() {
        return Err(oe(StatusCode::SERVICE_UNAVAILABLE, "UNAVAILABLE", "audit log unavailable; refusing pushes"));
    }
    match (method.clone(), r) {
        (Method::GET | Method::HEAD, Route::Blob(n, d)) => {
            get_blob(st, &oci, &n, &d, headers, method == Method::HEAD).await
        }
        (Method::DELETE, Route::Blob(..)) => Err(oe(
            StatusCode::METHOD_NOT_ALLOWED,
            "UNSUPPORTED",
            "blob deletion is not supported; GC reclaims unreferenced blobs",
        )),
        (Method::DELETE, Route::Manifest(n, reference)) => delete_manifest(st, p, c, &n, &reference).await,
        (Method::POST, Route::Upload(n, None)) => start_upload(st, &oci, p, c, &n, q, headers, body).await,
        (Method::GET, Route::Upload(n, Some(id))) => upload_status(&oci, p, &n, &id).await,
        (Method::PATCH, Route::Upload(n, Some(id))) => patch_upload(st, &oci, p, &n, &id, headers, body).await,
        (Method::PUT, Route::Upload(n, Some(id))) => finish_upload(st, &oci, p, c, &n, &id, q, headers, body).await,
        (Method::DELETE, Route::Upload(n, Some(id))) => cancel_upload(&oci, p, &n, &id).await,
        (Method::GET | Method::HEAD, Route::Manifest(n, reference)) => {
            get_manifest(st, &n, &reference, method == Method::HEAD).await
        }
        (Method::PUT, Route::Manifest(n, reference)) => {
            put_manifest(st, &oci, p, c, &n, &reference, headers, body).await
        }
        (Method::GET, Route::Tags(n)) => list_tags(st, &n, q).await,
        (Method::GET, Route::Referrers(n, d)) => referrers(st, &n, &d, q).await,
        _ => Err(oe(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED", format!("{method} not supported here"))),
    }
}

// ---------------------------------------------------------------------------
// Blobs
// ---------------------------------------------------------------------------

async fn get_blob(
    st: &Arc<AppState>,
    oci: &Arc<OciState>,
    repo: &str,
    digest: &str,
    headers: &HeaderMap,
    head: bool,
) -> OciResult<Response> {
    let hex = parse_digest(digest).ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"))?;
    let h = oci
        .blob(repo, hex)
        .await?
        .ok_or_else(|| oe(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", format!("blob {digest} not found in {repo}")))?;
    let r = st.oci_repo();
    let size = blocking(move || r.size(&h)).await?;
    let range = match parse_range(headers.get(header::RANGE), size) {
        Ok(r) => r,
        Err(Unsatisfiable) => {
            let mut e = oe(StatusCode::RANGE_NOT_SATISFIABLE, "RANGE_INVALID", "unsatisfiable range");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                e.extra.push((header::CONTENT_RANGE, v));
            }
            return Err(e);
        }
    };
    let len = range.as_ref().map_or(size, |r| r.end - r.start);
    let mut resp = if head {
        Response::new(Body::empty())
    } else {
        let r = st.oci_repo();
        let rr = range.clone();
        let reader = blocking(move || r.open(&h, rr)).await?;
        Response::new(stream_reader(reader, len, st.metrics.clone()))
    };
    let hm = resp.headers_mut();
    hm.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    hm.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    digest_header(hm, hex);
    if let Ok(v) = HeaderValue::from_str(&format!("\"sha256:{hex}\"")) {
        hm.insert(header::ETAG, v);
    }
    if let Some(r) = range {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{size}", r.start, r.end - 1)) {
            resp.headers_mut().insert(header::CONTENT_RANGE, v);
        }
    }
    Ok(resp)
}

fn upload_response(status: StatusCode, repo: &str, id: &str, offset: u64) -> Response {
    let mut r = status.into_response();
    let h = r.headers_mut();
    location(h, format!("/v2/{repo}/blobs/uploads/{id}"));
    // Range is inclusive; an empty upload reports 0-0 per common practice.
    if let Ok(v) = HeaderValue::from_str(&format!("0-{}", offset.saturating_sub(1))) {
        h.insert(header::RANGE, v);
    }
    if let Ok(v) = HeaderValue::from_str(id) {
        h.insert("docker-upload-uuid", v);
    }
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(0));
    r
}

fn new_upload_id() -> String {
    format!("{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..))
}

#[allow(clippy::too_many_arguments)]
async fn start_upload(
    st: &Arc<AppState>,
    oci: &Arc<OciState>,
    p: &Principal,
    c: &Caller,
    repo: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Body,
) -> OciResult<Response> {
    // Cross-repository mount; if not mountable, fall through to a regular upload session (spec).
    if let Some(mount) = q.get("mount")
        && let Some(hex) = parse_digest(mount)
        && let Some(from) = q.get("from").filter(|f| valid_repo(f))
        && st.authz.allows(p, &ns(from), Access::Read)
        && let Some(h) = oci.blob(from, hex).await?
    {
        let r = st.oci_repo();
        let size = blocking(move || r.size(&h)).await?;
        oci.link(repo, hex, h, size).await?;
        audit_push(st, p, c, repo, "mount", &format!("sha256:{hex} from={from}")).await?;
        let mut r = StatusCode::CREATED.into_response();
        location(r.headers_mut(), format!("/v2/{repo}/blobs/sha256:{hex}"));
        digest_header(r.headers_mut(), hex);
        return Ok(r);
    }
    let id = new_upload_id();
    let path = oci.dir.join(&id);
    tokio::fs::File::create(&path).await.map_err(io_err)?;
    let up = Upload {
        repo: repo.to_string(),
        principal: p.name.clone(),
        path,
        offset: 0,
        hasher: Sha256::new(),
        touched: Instant::now(),
        closed: false,
    };
    let spool = up.path.clone();
    let full = {
        let mut m = oci.uploads.lock().map_err(|_| oe(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "poisoned"))?;
        if m.len() >= oci.max_sessions {
            true
        } else {
            m.insert(id.clone(), Arc::new(Mutex::new(up)));
            false
        }
    };
    if full {
        let _ = tokio::fs::remove_file(&spool).await;
        return Err(oe(StatusCode::TOO_MANY_REQUESTS, "TOOMANYREQUESTS", "too many open upload sessions"));
    }
    // Monolithic upload in the POST itself.
    if let Some(d) = q.get("digest") {
        let mut q2 = q.clone();
        q2.insert("digest".into(), d.clone());
        return finish_upload(st, oci, p, c, repo, &id, &q2, headers, body).await;
    }
    Ok(upload_response(StatusCode::ACCEPTED, repo, &id, 0))
}

fn io_err(e: std::io::Error) -> OciError {
    oe(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", format!("upload spool: {e}"))
}

async fn session(oci: &OciState, p: &Principal, repo: &str, id: &str) -> OciResult<Arc<Mutex<Upload>>> {
    let unknown = || oe(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN", format!("upload {id} not found"));
    let s = oci.get(id).ok_or_else(unknown)?;
    {
        let u = s.lock().await;
        if u.closed || u.repo != repo || u.principal != p.name {
            return Err(unknown());
        }
    }
    Ok(s)
}

async fn upload_status(oci: &Arc<OciState>, p: &Principal, repo: &str, id: &str) -> OciResult<Response> {
    let s = session(oci, p, repo, id).await?;
    let u = s.lock().await;
    Ok(upload_response(StatusCode::NO_CONTENT, repo, id, u.offset))
}

async fn cancel_upload(oci: &Arc<OciState>, p: &Principal, repo: &str, id: &str) -> OciResult<Response> {
    let s = session(oci, p, repo, id).await?;
    let mut u = s.lock().await;
    u.closed = true;
    let _ = tokio::fs::remove_file(&u.path).await;
    drop(u);
    oci.remove(id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Appends `body` to the session file, hashing as it goes.
async fn append(st: &AppState, u: &mut Upload, body: Body) -> OciResult<()> {
    let mut f = tokio::fs::OpenOptions::new().append(true).open(&u.path).await.map_err(io_err)?;
    let mut stream = body.into_data_stream();
    loop {
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Err(_) => return Err(oe(StatusCode::REQUEST_TIMEOUT, "BLOB_UPLOAD_INVALID", "upload idle timeout")),
            Ok(None) => break,
            Ok(Some(Err(e))) => return Err(oe(StatusCode::BAD_REQUEST, "BLOB_UPLOAD_INVALID", format!("body: {e}"))),
            Ok(Some(Ok(b))) => b,
        };
        if u.offset + chunk.len() as u64 > st.max_upload_bytes {
            return Err(oe(StatusCode::PAYLOAD_TOO_LARGE, "SIZE_INVALID", "blob exceeds max_upload_bytes"));
        }
        f.write_all(&chunk).await.map_err(io_err)?;
        u.hasher.update(&chunk);
        u.offset += chunk.len() as u64;
        st.metrics.bytes_in.inc_by(chunk.len() as u64);
    }
    f.flush().await.map_err(io_err)?;
    u.touched = Instant::now();
    Ok(())
}

/// Checks an optional `Content-Range: <start>-<end>` against the session offset.
fn check_content_range(headers: &HeaderMap, offset: u64) -> OciResult<()> {
    let Some(v) = headers.get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) else { return Ok(()) };
    let v = v.trim().trim_start_matches("bytes ");
    let start = v.split('-').next().and_then(|s| s.trim().parse::<u64>().ok());
    match start {
        Some(s) if s == offset => Ok(()),
        _ => {
            let mut e =
                oe(StatusCode::RANGE_NOT_SATISFIABLE, "BLOB_UPLOAD_INVALID", format!("expected chunk at {offset}"));
            if let Ok(v) = HeaderValue::from_str(&format!("0-{}", offset.saturating_sub(1))) {
                e.extra.push((header::RANGE, v));
            }
            Err(e)
        }
    }
}

async fn patch_upload(
    st: &Arc<AppState>,
    oci: &Arc<OciState>,
    p: &Principal,
    repo: &str,
    id: &str,
    headers: &HeaderMap,
    body: Body,
) -> OciResult<Response> {
    let s = session(oci, p, repo, id).await?;
    let mut u = s.lock().await;
    check_content_range(headers, u.offset)?;
    append(st, &mut u, body).await?;
    Ok(upload_response(StatusCode::ACCEPTED, repo, id, u.offset))
}

#[allow(clippy::too_many_arguments)]
async fn finish_upload(
    st: &Arc<AppState>,
    oci: &Arc<OciState>,
    p: &Principal,
    c: &Caller,
    repo: &str,
    id: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Body,
) -> OciResult<Response> {
    let digest = q.get("digest").ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "missing ?digest="))?;
    let hex = parse_digest(digest)
        .ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", format!("unsupported digest {digest:?}")))?
        .to_string();
    let s = session(oci, p, repo, id).await?;
    let mut u = s.lock().await;
    if headers.get(header::CONTENT_RANGE).is_some() {
        check_content_range(headers, u.offset)?;
    }
    append(st, &mut u, body).await?;
    let got = hex::encode_lower(&u.hasher.clone().finalize());
    if got != hex {
        u.closed = true;
        let _ = tokio::fs::remove_file(&u.path).await;
        drop(u);
        oci.remove(id);
        return Err(oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", format!("content is sha256:{got}, not {digest}")));
    }
    // Dedup: if the digest is already indexed, only link it.
    let size = u.offset;
    let h = match oci.digest(&hex).await? {
        Some(h) => h,
        None => {
            let r = st.oci_repo();
            let path = u.path.clone();
            // Chunk with the namespace's chunker (e.g. Xet for `oci/models/...`, API v1.11).
            let local = st.local.clone();
            let ns_name = name(ns(repo))?;
            let h = blocking(move || {
                let mut f = std::fs::File::open(&path)?;
                match local {
                    Some(l) => l.put_stream_for(&ns_name, &mut f),
                    None => r.put_stream(&mut f),
                }
            })
            .await?;
            oci.index(&hex, h, size).await?;
            h
        }
    };
    oci.link(repo, &hex, h, size).await?;
    u.closed = true;
    let _ = tokio::fs::remove_file(&u.path).await;
    drop(u);
    oci.remove(id);
    audit_push(st, p, c, repo, "blob", &format!("sha256:{hex} size={size}")).await?;
    let mut r = StatusCode::CREATED.into_response();
    location(r.headers_mut(), format!("/v2/{repo}/blobs/sha256:{hex}"));
    digest_header(r.headers_mut(), &hex);
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(0));
    Ok(r)
}

async fn audit_push(st: &AppState, p: &Principal, c: &Caller, repo: &str, what: &str, detail: &str) -> OciResult<()> {
    st.audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "oci_push",
            target: ns(repo),
            outcome: "ok".into(),
            detail: Some(format!("{what} {detail}")),
            remote: c.remote.clone(),
        })
        .await
        .map_err(|e| {
            st.metrics.audit_failures.inc();
            oe(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", format!("audit write failed: {e}"))
        })
}

// ---------------------------------------------------------------------------
// Manifests
// ---------------------------------------------------------------------------

/// Digests a manifest references that must already exist in the repo.
/// `(blobs, child manifests, subject)` referenced by a manifest.
fn manifest_refs(body: &[u8], media_type: &str) -> OciResult<(Vec<String>, Vec<String>, Option<String>)> {
    let invalid = |m: String| oe(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", m);
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| invalid(format!("not JSON: {e}")))?;
    if let Some(mt) = v.get("mediaType").and_then(|m| m.as_str())
        && mt != media_type
    {
        return Err(invalid(format!("mediaType {mt:?} does not match Content-Type {media_type:?}")));
    }
    let digest_of = |d: &serde_json::Value| -> OciResult<String> {
        let s = d.get("digest").and_then(|x| x.as_str()).ok_or_else(|| invalid("descriptor without digest".into()))?;
        parse_digest(s).map(str::to_string).ok_or_else(|| invalid(format!("unsupported digest {s:?}")))
    };
    let mut blobs = Vec::new();
    let mut manifests = Vec::new();
    match media_type {
        MT_OCI_MANIFEST | MT_DOCKER_MANIFEST => {
            let cfg = v.get("config").ok_or_else(|| invalid("image manifest without config".into()))?;
            blobs.push(digest_of(cfg)?);
            for l in v.get("layers").and_then(|l| l.as_array()).ok_or_else(|| invalid("missing layers".into()))? {
                blobs.push(digest_of(l)?);
            }
        }
        MT_OCI_INDEX | MT_DOCKER_LIST => {
            for m in v.get("manifests").and_then(|l| l.as_array()).ok_or_else(|| invalid("missing manifests".into()))? {
                manifests.push(digest_of(m)?);
            }
        }
        // Artifact or unknown manifest types: stored as-is, without reference checks.
        _ => {}
    }
    let subject = match v.get("subject") {
        Some(d) => Some(digest_of(d)?),
        None => None,
    };
    Ok((blobs, manifests, subject))
}

#[allow(clippy::too_many_arguments)]
async fn put_manifest(
    st: &Arc<AppState>,
    oci: &Arc<OciState>,
    p: &Principal,
    c: &Caller,
    repo: &str,
    reference: &str,
    headers: &HeaderMap,
    body: Body,
) -> OciResult<Response> {
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| oe(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", "Content-Type is required"))?;
    let bytes = axum::body::to_bytes(body, MAX_MANIFEST).await.map_err(|_| {
        oe(StatusCode::PAYLOAD_TOO_LARGE, "SIZE_INVALID", format!("manifest larger than {MAX_MANIFEST}"))
    })?;
    let hex = sha256_hex(&bytes);
    let tag = if let Some(want) = reference.strip_prefix("sha256:") {
        if want != hex {
            return Err(oe(
                StatusCode::BAD_REQUEST,
                "DIGEST_INVALID",
                format!("manifest is sha256:{hex}, not {reference}"),
            ));
        }
        None
    } else if valid_tag(reference) {
        Some(reference.to_string())
    } else {
        return Err(oe(StatusCode::BAD_REQUEST, "TAG_INVALID", format!("invalid tag {reference:?}")));
    };
    let (blobs, manifests, subject) = manifest_refs(&bytes, &media_type)?;
    for b in blobs {
        if oci.blob(repo, &b).await?.is_none() {
            return Err(oe(StatusCode::BAD_REQUEST, "MANIFEST_BLOB_UNKNOWN", format!("blob sha256:{b} not in {repo}")));
        }
    }
    for m in manifests {
        if resolve(&st.oci_repo(), manifest_link(repo, &m)?).await?.is_none() {
            return Err(oe(
                StatusCode::BAD_REQUEST,
                "MANIFEST_BLOB_UNKNOWN",
                format!("manifest sha256:{m} not in {repo}"),
            ));
        }
    }
    let r = st.oci_repo();
    let b2 = bytes.clone();
    let h = blocking(move || r.put(&b2)).await?;
    let r = st.oci_repo();
    let mt = media_type.clone();
    let mh = blocking(move || r.put(mt.as_bytes())).await?;
    oci.index(&hex, h, bytes.len() as u64).await?;
    ensure_mapping(&st.oci_repo(), mediatype_link(repo, &hex)?, mh).await?;
    ensure_mapping(&st.oci_repo(), manifest_link(repo, &hex)?, h).await?;
    if let Some(t) = &tag {
        set_tag(&st.oci_repo(), tag_name(repo, t)?, h).await?;
    }
    if let Some(sub) = &subject {
        ensure_mapping(&st.oci_repo(), referrer_link(repo, sub, &hex)?, h).await?;
    }
    audit_push(st, p, c, repo, "manifest", &format!("sha256:{hex} tag={}", tag.as_deref().unwrap_or("-"))).await?;
    let m = [
        ("repo", repo.to_string()),
        ("digest", format!("sha256:{hex}")),
        ("tag", tag.clone().unwrap_or_default()),
        ("media_type", media_type.clone()),
    ]
    .into_iter()
    .map(|(k, v): (&str, String)| (k.to_string(), v))
    .collect();
    crate::prov_api::record_publish(st, "oci", h, &p.name, m);
    let mut resp = StatusCode::CREATED.into_response();
    location(resp.headers_mut(), format!("/v2/{repo}/manifests/sha256:{hex}"));
    digest_header(resp.headers_mut(), &hex);
    resp.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(0));
    if let Some(sub) = subject
        && let Ok(v) = HeaderValue::from_str(&format!("sha256:{sub}"))
    {
        resp.headers_mut().insert("oci-subject", v);
    }
    Ok(resp)
}

/// `GET /v2/<name>/referrers/<digest>[?artifactType=]`: an image index of the
/// manifests whose `subject` is `digest`.
async fn referrers(st: &Arc<AppState>, repo: &str, digest: &str, q: &HashMap<String, String>) -> OciResult<Response> {
    let hex = parse_digest(digest).ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"))?;
    let prefix = referrers_prefix(repo, hex);
    let r = st.oci_repo();
    let links = blocking(move || r.list_refs(&prefix)).await?;
    let filter = q.get("artifactType").filter(|s| !s.is_empty());
    let mut descs = Vec::new();
    for (n, v) in links {
        let Some(child_hex) = n.as_str().rsplit('/').next().map(str::to_string) else { continue };
        let r = st.oci_repo();
        let h = v.hash;
        let bytes = match blocking(move || r.get(&h, None)).await {
            Ok(b) => b,
            Err(e) => {
                warn!(name = %n, error = %e.message, "referrer manifest unreadable");
                continue;
            }
        };
        let Ok(m) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let media_type = m.get("mediaType").and_then(|x| x.as_str()).unwrap_or(MT_OCI_MANIFEST).to_string();
        let artifact_type = m
            .get("artifactType")
            .and_then(|x| x.as_str())
            .or_else(|| m.get("config").and_then(|c| c.get("mediaType")).and_then(|x| x.as_str()))
            .map(str::to_string);
        if let Some(f) = filter
            && artifact_type.as_deref() != Some(f.as_str())
        {
            continue;
        }
        let mut d = serde_json::json!({
            "mediaType": media_type,
            "digest": format!("sha256:{child_hex}"),
            "size": bytes.len(),
        });
        if let Some(at) = artifact_type {
            d["artifactType"] = serde_json::Value::String(at);
        }
        if let Some(a) = m.get("annotations") {
            d["annotations"] = a.clone();
        }
        descs.push(d);
    }
    let body = serde_json::json!({"schemaVersion": 2, "mediaType": MT_OCI_INDEX, "manifests": descs});
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(MT_OCI_INDEX));
    if filter.is_some() {
        resp.headers_mut().insert("oci-filters-applied", HeaderValue::from_static("artifactType"));
    }
    Ok(resp)
}

/// OCC-deletes `n` if it currently points at `expect` (any value if `None`).
/// Returns whether something was deleted.
async fn delete_name(repo: &Arc<dyn Repository>, n: Name, expect: Option<Hash>) -> OciResult<bool> {
    for _ in 0..5 {
        let r = repo.clone();
        let n2 = n.clone();
        let Some(cur) = blocking(move || r.resolve(&n2)).await? else { return Ok(false) };
        if expect.is_some_and(|h| h != cur.hash) {
            return Ok(false);
        }
        let r = repo.clone();
        let n2 = n.clone();
        match blocking(move || r.delete_ref(&n2, &[(n2.clone(), Observed::Version(cur.version))])).await {
            Ok(ReplaceOutcome::Ok { .. }) => return Ok(true),
            Ok(ReplaceOutcome::Conflict { .. }) => continue,
            Err(e) if e.status == StatusCode::FORBIDDEN => {
                return Err(oe(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "UNSUPPORTED",
                    format!("deletion disallowed by namespace rule: {}", e.message),
                ));
            }
            Err(e) => return Err(e),
        }
    }
    Err(oe(StatusCode::SERVICE_UNAVAILABLE, "TOOMANYREQUESTS", format!("contention on {n}")))
}

/// `DELETE /v2/<name>/manifests/<tag|digest>`. A tag deletes just the tag; a
/// digest deletes the manifest from the repo along with every tag and
/// referrer link pointing at it. Blobs are reclaimed later by GC.
async fn delete_manifest(
    st: &Arc<AppState>,
    p: &Principal,
    c: &Caller,
    repo: &str,
    reference: &str,
) -> OciResult<Response> {
    let unknown = || oe(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", format!("manifest {reference} not found in {repo}"));
    if !reference.starts_with("sha256:") {
        if !valid_tag(reference) || !delete_name(&st.oci_repo(), tag_name(repo, reference)?, None).await? {
            return Err(unknown());
        }
        audit_push(st, p, c, repo, "delete_tag", reference).await?;
        return Ok(StatusCode::ACCEPTED.into_response());
    }
    let hex =
        parse_digest(reference).ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"))?.to_string();
    let link = manifest_link(repo, &hex)?;
    let h = resolve(&st.oci_repo(), link.clone()).await?.ok_or_else(unknown)?;
    // Tags pointing at it.
    let prefix = format!("oci/{repo}/tags");
    let r = st.oci_repo();
    let tags = blocking(move || r.list_refs(&prefix)).await?;
    for (n, v) in tags {
        if v.hash == h {
            delete_name(&st.oci_repo(), n, Some(h)).await?;
        }
    }
    // Its referrer link, if it has a subject.
    let r = st.oci_repo();
    if let Ok(bytes) = blocking(move || r.get(&h, None)).await
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(sub) =
            v.get("subject").and_then(|s| s.get("digest")).and_then(|d| d.as_str()).and_then(parse_digest)
    {
        delete_name(&st.oci_repo(), referrer_link(repo, sub, &hex)?, Some(h)).await?;
    }
    delete_name(&st.oci_repo(), mediatype_link(repo, &hex)?, None).await?;
    delete_name(&st.oci_repo(), link, Some(h)).await?;
    audit_push(st, p, c, repo, "delete_manifest", &format!("sha256:{hex}")).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn get_manifest(st: &Arc<AppState>, repo: &str, reference: &str, head: bool) -> OciResult<Response> {
    let unknown = || oe(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", format!("manifest {reference} not found in {repo}"));
    let h = if reference.starts_with("sha256:") {
        let hex = parse_digest(reference).ok_or_else(|| oe(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "bad digest"))?;
        resolve(&st.oci_repo(), manifest_link(repo, hex)?).await?
    } else if valid_tag(reference) {
        resolve(&st.oci_repo(), tag_name(repo, reference)?).await?
    } else {
        None // an invalid tag cannot exist
    }
    .ok_or_else(unknown)?;
    let r = st.oci_repo();
    let bytes = blocking(move || r.get(&h, None)).await?;
    let hex = sha256_hex(&bytes);
    let media_type = match resolve(&st.oci_repo(), mediatype_link(repo, &hex)?).await? {
        Some(mh) => {
            let r = st.oci_repo();
            String::from_utf8(blocking(move || r.get(&mh, None)).await?).unwrap_or_else(|_| MT_OCI_MANIFEST.into())
        }
        None => {
            warn!(repo, hex, "manifest without media type record");
            MT_OCI_MANIFEST.to_string()
        }
    };
    let len = bytes.len();
    let mut resp = if head { Response::new(Body::empty()) } else { Response::new(Body::from(Bytes::from(bytes))) };
    let hm = resp.headers_mut();
    hm.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&media_type) {
        hm.insert(header::CONTENT_TYPE, v);
    }
    digest_header(hm, &hex);
    if let Ok(v) = HeaderValue::from_str(&format!("\"sha256:{hex}\"")) {
        hm.insert(header::ETAG, v);
    }
    Ok(resp)
}

async fn list_tags(st: &Arc<AppState>, repo: &str, q: &HashMap<String, String>) -> OciResult<Response> {
    let prefix = format!("oci/{repo}/tags");
    let r = st.oci_repo();
    let p2 = prefix.clone();
    let refs = blocking(move || r.list_refs(&p2)).await?;
    let mut tags: Vec<String> = refs
        .iter()
        .filter_map(|(n, _)| n.as_str().strip_prefix(&prefix)?.strip_prefix('/').map(str::to_string))
        .filter(|t| !t.contains('/'))
        .collect();
    tags.sort();
    if let Some(last) = q.get("last") {
        tags.retain(|t| t.as_str() > last.as_str());
    }
    let mut link = None;
    if let Some(n) = q.get("n").and_then(|n| n.parse::<usize>().ok())
        && tags.len() > n
    {
        tags.truncate(n);
        if let Some(l) = tags.last() {
            link = Some(format!("</v2/{repo}/tags/list?n={n}&last={l}>; rel=\"next\""));
        }
    }
    let mut resp = Json(serde_json::json!({"name": repo, "tags": tags})).into_response();
    if let Some(l) = link
        && let Ok(v) = HeaderValue::from_str(&l)
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// GC tracer
// ---------------------------------------------------------------------------

/// GC tracer for OCI content. A manifest keeps its config, layers and child
/// manifests alive, resolved through the digest index. Segment-map roots keep
/// their segments alive. Everything else is a leaf. False positives (a JSON
/// blob that looks like a manifest) only keep extra objects alive, which is safe.
pub struct OciTracer {
    digests: SegMap,
}

impl OciTracer {
    pub fn new(repo: Arc<dyn Repository>) -> OciTracer {
        let n = Name::new("oci/_digest").unwrap_or_else(|_| unreachable!("static name is valid"));
        OciTracer { digests: SegMap::new(repo, n, Arc::default()) }
    }

    /// Children of `h` if this tracer recognizes it.
    pub fn children_of(
        &self,
        h: &Hash,
        store: &dyn pico2peta_core::ObjectStore,
    ) -> pico2peta_core::Result<Option<Vec<Hash>>> {
        let size = store.size(h)?;
        let head = store.get(h, Some(0..size.min(16)))?;
        if let Some(c) = crate::segmap::map_children(&head, h, store)? {
            return Ok(Some(c));
        }
        let first = head.iter().find(|b| !b.is_ascii_whitespace());
        if first != Some(&b'{') || size > MAX_MANIFEST as u64 {
            return Ok(None);
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&store.get(h, None)?) else { return Ok(None) };
        let mut digests = Vec::new();
        let mut push = |d: &serde_json::Value| {
            if let Some(hex) = d.get("digest").and_then(|x| x.as_str()).and_then(parse_digest) {
                digests.push(hex.to_string());
            }
        };
        for field in ["config", "subject"] {
            if let Some(d) = v.get(field) {
                push(d);
            }
        }
        for field in ["layers", "manifests", "blobs"] {
            if let Some(arr) = v.get(field).and_then(|x| x.as_array()) {
                arr.iter().for_each(&mut push);
            }
        }
        if digests.is_empty() {
            return Ok(None);
        }
        // One batched lookup for every referenced digest.
        let keys: Vec<_> = digests.iter().filter_map(|hex| hex_key(hex).ok()).collect();
        let (_, root) = self.digests.root_blocking()?;
        Ok(Some(self.digests.get_many(&root, &keys)?.into_iter().flatten().map(|v| v.hash).collect()))
    }
}

impl pico2peta_repo::Tracer for OciTracer {
    fn children(&self, h: &Hash, store: &dyn pico2peta_core::ObjectStore) -> pico2peta_core::Result<Vec<Hash>> {
        Ok(self.children_of(h, store)?.unwrap_or_default())
    }
}

pub(crate) fn fuzz_parsers(data: &[u8]) {
    for mt in [MT_OCI_MANIFEST, MT_OCI_INDEX, MT_DOCKER_MANIFEST, MT_DOCKER_LIST, "application/x-other"] {
        let _ = manifest_refs(data, mt);
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = route(s);
        let _ = valid_repo(s);
        let _ = valid_tag(s);
        let _ = parse_digest(s);
        let _ = hex_key(s);
    }
}

mod hex {
    pub fn encode_lower(b: &[u8]) -> String {
        super::hex(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "library/ubuntu", "a.b_c__d---e/f0", "x9/y"] {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in ["", "A", "a/", "/a", "a//b", "_a", "a_", "a..b", "a___b", "a/../b", "oci/_digest"] {
            assert!(!valid_repo(bad), "{bad}");
        }
        assert!(valid_tag("latest") && valid_tag("_x") && valid_tag("v1.2.3-rc_1"));
        assert!(
            !valid_tag(".x") && !valid_tag("-x") && !valid_tag("") && !valid_tag(&"a".repeat(129)) && !valid_tag("a/b")
        );
        assert!(parse_digest(&format!("sha256:{}", "a".repeat(64))).is_some());
        assert!(parse_digest(&format!("sha256:{}", "A".repeat(64))).is_none());
        assert!(parse_digest(&format!("sha512:{}", "a".repeat(128))).is_none());
    }

    #[test]
    fn routes() {
        assert!(matches!(route(""), Some(Route::Base)));
        assert!(matches!(route("a/b/tags/list"), Some(Route::Tags(n)) if n == "a/b"));
        assert!(matches!(route("a/b/blobs/uploads/"), Some(Route::Upload(n, None)) if n == "a/b"));
        assert!(matches!(route("a/blobs/uploads/xyz"), Some(Route::Upload(n, Some(id))) if n == "a" && id == "xyz"));
        assert!(matches!(route("a/blobs/sha256:00"), Some(Route::Blob(n, d)) if n == "a" && d == "sha256:00"));
        assert!(matches!(route("blobs/blobs/sha256:00"), Some(Route::Blob(n, _)) if n == "blobs"));
        assert!(matches!(route("a/manifests/latest"), Some(Route::Manifest(n, r)) if n == "a" && r == "latest"));
        assert!(route("a/other").is_none());
    }

    #[test]
    fn manifest_validation() {
        let m = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
            "config":{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "layers":[{"digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]}"#;
        let (b, ms, sub) = manifest_refs(m, MT_OCI_MANIFEST).ok().unwrap();
        assert!(sub.is_none());
        assert_eq!(b.len(), 2);
        assert!(ms.is_empty());
        assert!(manifest_refs(m, MT_OCI_INDEX).is_err(), "mediaType mismatch");
        assert!(manifest_refs(b"{", MT_OCI_MANIFEST).is_err());
        assert!(manifest_refs(br#"{"layers":[]}"#, MT_OCI_MANIFEST).is_err(), "no config");
        assert!(manifest_refs(br#"{"anything":1}"#, "application/vnd.example+json").is_ok());
    }
}
