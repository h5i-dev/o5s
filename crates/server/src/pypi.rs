//! PyPI-compatible package indexes: `pip`, `uv` and `twine` work against them unchanged.
//!
//! - Simple API (PEP 503 HTML and PEP 691 JSON): `GET /pypi/<index>/simple/` and
//!   `/pypi/<index>/simple/<project>/`. Files are served at `/pypi/<index>/files/<project>/<file>`.
//! - Upload (the legacy API that twine and `uv publish` use): `POST /pypi/<index>/`, multipart
//!   with `:action=file_upload`. Files are create-once, as on PyPI. Re-uploading identical
//!   content succeeds; different content under the same filename is a 409.
//! - Upstream proxy-cache (`[pypi.upstream.<index>]`): project pages merge local files with
//!   the upstream's (local wins). The first download of an upstream file fetches it, checks
//!   it against the upstream's sha256, and stores it; later downloads, including while the
//!   upstream is down, come from the store.
//!
//! **Storage.** File `f` of project `p` in index `i` is the name `pypi/<i>/<p>/<f>` → a small
//! record object (`P2PPYPI1`, content hash, JSON metadata). The content is a core object
//! stored with namespace `pypi/<i>/<p>`'s encoding profile. The `pypi` front claims `pypi/`,
//! and its GC tracer follows record → content. Authorization is on `pypi/<i>`.

// Handlers short-circuit with a ready `Response` as the error value.
#![allow(clippy::result_large_err)]

use crate::api::{AppState, Caller};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::{Access, Upstream};
use crate::pkg::{Store, blocking, core_err, err};
use axum::Extension;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use pico2peta_core::{Error, Hash, Name, ObjectStore, Repository, Result};
use sha2::Digest as _;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RECORD_MAGIC: &[u8; 8] = b"P2PPYPI1";
const JSON_TYPE: &str = "application/vnd.pypi.simple.v1+json";

pub struct PypiState {
    /// The `pypi` front's handle (moves names under `pypi/`).
    repo: Arc<dyn Repository>,
    store: Store,
    upstreams: BTreeMap<String, Upstream>,
    http: reqwest::Client,
    /// (index, project) → when fetched, the upstream's files.
    pages: Mutex<HashMap<(String, String), CachedPage>>,
}

impl PypiState {
    pub fn new(
        repo: Arc<dyn Repository>,
        local: Option<pico2peta_repo::LocalRepo>,
        upstreams: BTreeMap<String, Upstream>,
    ) -> std::result::Result<PypiState, String> {
        let http = crate::pkg::http_client()?;
        let store = Store { repo: repo.clone(), local };
        Ok(PypiState { repo, store, upstreams, http, pages: Mutex::new(HashMap::new()) })
    }
}

/// When an upstream page was fetched, and its files.
type CachedPage = (Instant, Arc<Vec<UpFile>>);

/// A file on an upstream project page.
#[derive(Clone, Debug)]
struct UpFile {
    filename: String,
    url: String,
    sha256: Option<String>,
    requires_python: Option<String>,
    size: Option<u64>,
    yanked: serde_json::Value,
    /// PEP 714 `core-metadata` (or the older `data-dist-info-metadata`), if offered.
    core_metadata: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Meta {
    sha256: String,
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requires_python: Option<String>,
    upload_time: String,
    /// `upload` or `upstream`.
    source: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    yanked: serde_json::Value,
    /// sha256 of the wheel's core metadata (PEP 658), stored as `<file>.metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata_sha256: Option<String>,
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

/// GC: the content object a PyPI record references.
pub fn record_child(h: &Hash, store: &dyn ObjectStore) -> Result<Option<Hash>> {
    let size = store.size(h)?;
    if !(40..=1 << 20).contains(&size) {
        return Ok(None);
    }
    let head = store.get(h, Some(0..40))?;
    if !head.starts_with(RECORD_MAGIC) {
        return Ok(None);
    }
    Ok(Some(Hash(head[8..40].try_into().map_err(|_| Error::Corrupt("pypi record".into()))?)))
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// PEP 503 normalization.
pub fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

fn valid_project(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && p.as_bytes()[0].is_ascii_alphanumeric()
}

fn valid_index(i: &str) -> bool {
    !i.is_empty() && i.len() <= 64 && i.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

/// A distribution filename (wheel or sdist) belonging to normalized project `p`.
fn valid_filename(f: &str, p: &str) -> bool {
    let ok_chars = f.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+'));
    let ok_ext = [".whl", ".tar.gz", ".zip", ".tar.bz2"].iter().any(|e| f.ends_with(e));
    if !ok_chars || !ok_ext || f.len() > 255 || f.starts_with('.') {
        return false;
    }
    // Wheel: `<dist>-<ver>-...`, with `_` in place of `-` in dist; sdist: `<dist>-<ver>.tar.gz`.
    let dist =
        if f.ends_with(".whl") { f.split('-').next().unwrap_or("") } else { f.rsplit_once('-').map_or("", |x| x.0) };
    normalize(dist) == p
}

fn file_name(index: &str, project: &str, file: &str) -> Option<Name> {
    Name::new(format!("pypi/{index}/{project}/{file}")).ok()
}

fn authz_ns(index: &str) -> String {
    format!("pypi/{index}")
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn check(st: &AppState, p: &Principal, c: &Caller, index: &str, need: Access) -> std::result::Result<(), Response> {
    let ns = authz_ns(index);
    if st.authz.allows(p, &ns, need) {
        return Ok(());
    }
    st.metrics.denied.inc();
    st.audit.record_nowait(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "pypi",
        target: ns.clone(),
        outcome: "denied".into(),
        detail: None,
        remote: c.remote.clone(),
    });
    if p.name == crate::auth::ANONYMOUS {
        // pip and twine send credentials only after a Basic challenge.
        let mut r = err(StatusCode::UNAUTHORIZED, "authentication required");
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"pico2peta\""));
        return Err(r);
    }
    Err(err(
        StatusCode::FORBIDDEN,
        format!("{} may not {} {ns}", p.name, if need == Access::Read { "read" } else { "write" }),
    ))
}

/// `/pypi/{*rest}`.
pub async fn dispatch(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    let Some(py) = st.pypi.clone() else { return err(StatusCode::NOT_FOUND, "PyPI front disabled") };
    let method = req.method().clone();
    let headers = req.headers().clone();
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    let Some(index) = parts.first().copied().filter(|i| valid_index(i)) else {
        return err(StatusCode::NOT_FOUND, "invalid index name");
    };
    let head = method == Method::HEAD;
    match (method, &parts[1..]) {
        (Method::POST, []) => {
            if let Err(r) = check(&st, &p, &c, index, Access::Write) {
                return r;
            }
            let mp = match Multipart::from_request(req, &()).await {
                Ok(m) => m,
                Err(e) => return err(StatusCode::BAD_REQUEST, format!("expected a multipart upload: {e}")),
            };
            upload(&st, &py, &p, &c, index, mp).await
        }
        (Method::GET | Method::HEAD, ["simple"]) => {
            if let Err(r) = check(&st, &p, &c, index, Access::Read) {
                return r;
            }
            root_page(&py, index, &headers).await
        }
        (Method::GET | Method::HEAD, ["simple", project]) => {
            if let Err(r) = check(&st, &p, &c, index, Access::Read) {
                return r;
            }
            if !valid_project(project) {
                return err(StatusCode::NOT_FOUND, "invalid project name");
            }
            let norm = normalize(project);
            if norm != *project || !rest.ends_with('/') {
                let mut r = StatusCode::MOVED_PERMANENTLY.into_response();
                if let Ok(v) = HeaderValue::from_str(&format!("/pypi/{index}/simple/{norm}/")) {
                    r.headers_mut().insert(header::LOCATION, v);
                }
                return r;
            }
            project_page(&py, index, &norm, &headers).await
        }
        (Method::GET | Method::HEAD, ["files", project, file]) => {
            if let Err(r) = check(&st, &p, &c, index, Access::Read) {
                return r;
            }
            if let Some(wheel) = file.strip_suffix(".metadata")
                && wheel.ends_with(".whl")
                && valid_project(project)
                && normalize(project) == *project
                && valid_filename(wheel, project)
            {
                return metadata(&st, &py, index, project, wheel, &headers, head).await;
            }
            if !valid_project(project) || normalize(project) != *project || !valid_filename(file, project) {
                return err(StatusCode::NOT_FOUND, "no such file");
            }
            download(&st, &py, index, project, file, &headers, head).await
        }
        _ => err(StatusCode::NOT_FOUND, "unknown PyPI endpoint"),
    }
}

fn wants_json(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains(JSON_TYPE))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

async fn root_page(py: &Arc<PypiState>, index: &str, h: &HeaderMap) -> Response {
    let (py2, prefix) = (py.clone(), format!("pypi/{index}/"));
    let names = match blocking(move || py2.repo.list_refs(&prefix)).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut projects: Vec<String> =
        names.iter().filter_map(|(n, _)| n.as_str().split('/').nth(2).map(str::to_string)).collect();
    projects.dedup();
    if wants_json(h) {
        let ps: Vec<_> = projects.iter().map(|p| serde_json::json!({ "name": p })).collect();
        let mut r = axum::Json(serde_json::json!({ "meta": { "api-version": "1.0" }, "projects": ps })).into_response();
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON_TYPE));
        return r;
    }
    let mut body = String::from("<!DOCTYPE html>\n<html><body>\n");
    for p in &projects {
        body.push_str(&format!("<a href=\"{p}/\">{p}</a>\n"));
    }
    body.push_str("</body></html>\n");
    ([(header::CONTENT_TYPE, "text/html")], body).into_response()
}

/// One row of a project page.
struct Row {
    filename: String,
    sha256: Option<String>,
    requires_python: Option<String>,
    size: Option<u64>,
    yanked: serde_json::Value,
    upload_time: Option<String>,
    metadata: Option<serde_json::Value>,
}

async fn local_files(py: &Arc<PypiState>, index: &str, project: &str) -> std::result::Result<Vec<Row>, Response> {
    let (py2, prefix) = (py.clone(), format!("pypi/{index}/{project}"));
    blocking(move || {
        let mut out = Vec::new();
        for (n, v) in py2.repo.list_refs(&prefix)? {
            let Some(f) = n.as_str().rsplit('/').next() else { continue };
            let b = py2.repo.get(&v.hash, None)?;
            let Some((_, m)) = decode_record(&b) else { continue };
            out.push(Row {
                filename: f.to_string(),
                sha256: Some(m.sha256),
                requires_python: m.requires_python,
                size: Some(m.size),
                yanked: m.yanked,
                upload_time: Some(m.upload_time),
                metadata: m.metadata_sha256.map(|s| serde_json::json!({ "sha256": s })),
            });
        }
        Ok(out)
    })
    .await
}

async fn project_page(py: &Arc<PypiState>, index: &str, project: &str, h: &HeaderMap) -> Response {
    let mut rows = match local_files(py, index, project).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Some(up) = py.upstreams.get(index) {
        match upstream_files(py, index, up, project).await {
            Ok(files) => {
                for f in files.iter() {
                    if !rows.iter().any(|r| r.filename == f.filename) && valid_filename(&f.filename, project) {
                        rows.push(Row {
                            filename: f.filename.clone(),
                            sha256: f.sha256.clone(),
                            requires_python: f.requires_python.clone(),
                            size: f.size,
                            yanked: f.yanked.clone(),
                            upload_time: None,
                            metadata: f.core_metadata.clone(),
                        });
                    }
                }
            }
            // Offline: serve what is cached locally.
            Err(e) => tracing::warn!(index, project, error = %e, "upstream unavailable; serving cached files only"),
        }
    }
    if rows.is_empty() {
        return err(StatusCode::NOT_FOUND, format!("project {project} not found"));
    }
    rows.sort_by(|a, b| a.filename.cmp(&b.filename));
    let url = |f: &str| format!("/pypi/{index}/files/{project}/{f}");
    if wants_json(h) {
        let files: Vec<_> = rows
            .iter()
            .map(|r| {
                let mut e = serde_json::json!({
                    "filename": r.filename, "url": url(&r.filename),
                    "hashes": r.sha256.as_ref().map(|s| serde_json::json!({ "sha256": s })).unwrap_or(serde_json::json!({})),
                });
                if let Some(rp) = &r.requires_python {
                    e["requires-python"] = serde_json::json!(rp);
                }
                if let Some(s) = r.size {
                    e["size"] = serde_json::json!(s);
                }
                if !r.yanked.is_null() && r.yanked != serde_json::json!(false) {
                    e["yanked"] = r.yanked.clone();
                }
                if let Some(t) = &r.upload_time {
                    e["upload-time"] = serde_json::json!(t);
                }
                if let Some(m) = &r.metadata {
                    e["core-metadata"] = m.clone();
                    e["data-dist-info-metadata"] = m.clone();
                }
                e
            })
            .collect();
        let mut r =
            axum::Json(serde_json::json!({ "meta": { "api-version": "1.0" }, "name": project, "files": files }))
                .into_response();
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON_TYPE));
        return r;
    }
    let mut body = format!("<!DOCTYPE html>\n<html><head><title>Links for {project}</title></head><body>\n");
    for r in &rows {
        let frag = r.sha256.as_ref().map(|s| format!("#sha256={s}")).unwrap_or_default();
        let rp = r
            .requires_python
            .as_ref()
            .map(|s| format!(" data-requires-python=\"{}\"", html_escape(s)))
            .unwrap_or_default();
        let yk = match &r.yanked {
            serde_json::Value::String(s) => format!(" data-yanked=\"{}\"", html_escape(s)),
            serde_json::Value::Bool(true) => " data-yanked=\"\"".into(),
            _ => String::new(),
        };
        let md = match &r.metadata {
            Some(m) => {
                let v = m["sha256"].as_str().map_or("true".to_string(), |s| format!("sha256={s}"));
                format!(" data-core-metadata=\"{v}\" data-dist-info-metadata=\"{v}\"")
            }
            None => String::new(),
        };
        body.push_str(&format!("<a href=\"{}{frag}\"{rp}{yk}{md}>{}</a>\n", url(&r.filename), r.filename));
    }
    body.push_str("</body></html>\n");
    ([(header::CONTENT_TYPE, "text/html")], body).into_response()
}

/// The upstream's files for `project` (cached for the upstream's TTL).
async fn upstream_files(
    py: &PypiState,
    index: &str,
    up: &Upstream,
    project: &str,
) -> std::result::Result<Arc<Vec<UpFile>>, String> {
    let key = (index.to_string(), project.to_string());
    if let Some((t, v)) = py.pages.lock().map_err(|_| "poisoned")?.get(&key)
        && t.elapsed() < Duration::from_secs(up.ttl_secs)
    {
        return Ok(v.clone());
    }
    let page = format!("{}/{project}/", up.url.trim_end_matches('/'));
    let base = reqwest::Url::parse(&page).map_err(|e| e.to_string())?;
    let mut rq = py.http.get(base.clone()).header(header::ACCEPT, JSON_TYPE).timeout(Duration::from_secs(30));
    if let Some(t) = &up.token {
        rq = rq.bearer_auth(t);
    }
    let resp = rq.send().await.map_err(|e| e.to_string())?;
    let files = if resp.status() == StatusCode::NOT_FOUND {
        Vec::new()
    } else if !resp.status().is_success() {
        return Err(format!("upstream {page}: {}", resp.status()));
    } else {
        let v: serde_json::Value = resp.json().await.map_err(|e| format!("upstream {page}: not PEP 691 JSON: {e}"))?;
        let mut out = Vec::new();
        for f in v["files"].as_array().into_iter().flatten() {
            let (Some(name), Some(u)) = (f["filename"].as_str(), f["url"].as_str()) else { continue };
            let Ok(abs) = base.join(u) else { continue };
            out.push(UpFile {
                filename: name.to_string(),
                url: abs.to_string(),
                sha256: f["hashes"]["sha256"].as_str().map(|s| s.to_ascii_lowercase()),
                requires_python: f["requires-python"].as_str().map(str::to_string),
                size: f["size"].as_u64(),
                yanked: f.get("yanked").cloned().unwrap_or(serde_json::Value::Null),
                core_metadata: f
                    .get("core-metadata")
                    .or_else(|| f.get("data-dist-info-metadata"))
                    .filter(|v| !v.is_null() && **v != serde_json::json!(false))
                    .cloned(),
            });
        }
        out
    };
    let files = Arc::new(files);
    if let Ok(mut m) = py.pages.lock() {
        if m.len() > 10_000 {
            m.clear();
        }
        m.insert(key, (Instant::now(), files.clone()));
    }
    Ok(files)
}

/// Creates `name` → record, create-once (an identical file is fine; other bytes are not).
fn create_file(
    repo: &dyn Repository,
    name: &Name,
    content: Hash,
    meta: &Meta,
) -> Result<std::result::Result<bool, String>> {
    let sha = meta.sha256.clone();
    crate::pkg::create_once(repo, name, &encode_record(&content, meta), &|b| {
        decode_record(b).is_some_and(|(_, m)| m.sha256 == sha)
    })
    .map(|r| r.map_err(|_| "File already exists with different content".to_string()))
}

async fn upload(
    st: &Arc<AppState>,
    py: &Arc<PypiState>,
    p: &Principal,
    c: &Caller,
    index: &str,
    mut mp: Multipart,
) -> Response {
    if !st.audit.is_healthy() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable; refusing uploads");
    }
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut stored: Option<(String, Hash, String, u64)> = None;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("bad multipart body: {e}")),
        };
        let fname = field.name().unwrap_or_default().to_string();
        if fname == "content" {
            let Some(file) = field.file_name().map(str::to_string) else {
                return err(StatusCode::BAD_REQUEST, "content field without a filename");
            };
            // The project name may arrive after the file; validate the name fully later.
            let project = normalize(
                fields.get("name").map(String::as_str).unwrap_or_else(|| file.split('-').next().unwrap_or("")),
            );
            if !valid_filename(&file, &project) {
                return err(StatusCode::BAD_REQUEST, format!("invalid distribution filename {file:?}"));
            }
            let Ok(ns) = Name::new(format!("pypi/{index}/{project}")) else {
                return err(StatusCode::BAD_REQUEST, "invalid project");
            };
            let stream = field.map(|r| r.map_err(|e| e.to_string()));
            match py.store.put_stream(ns, Box::pin(stream), st.max_upload_bytes).await {
                Ok(x) => stored = Some((file, x.hash, x.sha256_hex(), x.size)),
                Err(r) => return r,
            }
        } else {
            match field.text().await {
                Ok(t) => {
                    fields.insert(fname, t);
                }
                Err(e) => return err(StatusCode::BAD_REQUEST, format!("bad field {fname}: {e}")),
            }
        }
    }
    if fields.get(":action").map(String::as_str) != Some("file_upload") {
        return err(StatusCode::BAD_REQUEST, "only :action=file_upload is supported");
    }
    let Some(name) = fields.get("name").filter(|n| valid_project(n)) else {
        return err(StatusCode::BAD_REQUEST, "missing or invalid project name");
    };
    let project = normalize(name);
    let Some((file, h, sha, size)) = stored else { return err(StatusCode::BAD_REQUEST, "missing content") };
    if !valid_filename(&file, &project) {
        return err(StatusCode::BAD_REQUEST, format!("{file} does not belong to project {project}"));
    }
    if let Some(want) = fields.get("sha256_digest").filter(|s| !s.is_empty())
        && want.to_ascii_lowercase() != sha
    {
        return err(StatusCode::BAD_REQUEST, format!("sha256 mismatch: received {sha}, form says {want}"));
    }
    let Some(n) = file_name(index, &project, &file) else { return err(StatusCode::BAD_REQUEST, "invalid file name") };
    let meta = Meta {
        sha256: sha.clone(),
        size,
        requires_python: fields.get("requires_python").filter(|s| !s.is_empty()).cloned(),
        upload_time: crate::audit::rfc3339_now(),
        source: "upload".into(),
        yanked: serde_json::Value::Null,
        metadata_sha256: None,
    };
    let mut meta = meta;
    if file.ends_with(".whl") {
        let (py2, mn) = (py.clone(), file_name(index, &project, &format!("{file}.metadata")));
        let md = blocking(move || {
            let Some(mn) = mn else { return Ok(None) };
            let Some(md) = crate::pkg::wheel_metadata(&*py2.repo, &h, size)? else { return Ok(None) };
            // Create-once like the file itself; the same bytes are fine.
            Ok(match crate::pkg::create_once(&*py2.repo, &mn, &md, &|b| b == md.as_slice())? {
                Ok(_) => Some(crate::git::hex(&sha2::Sha256::digest(&md))),
                Err(_) => None,
            })
        })
        .await;
        match md {
            Ok(m) => meta.metadata_sha256 = m,
            Err(r) => return r,
        }
    }
    let (py2, n2) = (py.clone(), n.clone());
    let res = blocking(move || create_file(&*py2.repo, &n2, h, &meta)).await;
    let (status, outcome) = match &res {
        Ok(Ok(true)) => (StatusCode::OK, "ok"),
        Ok(Ok(false)) => (StatusCode::OK, "exists"),
        Ok(Err(_)) => (StatusCode::CONFLICT, "conflict"),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "error"),
    };
    let _ = st
        .audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "pypi_upload",
            target: n.to_string(),
            outcome: outcome.into(),
            detail: Some(format!("sha256={sha} size={size}")),
            remote: c.remote.clone(),
        })
        .await;
    match res {
        Ok(Ok(_)) => (status, "OK").into_response(),
        Ok(Err(m)) => err(status, m),
        Err(r) => r,
    }
}

async fn download(
    st: &Arc<AppState>,
    py: &Arc<PypiState>,
    index: &str,
    project: &str,
    file: &str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let Some(n) = file_name(index, project, file) else { return err(StatusCode::NOT_FOUND, "no such file") };
    let (py2, n2) = (py.clone(), n.clone());
    let found = match blocking(move || {
        Ok(match py2.repo.resolve(&n2)? {
            Some(v) => decode_record(&py2.repo.get(&v.hash, None)?),
            None => None,
        })
    })
    .await
    {
        Ok(x) => x,
        Err(r) => return r,
    };
    let (content, meta) = match found {
        Some(x) => x,
        None => match fetch_upstream(st, py, index, project, file, &n).await {
            Ok(x) => x,
            Err(r) => return r,
        },
    };
    crate::hf::serve_object(st, content, headers, head, |hm| {
        if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", meta.sha256)) {
            hm.insert(header::ETAG, v);
        }
        hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    })
    .await
}

/// `<wheel>.metadata` (PEP 658): stored at upload, or fetched once from the upstream.
#[allow(clippy::too_many_arguments)]
async fn metadata(
    st: &Arc<AppState>,
    py: &Arc<PypiState>,
    index: &str,
    project: &str,
    wheel: &str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let Some(n) = file_name(index, project, &format!("{wheel}.metadata")) else {
        return err(StatusCode::NOT_FOUND, "no such file");
    };
    let (py2, n2) = (py.clone(), n.clone());
    let have = match blocking(move || Ok(py2.repo.resolve(&n2)?.map(|v| v.hash))).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let h = match have {
        Some(h) => h,
        None => {
            let Some(up) = py.upstreams.get(index) else { return err(StatusCode::NOT_FOUND, "no metadata") };
            let files = match upstream_files(py, index, up, project).await {
                Ok(f) => f,
                Err(e) => return err(StatusCode::BAD_GATEWAY, e),
            };
            let Some(f) = files.iter().find(|f| f.filename == wheel && f.core_metadata.is_some()).cloned() else {
                return err(StatusCode::NOT_FOUND, "no metadata");
            };
            st.metrics
                .upstream_fetches
                .get_or_create(&crate::metrics::FrontLabels { front: "pypi_metadata".into() })
                .inc();
            let murl = format!("{}.metadata", f.url);
            let mut rq = py.http.get(&murl);
            if let (Some(t), Ok(a), Ok(b)) = (&up.token, reqwest::Url::parse(&murl), reqwest::Url::parse(&up.url))
                && a.host_str() == b.host_str()
            {
                rq = rq.bearer_auth(t);
            }
            let body = match rq.send().await {
                Ok(r) if r.status().is_success() => match r.bytes().await {
                    Ok(b) if b.len() <= 16 << 20 => b,
                    _ => return err(StatusCode::BAD_GATEWAY, "upstream metadata unreadable"),
                },
                Ok(r) => return err(StatusCode::BAD_GATEWAY, format!("upstream metadata: {}", r.status())),
                Err(e) => return err(StatusCode::BAD_GATEWAY, format!("upstream metadata: {e}")),
            };
            if let Some(want) = f.core_metadata.as_ref().and_then(|m| m["sha256"].as_str())
                && !want.eq_ignore_ascii_case(&crate::git::hex(&sha2::Sha256::digest(&body)))
            {
                return err(StatusCode::BAD_GATEWAY, "upstream metadata does not match its sha256");
            }
            let (py2, n2) = (py.clone(), n.clone());
            match blocking(move || {
                let h = py2.repo.put(&body)?;
                // A concurrent fetch may have stored it first (the same upstream bytes).
                let _ = crate::pkg::create_once(&*py2.repo, &n2, &body, &|b| b == &body[..])?;
                Ok(h)
            })
            .await
            {
                Ok(h) => h,
                Err(r) => return r,
            }
        }
    };
    crate::hf::serve_object(st, h, headers, head, |hm| {
        hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    })
    .await
}

/// Fetches an upstream file into the store (sha256-checked) and records it.
async fn fetch_upstream(
    st: &Arc<AppState>,
    py: &Arc<PypiState>,
    index: &str,
    project: &str,
    file: &str,
    n: &Name,
) -> std::result::Result<(Hash, Meta), Response> {
    let Some(up) = py.upstreams.get(index) else { return Err(err(StatusCode::NOT_FOUND, "no such file")) };
    let files = upstream_files(py, index, up, project).await.map_err(|e| err(StatusCode::BAD_GATEWAY, e))?;
    let Some(f) = files.iter().find(|f| f.filename == file).cloned() else {
        return Err(err(StatusCode::NOT_FOUND, "no such file"));
    };
    let mut rq = py.http.get(&f.url);
    // Only send the upstream token to the upstream's own host (files may live on a CDN).
    if let (Some(t), Ok(a), Ok(b)) = (&up.token, reqwest::Url::parse(&f.url), reqwest::Url::parse(&up.url))
        && a.host_str() == b.host_str()
    {
        rq = rq.bearer_auth(t);
    }
    let resp = rq.send().await.map_err(|e| err(StatusCode::BAD_GATEWAY, format!("upstream: {e}")))?;
    if !resp.status().is_success() {
        return Err(err(StatusCode::BAD_GATEWAY, format!("upstream {}: {}", f.url, resp.status())));
    }
    st.metrics.upstream_fetches.get_or_create(&crate::metrics::FrontLabels { front: "pypi".into() }).inc();
    let ns = Name::new(format!("pypi/{index}/{project}")).map_err(core_err)?;
    let x = py.store.put_stream(ns, Box::pin(resp.bytes_stream()), st.max_upload_bytes).await?;
    let (h, sha, size) = (x.hash, x.sha256_hex(), x.size);
    if let Some(want) = &f.sha256
        && *want != sha
    {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            format!("upstream file {file} has sha256 {sha}, its index says {want}"),
        ));
    }
    let meta = Meta {
        sha256: sha,
        size,
        requires_python: f.requires_python.clone(),
        upload_time: crate::audit::rfc3339_now(),
        source: "upstream".into(),
        yanked: f.yanked.clone(),
        // Served from the upstream's `.metadata` on first use, then from the store.
        metadata_sha256: f.core_metadata.as_ref().and_then(|m| m["sha256"].as_str()).map(str::to_ascii_lowercase),
    };
    let (py2, n2, m2) = (py.clone(), n.clone(), meta.clone());
    match blocking(move || create_file(&*py2.repo, &n2, h, &m2)).await? {
        Ok(_) => Ok((h, meta)),
        Err(m) => Err(err(StatusCode::CONFLICT, m)),
    }
}

/// Fuzzing: name handling on arbitrary input.
pub fn fuzz_names(s: &str) -> bool {
    let n = normalize(s);
    valid_project(s) && valid_index(s) && valid_filename(s, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(normalize("Foo.Bar__baz-Q"), "foo-bar-baz-q");
        assert!(valid_filename("my_pkg-1.0-py3-none-any.whl", "my-pkg"));
        assert!(valid_filename("My.Pkg-1.0.tar.gz", "my-pkg"));
        assert!(valid_filename("torch-2.5.0+cpu-cp312-cp312-linux_x86_64.whl", "torch"));
        assert!(!valid_filename("other-1.0.tar.gz", "my-pkg"));
        assert!(!valid_filename("../x-1.0.tar.gz", "x"));
        assert!(!valid_filename("my_pkg-1.0.exe", "my-pkg"));
    }

    #[test]
    fn records() {
        let m = Meta {
            sha256: "ab".into(),
            size: 3,
            requires_python: Some(">=3.8".into()),
            upload_time: "t".into(),
            source: "upload".into(),
            yanked: serde_json::Value::Null,
            metadata_sha256: Some("cd".into()),
        };
        let h = Hash([9; 32]);
        let (h2, m2) = decode_record(&encode_record(&h, &m)).unwrap();
        assert_eq!(h2, h);
        assert_eq!(m2.requires_python.as_deref(), Some(">=3.8"));
    }
}
