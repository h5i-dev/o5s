//! Router, middleware and handlers of the v1 HTTP API.
//!
//! The repository API is synchronous. Handlers call it on the blocking pool
//! and never hold a blocking thread while waiting on a slow client:
//! * Uploads: an async task forwards body frames into a bounded channel
//!   (8 frames), and `put_stream` reads from it on a blocking thread. When the
//!   channel is full we stop polling the socket, which is TCP backpressure.
//!   An idle client times out after `IDLE_TIMEOUT`.
//! * Downloads: each 256 KiB chunk is read by a short `spawn_blocking` call
//!   and sent through a bounded channel (4 chunks) with a send timeout.
//!
//! So memory per transfer is bounded (about 2 MiB), whatever the object size.

use crate::audit::{AuditLog, Event};
use crate::auth::{Authenticator, Authorizer, Principal};
use crate::config::Access;
use crate::error::{ApiError, ApiResult};
use crate::metrics::{HttpLabels, Metrics, OutcomeLabels, RouteLabels};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Router};
use futures_util::StreamExt;
use pico2peta_client::wire::{self, codes};
use pico2peta_core::{Hash, Name, Repository, validate_path};
use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};

/// A stalled client is disconnected after this long without progress.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_CHUNK: usize = 256 * 1024;

/// Shared server state.
pub struct AppState {
    pub repo: Arc<dyn Repository>,
    pub authn: Arc<dyn Authenticator>,
    pub authz: Authorizer,
    /// The token authenticator, if that is what `authn` is (for SIGHUP reload).
    pub tokens: Option<Arc<crate::auth::TokenAuthenticator>>,
    pub audit: AuditLog,
    pub log_reads: bool,
    pub metrics: Arc<Metrics>,
    pub max_upload_bytes: u64,
    pub shutting_down: Arc<AtomicBool>,
    pub limiter: Arc<Semaphore>,
    pub trust_forwarded_for: bool,
    /// Per-principal token buckets (`None` = unlimited).
    pub limits: Option<RateLimiter>,
    /// `/metrics` is served on a separate listener instead of the main one.
    pub metrics_separate: bool,
    /// The concrete repository, for admin operations outside the trait (GC).
    pub local: Option<pico2peta_repo::LocalRepo>,
    pub gc: crate::config::GcSection,
    pub gc_running: Arc<AtomicBool>,
    pub oci: Option<Arc<crate::oci::OciState>>,
    pub git: Option<Arc<crate::git::GitState>>,
}

type St = State<Arc<AppState>>;

/// Per-request metadata for audit records.
#[derive(Clone, Debug)]
pub struct Caller {
    pub request_id: String,
    /// Client address: the TCP peer, or the first `X-Forwarded-For` hop when
    /// `server.trust_forwarded_for` is set (only behind a trusted proxy).
    pub remote: Option<String>,
}

fn client_addr(req: &Request, trust_forwarded_for: bool) -> Option<String> {
    if trust_forwarded_for
        && let Some(f) = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = f.split(',').next().map(str::trim).filter(|s| !s.is_empty() && s.len() <= 64)
    {
        return Some(first.to_string());
    }
    req.extensions().get::<axum::extract::ConnectInfo<PeerAddr>>().map(|ci| ci.0.0.to_string())
}

/// The TCP peer of a connection, for any listener (plain or TLS).
#[derive(Clone, Copy, Debug)]
pub struct PeerAddr(pub std::net::SocketAddr);

impl<'a> axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, tokio::net::TcpListener>> for PeerAddr {
    fn connect_info(s: axum::serve::IncomingStream<'a, tokio::net::TcpListener>) -> Self {
        PeerAddr(*s.remote_addr())
    }
}

impl<'a> axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, crate::tls::TlsListener>> for PeerAddr {
    fn connect_info(s: axum::serve::IncomingStream<'a, crate::tls::TlsListener>) -> Self {
        PeerAddr(*s.remote_addr())
    }
}

/// The request id assigned by the request-id layer.
fn request_id(h: &HeaderMap) -> String {
    h.get(wire::REQUEST_ID_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("-").to_string()
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/v1/objects", put(put_object))
        .route("/v1/objects/{hash}", get(get_object).head(head_object))
        .route("/v1/flush", post(flush))
        .route("/v1/refs", get(list_refs))
        .route("/v1/refs/{*name}", get(resolve).delete(delete_ref))
        .route("/v1/replace", post(replace))
        .route("/v1/replace_many", post(replace_many))
        .route("/v1/update_many", post(update_many))
        .route("/v1/namespaces", get(list_rules))
        .route("/v1/namespaces/{*prefix}", put(set_rule))
        .route("/v1/whoami", get(whoami))
        .route("/v1/admin/gc", post(gc))
        .route("/v2/", get(crate::oci::base))
        .route("/v2", get(crate::oci::base))
        .route("/v2/{*rest}", axum::routing::any(crate::oci::dispatch))
        .route("/git/{*rest}", axum::routing::any(crate::git::dispatch))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(middleware::from_fn_with_state(state.clone(), shed_load));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_main))
        .merge(api)
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Records request count, latency and in-flight gauge.
async fn observe(State(st): St, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map_or("unmatched", |m| m.as_str()).to_string();
    let method = req.method().to_string();
    st.metrics.in_flight.inc();
    let t = Instant::now();
    let resp = next.run(req).await;
    st.metrics.in_flight.dec();
    st.metrics.http_duration.get_or_create(&RouteLabels { route: route.clone() }).observe(t.elapsed().as_secs_f64());
    st.metrics.http_requests.get_or_create(&HttpLabels { method, route, status: resp.status().as_u16() }).inc();
    resp
}

/// Rejects requests beyond `max_concurrent_requests` with 503 instead of queueing.
/// The permit lives until the response body has been fully sent.
async fn shed_load(State(st): St, req: Request, next: Next) -> Response {
    if st.shutting_down.load(Ordering::Acquire) {
        return ApiError::unavailable(codes::SHUTTING_DOWN, "server is shutting down").into_response();
    }
    let Ok(permit) = st.limiter.clone().try_acquire_owned() else {
        return ApiError::unavailable(codes::OVERLOADED, "too many concurrent requests").into_response();
    };
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    let body = Body::from_stream(body.into_data_stream().map(move |c| {
        let _held = &permit;
        c
    }));
    Response::from_parts(parts, body)
}

/// Authenticates the caller and stores the [`Principal`] in request extensions.
async fn authenticate(State(st): St, mut req: Request, next: Next) -> Response {
    let caller_remote = client_addr(&req, st.trust_forwarded_for);
    let rid = request_id(req.headers());
    req.extensions_mut().insert(Caller { request_id: rid, remote: caller_remote.clone() });
    match st.authn.authenticate(req.headers()).await {
        Ok(p) => {
            if let Some(l) = &st.limits {
                let key = if p.authenticated {
                    format!("p:{}", p.name)
                } else {
                    // Anonymous: per client IP (port stripped).
                    let ip = caller_remote.as_deref().map(|a| a.rsplit_once(':').map_or(a, |x| x.0)).unwrap_or("-");
                    format!("a:{ip}")
                };
                if let Err(wait) = l.check(&key) {
                    st.metrics.rate_limited.inc();
                    let mut r = ApiError::new(StatusCode::TOO_MANY_REQUESTS, codes::OVERLOADED, "rate limit exceeded")
                        .into_response();
                    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait.as_secs().max(1)));
                    return r;
                }
            }
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(e) => {
            let reason = match e {
                crate::auth::AuthError::Missing => "missing",
                crate::auth::AuthError::Invalid => "invalid",
                crate::auth::AuthError::Malformed => "malformed",
                crate::auth::AuthError::Unavailable(_) => "unavailable",
            };
            st.metrics.auth_failures.get_or_create(&OutcomeLabels { outcome: reason.into() }).inc();
            if matches!(e, crate::auth::AuthError::Invalid | crate::auth::AuthError::Malformed) {
                st.audit.record_nowait(Event {
                    request_id: request_id(req.headers()),
                    principal: "-".into(),
                    action: "auth",
                    target: req.uri().path().to_string(),
                    outcome: "denied".into(),
                    detail: Some(reason.into()),
                    remote: caller_remote.clone(),
                });
            }
            if let crate::auth::AuthError::Unavailable(m) = e {
                return ApiError::unavailable(codes::INTERNAL, m).into_response();
            }
            if req.uri().path().starts_with("/git/") {
                let mut r = (StatusCode::UNAUTHORIZED, e.to_string()).into_response();
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"pico2peta\""));
                return r;
            }
            if req.uri().path().starts_with("/v2") {
                let body = serde_json::json!({"errors": [{"code": "UNAUTHORIZED", "message": e.to_string()}]});
                let mut r = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"pico2peta\""));
                r.headers_mut().insert("docker-distribution-api-version", HeaderValue::from_static("registry/2.0"));
                return r;
            }
            ApiError::unauthenticated(e.to_string()).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl AppState {
    /// Fails with 403 (and audits the denial) unless `cond`.
    fn require(&self, cond: bool, p: &Principal, c: &Caller, action: &'static str, target: &str) -> ApiResult<()> {
        if cond {
            return Ok(());
        }
        self.metrics.denied.inc();
        self.audit.record_nowait(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action,
            target: target.to_string(),
            outcome: "denied".into(),
            detail: None,
            remote: c.remote.clone(),
        });
        Err(ApiError::denied(format!("{} may not {action} {target}", p.name)))
    }

    /// Mutations are refused once the audit log has failed.
    fn require_audit_healthy(&self) -> ApiResult<()> {
        if self.audit.is_healthy() {
            Ok(())
        } else {
            Err(ApiError::unavailable(codes::INTERNAL, "audit log unavailable; refusing mutations"))
        }
    }

    /// Writes a durable audit record for a completed mutation.
    async fn audit_mutation(&self, ev: Event) -> ApiResult<()> {
        self.audit.record(ev).await.map_err(|e| {
            self.metrics.audit_failures.inc();
            ApiError::internal(format!("operation applied but audit write failed: {e}"))
        })
    }
}

/// Token buckets keyed by principal (bounded table).
pub struct RateLimiter {
    cfg: crate::config::RateLimit,
    buckets: std::sync::Mutex<HashMap<String, (f64, Instant)>>,
}

const MAX_BUCKETS: usize = 100_000;

impl RateLimiter {
    pub fn new(cfg: crate::config::RateLimit) -> Self {
        RateLimiter { cfg, buckets: std::sync::Mutex::new(HashMap::new()) }
    }

    /// Takes one token for `key`, or returns how long until one is available.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        let Ok(mut m) = self.buckets.lock() else { return Ok(()) };
        let now = Instant::now();
        if m.len() >= MAX_BUCKETS && !m.contains_key(key) {
            // Drop buckets that are full again (idle callers); they lose nothing.
            let full = self.cfg.burst;
            let rate = self.cfg.per_sec;
            m.retain(|_, (t, at)| (*t + now.duration_since(*at).as_secs_f64() * rate) < full);
            if m.len() >= MAX_BUCKETS {
                return Err(Duration::from_secs(1));
            }
        }
        let (tokens, at) = m.entry(key.to_string()).or_insert((self.cfg.burst, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * self.cfg.per_sec).min(self.cfg.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / self.cfg.per_sec))
        }
    }
}

/// Namespaces owned by protocol adapters (OCI, Git) and the provenance log.
pub fn is_reserved(name: &str) -> bool {
    ["oci", "git", "_prov"].iter().any(|p| pico2peta_core::is_under(name, p))
}

/// Runs a repository call on the blocking pool.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> pico2peta_core::Result<T> + Send + 'static) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(e) => Err(ApiError::internal(format!("worker panicked: {e}"))),
    }
}

fn parse_hash(s: &str) -> ApiResult<Hash> {
    s.parse::<Hash>().map_err(|e| ApiError::bad_request(e.to_string()))
}

fn parse_name(s: &str) -> ApiResult<Name> {
    Name::new(s).map_err(ApiError::from)
}

/// The `Range` header cannot be satisfied (→ 416).
#[derive(Debug, PartialEq, Eq)]
pub struct Unsatisfiable;

/// Parses a single `bytes=` range against `size`. `Ok(None)` = whole object
/// (no header, or a form we ignore such as multiple ranges).
pub fn parse_range(h: Option<&HeaderValue>, size: u64) -> Result<Option<Range<u64>>, Unsatisfiable> {
    let Some(v) = h.and_then(|v| v.to_str().ok()) else { return Ok(None) };
    let Some(spec) = v.trim().strip_prefix("bytes=") else { return Ok(None) };
    if spec.contains(',') {
        return Ok(None); // multi-range: RFC 9110 allows ignoring Range
    }
    let (a, b) = spec.split_once('-').ok_or(Unsatisfiable)?;
    let (a, b) = (a.trim(), b.trim());
    let r = if a.is_empty() {
        // suffix: last n bytes
        let n: u64 = b.parse().map_err(|_| Unsatisfiable)?;
        if n == 0 {
            return Err(Unsatisfiable);
        }
        size.saturating_sub(n)..size
    } else {
        let start: u64 = a.parse().map_err(|_| Unsatisfiable)?;
        let end =
            if b.is_empty() { size } else { b.parse::<u64>().map_err(|_| Unsatisfiable)?.saturating_add(1).min(size) };
        if start >= size || end <= start {
            return Err(Unsatisfiable);
        }
        start..end
    };
    if r.is_empty() { Err(Unsatisfiable) } else { Ok(Some(r)) }
}

/// Sync `Read` over a channel of body frames (used on the blocking pool).
struct ChannelReader {
    rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
    cur: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.cur = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.cur.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        let _ = self.cur.split_to(n);
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PutQuery {
    /// Store as an XOR delta against this object (API v1.7, model weights).
    #[serde(default)]
    delta_base: Option<String>,
}

async fn put_object(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<PutQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    let delta_base = match &q.delta_base {
        Some(b) => {
            st.require(st.authz.allows_somewhere(&p, Access::Read), &p, &c, "get", "objects")?;
            Some(parse_hash(b)?)
        }
        None => None,
    };
    st.require(st.authz.allows_somewhere(&p, Access::Write), &p, &c, "put", "objects")?;
    st.require_audit_healthy()?;
    let max = st.max_upload_bytes;
    if let Some(cl) =
        headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
        && cl > max
    {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, codes::TOO_LARGE, format!("limit is {max} bytes")));
    }
    let expect = match headers.get(wire::EXPECT_HASH_HEADER) {
        Some(v) => Some(parse_hash(v.to_str().map_err(|_| ApiError::bad_request("bad expect-hash header"))?)?),
        None => None,
    };

    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let received = Arc::new(AtomicU64::new(0));
    let too_large = Arc::new(AtomicBool::new(false));
    let (rcv, tl, metrics) = (received.clone(), too_large.clone(), st.metrics.clone());
    let pump = tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        loop {
            let next = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
                Err(_) => Some(Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "upload idle timeout"))),
                Ok(None) => None,
                Ok(Some(Ok(b))) => {
                    let total = rcv.fetch_add(b.len() as u64, Ordering::Relaxed) + b.len() as u64;
                    metrics.bytes_in.inc_by(b.len() as u64);
                    if total > max {
                        tl.store(true, Ordering::Release);
                        Some(Err(std::io::Error::other("upload exceeds max_upload_bytes")))
                    } else {
                        Some(Ok(b))
                    }
                }
                Ok(Some(Err(e))) => Some(Err(std::io::Error::other(format!("upload body: {e}")))),
            };
            match next {
                None => return,
                Some(item) => {
                    let stop = item.is_err();
                    if tx.send(item).await.is_err() || stop {
                        return;
                    }
                }
            }
        }
    });
    let repo = st.repo.clone();
    let res = blocking(move || {
        let mut r = ChannelReader { rx, cur: Bytes::new() };
        match delta_base {
            Some(b) => repo.put_delta(&b, &mut r),
            None => repo.put_stream(&mut r),
        }
    })
    .await;
    pump.abort();
    let size = received.load(Ordering::Relaxed);
    let hash = match res {
        Ok(h) => h,
        Err(_) if too_large.load(Ordering::Acquire) => {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                codes::TOO_LARGE,
                format!("limit is {max} bytes"),
            ));
        }
        Err(e) => return Err(e),
    };
    if let Some(want) = expect
        && want != hash
    {
        // The stored object is unreferenced; GC reclaims it.
        return Err(
            ApiError::bad_request(format!("body hashes to {hash}, expected {want}")).with_code(codes::HASH_MISMATCH)
        );
    }
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "put",
        target: hash.to_hex(),
        outcome: "ok".into(),
        detail: Some(match &q.delta_base {
            Some(b) => format!("size={size} delta_base={b}"),
            None => format!("size={size}"),
        }),
        remote: c.remote.clone(),
    })
    .await?;
    Ok((StatusCode::OK, Json(wire::PutResponse { hash: hash.to_hex(), size })).into_response())
}

impl ApiError {
    fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }
}

fn object_headers(h: &mut HeaderMap, hash: &Hash) {
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", hash.to_hex())) {
        h.insert(header::ETAG, v);
    }
    // Content-addressed: never changes. `private` because access is authorized.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
}

async fn head_object(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(hash): Path<String>,
) -> ApiResult<Response> {
    st.require(st.authz.allows_somewhere(&p, Access::Read), &p, &c, "get", "objects")?;
    let hash = parse_hash(&hash)?;
    let repo = st.repo.clone();
    let size = blocking(move || repo.size(&hash)).await?;
    let mut r = StatusCode::OK.into_response();
    object_headers(r.headers_mut(), &hash);
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    Ok(r)
}

async fn get_object(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(hash): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    st.require(st.authz.allows_somewhere(&p, Access::Read), &p, &c, "get", "objects")?;
    let hash = parse_hash(&hash)?;
    let repo = st.repo.clone();
    let size = blocking(move || repo.size(&hash)).await?;
    let range = match parse_range(headers.get(header::RANGE), size) {
        Ok(r) => r,
        Err(Unsatisfiable) => {
            let mut r = ApiError::new(StatusCode::RANGE_NOT_SATISFIABLE, "invalid_range", "unsatisfiable range")
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return Ok(r);
        }
    };
    let repo = st.repo.clone();
    let rr = range.clone();
    let reader = blocking(move || repo.open(&hash, rr)).await?;
    if st.log_reads {
        st.audit.record_nowait(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "get",
            target: hash.to_hex(),
            outcome: "ok".into(),
            detail: range.as_ref().map(|r| format!("range={}-{}", r.start, r.end)),
            remote: c.remote.clone(),
        });
    }
    let len = range.as_ref().map_or(size, |r| r.end - r.start);
    let body = stream_reader(reader, len, st.metrics.clone());
    let mut resp = Response::new(body);
    object_headers(resp.headers_mut(), &hash);
    resp.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Some(r) = range {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{size}", r.start, r.end - 1)) {
            resp.headers_mut().insert(header::CONTENT_RANGE, v);
        }
    }
    Ok(resp)
}

/// Streams `reader` (exactly `len` bytes) as a response body with bounded buffering.
pub(crate) fn stream_reader(reader: Box<dyn Read + Send>, len: u64, metrics: Arc<Metrics>) -> Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let mut reader = Some(reader);
        let mut sent = 0u64;
        while sent < len {
            let Some(mut r) = reader.take() else { return };
            let want = DOWNLOAD_CHUNK.min((len - sent) as usize);
            let res = tokio::task::spawn_blocking(move || {
                let mut buf = vec![0u8; want];
                let mut filled = 0;
                while filled < want {
                    match r.read(&mut buf[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return (r, Err(e)),
                    }
                }
                buf.truncate(filled);
                (r, Ok(buf))
            })
            .await;
            let (r, chunk) = match res {
                Ok(x) => x,
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(format!("reader task: {e}")))).await;
                    return;
                }
            };
            reader = Some(r);
            let chunk = match chunk {
                Ok(c) if c.is_empty() => {
                    // Fewer bytes than promised: abort the body so the client sees truncation.
                    tracing::error!(sent, len, "object stream ended early");
                    let _ = tx.send(Err(std::io::Error::other("object stream ended early"))).await;
                    return;
                }
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "object stream failed");
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            sent += chunk.len() as u64;
            metrics.bytes_out.inc_by(chunk.len() as u64);
            match tokio::time::timeout(IDLE_TIMEOUT, tx.send(Ok(Bytes::from(chunk)))).await {
                Ok(Ok(())) => {}
                _ => return, // client gone or stalled
            }
        }
    });
    Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|i| (i, rx)) }))
}

async fn flush(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
) -> ApiResult<StatusCode> {
    st.require(st.authz.allows_somewhere(&p, Access::Write), &p, &c, "flush", "objects")?;
    let repo = st.repo.clone();
    blocking(move || repo.flush()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

async fn resolve(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(name): Path<String>,
) -> ApiResult<Json<wire::RefJson>> {
    let name = parse_name(&name)?;
    st.require(st.authz.allows(&p, name.as_str(), Access::Read), &p, &c, "resolve", name.as_str())?;
    let repo = st.repo.clone();
    let n = name.clone();
    match blocking(move || repo.resolve(&n)).await? {
        Some(v) => Ok(Json(wire::RefJson::from_core(&name, &v))),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, codes::REF_NOT_FOUND, format!("no ref {name}"))),
    }
}

#[derive(serde::Deserialize)]
struct ListQuery {
    #[serde(default)]
    prefix: String,
}

async fn list_refs(
    State(st): St,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<wire::RefList>> {
    if !q.prefix.is_empty() {
        validate_path(&q.prefix).map_err(ApiError::from)?;
    }
    let repo = st.repo.clone();
    let prefix = q.prefix.clone();
    let all = blocking(move || repo.list_refs(&prefix)).await?;
    let refs = all
        .iter()
        .filter(|(n, _)| st.authz.allows(&p, n.as_str(), Access::Read))
        .map(|(n, v)| wire::RefJson::from_core(n, v))
        .collect();
    Ok(Json(wire::RefList { refs }))
}

async fn replace(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    body: Bytes,
) -> ApiResult<Response> {
    let req: wire::ReplaceRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("bad replace request: {e}")))?;
    let (name, read_set, new) = req.to_core().map_err(ApiError::from)?;
    st.require(st.authz.allows(&p, name.as_str(), Access::Write), &p, &c, "replace", name.as_str())?;
    // Adapter-managed namespaces keep invariants (digest ↔ content) that raw
    // replaces could break; only global admins may touch them through /v1.
    if is_reserved(name.as_str()) {
        st.require(st.authz.allows(&p, "", Access::Admin), &p, &c, "replace_reserved", name.as_str())?;
    }
    // Conflict responses reveal versions of read-set names, so require read on them.
    for (n, _) in &read_set {
        st.require(st.authz.allows(&p, n.as_str(), Access::Read), &p, &c, "replace", n.as_str())?;
    }
    st.require_audit_healthy()?;
    let repo = st.repo.clone();
    let (n, rs) = (name.clone(), read_set.clone());
    let res = blocking(move || repo.replace(&n, &rs, new)).await;
    let (outcome, detail) = match &res {
        Ok(pico2peta_core::ReplaceOutcome::Ok { version }) => ("ok", format!("new={new} version={version}")),
        Ok(pico2peta_core::ReplaceOutcome::Conflict { stale }) => {
            ("conflict", format!("new={new} stale={}", stale.len()))
        }
        Err(e) => ("rejected", format!("new={new} code={}", e.code)),
    };
    st.metrics.replace.get_or_create(&OutcomeLabels { outcome: outcome.into() }).inc();
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "replace",
        target: name.to_string(),
        outcome: outcome.into(),
        detail: Some(detail),
        remote: c.remote.clone(),
    })
    .await?;
    let out = res?;
    let status =
        if matches!(out, pico2peta_core::ReplaceOutcome::Ok { .. }) { StatusCode::OK } else { StatusCode::CONFLICT };
    Ok((status, Json(wire::ReplaceResponse::from_core(&out))).into_response())
}

async fn delete_ref(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let name = parse_name(&name)?;
    let req: wire::DeleteRefRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("bad delete request: {e}")))?;
    let read_set =
        req.read_set.iter().map(wire::ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>().map_err(ApiError::from)?;
    st.require(st.authz.allows(&p, name.as_str(), Access::Write), &p, &c, "delete_ref", name.as_str())?;
    if is_reserved(name.as_str()) {
        st.require(st.authz.allows(&p, "", Access::Admin), &p, &c, "delete_reserved", name.as_str())?;
    }
    for (n, _) in &read_set {
        st.require(st.authz.allows(&p, n.as_str(), Access::Read), &p, &c, "delete_ref", n.as_str())?;
    }
    st.require_audit_healthy()?;
    let repo = st.repo.clone();
    let (n, rs) = (name.clone(), read_set.clone());
    let res = blocking(move || repo.delete_ref(&n, &rs)).await;
    let outcome = match &res {
        Ok(pico2peta_core::ReplaceOutcome::Ok { .. }) => "ok",
        Ok(pico2peta_core::ReplaceOutcome::Conflict { .. }) => "conflict",
        Err(_) => "rejected",
    };
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "delete_ref",
        target: name.to_string(),
        outcome: outcome.into(),
        detail: None,
        remote: c.remote.clone(),
    })
    .await?;
    let out = res?;
    let status =
        if matches!(out, pico2peta_core::ReplaceOutcome::Ok { .. }) { StatusCode::OK } else { StatusCode::CONFLICT };
    Ok((status, Json(wire::ReplaceResponse::from_core(&out))).into_response())
}

async fn replace_many(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    body: Bytes,
) -> ApiResult<Response> {
    let req: wire::ReplaceManyRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("bad replace_many request: {e}")))?;
    let (updates, read_set) = req.to_core().map_err(ApiError::from)?;
    let updates = updates.into_iter().map(|(n, h)| (n, Some(h))).collect();
    multi_update(&st, &p, &c, updates, read_set, false).await
}

async fn update_many(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    body: Bytes,
) -> ApiResult<Response> {
    let req: wire::UpdateManyRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("bad update_many request: {e}")))?;
    let (updates, read_set) = req.to_core().map_err(ApiError::from)?;
    multi_update(&st, &p, &c, updates, read_set, true).await
}

/// Shared body of `replace_many` / `update_many` (None = delete, only when `allow_delete`).
async fn multi_update(
    st: &AppState,
    p: &Principal,
    c: &Caller,
    updates: Vec<(Name, Option<Hash>)>,
    read_set: Vec<(Name, pico2peta_core::Observed)>,
    allow_delete: bool,
) -> ApiResult<Response> {
    if updates.is_empty() {
        return Err(ApiError::bad_request("at least one update is required"));
    }
    for (n, _) in &updates {
        st.require(st.authz.allows(p, n.as_str(), Access::Write), p, c, "replace", n.as_str())?;
        if is_reserved(n.as_str()) {
            st.require(st.authz.allows(p, "", Access::Admin), p, c, "replace_reserved", n.as_str())?;
        }
    }
    for (n, _) in &read_set {
        st.require(st.authz.allows(p, n.as_str(), Access::Read), p, c, "replace", n.as_str())?;
    }
    st.require_audit_healthy()?;
    let repo = st.repo.clone();
    let (u, rs) = (updates.clone(), read_set.clone());
    let res = blocking(move || {
        if allow_delete {
            repo.update_many(&u, &rs)
        } else {
            let sets: Vec<(Name, Hash)> = u.into_iter().filter_map(|(n, h)| Some((n, h?))).collect();
            repo.replace_many(&sets, &rs)
        }
    })
    .await;
    let outcome = match &res {
        Ok(pico2peta_core::ReplaceManyOutcome::Ok { .. }) => "ok",
        Ok(pico2peta_core::ReplaceManyOutcome::Conflict { .. }) => "conflict",
        Err(_) => "rejected",
    };
    st.metrics.replace.get_or_create(&OutcomeLabels { outcome: outcome.into() }).inc();
    let targets: Vec<String> = updates
        .iter()
        .map(|(n, h)| match h {
            Some(h) => format!("{n}={h}"),
            None => format!("{n}=<delete>"),
        })
        .collect();
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: if allow_delete { "update_many" } else { "replace_many" },
        target: updates.first().map(|(n, _)| n.to_string()).unwrap_or_default(),
        outcome: outcome.into(),
        detail: Some(targets.join(" ")),
        remote: c.remote.clone(),
    })
    .await?;
    let out = res?;
    let status = if matches!(out, pico2peta_core::ReplaceManyOutcome::Ok { .. }) {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    Ok((status, Json(wire::ReplaceManyResponse::from_core(&out))).into_response())
}

// ---------------------------------------------------------------------------
// Namespaces (admin)
// ---------------------------------------------------------------------------

async fn list_rules(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<wire::RuleList>> {
    st.require(st.authz.allows_somewhere(&p, Access::Read), &p, &c, "list_rules", "namespaces")?;
    let repo = st.repo.clone();
    let rules = blocking(move || repo.rules()).await?;
    Ok(Json(wire::RuleList {
        rules: rules.into_iter().map(|(prefix, r)| wire::RuleJson { prefix, rule: r.as_str().to_string() }).collect(),
    }))
}

async fn set_rule(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Path(prefix): Path<String>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    validate_path(&prefix).map_err(ApiError::from)?;
    let req: wire::SetRuleRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("bad set-rule request: {e}")))?;
    let rule = wire::rule_from_wire(&req.rule).map_err(ApiError::from)?;
    st.require(st.authz.allows(&p, &prefix, Access::Admin), &p, &c, "set_rule", &prefix)?;
    st.require_audit_healthy()?;
    let repo = st.repo.clone();
    let pf = prefix.clone();
    let res = blocking(move || repo.set_rule(&pf, rule)).await;
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "set_rule",
        target: prefix,
        outcome: if res.is_ok() { "ok".into() } else { "rejected".into() },
        detail: Some(format!("rule={}", rule.as_str())),
        remote: c.remote.clone(),
    })
    .await?;
    res?;
    Ok(StatusCode::NO_CONTENT)
}

/// GC tracer for adapter content: git objects first, then OCI/segment maps.
pub struct AdapterTracer {
    pub git: crate::git::GitTracer,
    pub oci: crate::oci::OciTracer,
}

impl pico2peta_repo::Tracer for AdapterTracer {
    fn children(&self, h: &Hash, store: &dyn pico2peta_core::ObjectStore) -> pico2peta_core::Result<Vec<Hash>> {
        if let Some(c) = self.git.children_of(h, store)? {
            return Ok(c);
        }
        Ok(self.oci.children_of(h, store)?.unwrap_or_default())
    }
}

#[derive(serde::Deserialize)]
struct GcQuery {
    #[serde(default)]
    dry_run: bool,
    /// Regroup fully-live packs too (aggressive repack).
    #[serde(default)]
    repack_all: bool,
}

/// Runs one GC invocation, unless one is already running.
pub async fn run_gc(st: &AppState, dry_run: bool, repack_all: bool) -> ApiResult<wire::GcReportJson> {
    let Some(local) = st.local.clone() else {
        return Err(ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", "GC needs a local repository"));
    };
    if st.gc_running.swap(true, Ordering::AcqRel) {
        return Err(ApiError::new(StatusCode::CONFLICT, codes::GC_RUNNING, "a GC run is already in progress"));
    }
    let opts = st.gc.options_with(dry_run, repack_all);
    let grouped = st.gc.grouped;
    let running = st.gc_running.clone();
    let res = blocking(move || {
        // Provenance records, then git objects, OCI manifests and segment maps;
        // everything else is a leaf.
        let shared: Arc<dyn Repository> = Arc::new(local.clone());
        let tracer = pico2peta_prov::ProvTracer {
            inner: AdapterTracer {
                git: crate::git::GitTracer::new(shared.clone())?,
                oci: crate::oci::OciTracer::new(shared),
            },
        };
        let grouper = crate::git::GitGrouper;
        let r = local.gc_with(&opts, &tracer, if grouped { Some(&grouper) } else { None });
        running.store(false, Ordering::Release);
        r
    })
    .await;
    if res.is_err() {
        st.gc_running.store(false, Ordering::Release);
    }
    let r = res?;
    let outcome = if r.dangling_refs.is_empty() { "ok" } else { "dangling_refs" };
    st.metrics.gc_runs.get_or_create(&OutcomeLabels { outcome: outcome.into() }).inc();
    if !dry_run {
        st.metrics.gc_deleted_bytes.inc_by(r.deleted_bytes);
    }
    Ok(wire::GcReportJson {
        dry_run,
        planned: r.planned,
        swept: r.swept,
        condemned_packs: r.condemned_packs,
        copied_records: r.copied_records,
        rescued_records: r.rescued_records,
        deleted_packs: r.deleted_packs,
        deleted_bytes: r.deleted_bytes,
        live_hashes: r.live_hashes,
        dangling_refs: r.dangling_refs,
    })
}

async fn gc(
    State(st): St,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<GcQuery>,
) -> ApiResult<Json<wire::GcReportJson>> {
    st.require(st.authz.allows(&p, "", Access::Admin), &p, &c, "gc", "repository")?;
    st.require_audit_healthy()?;
    let res = run_gc(&st, q.dry_run, q.repack_all).await;
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "gc",
        target: "repository".into(),
        outcome: match &res {
            Ok(_) => "ok".into(),
            Err(e) => e.code.to_string(),
        },
        detail: res.as_ref().ok().map(|r| {
            format!(
                "dry_run={} deleted_packs={} deleted_bytes={} live={}",
                r.dry_run, r.deleted_packs, r.deleted_bytes, r.live_hashes
            )
        }),
        remote: c.remote.clone(),
    })
    .await?;
    Ok(Json(res?))
}

async fn whoami(Extension(p): Extension<Principal>) -> Json<wire::WhoAmI> {
    Json(wire::WhoAmI { principal: p.name, authenticated: p.authenticated })
}

// ---------------------------------------------------------------------------
// Ops
// ---------------------------------------------------------------------------

async fn readyz(State(st): St) -> Response {
    if st.shutting_down.load(Ordering::Acquire) {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    }
    if !st.audit.is_healthy() {
        return (StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable").into_response();
    }
    let repo = st.repo.clone();
    match tokio::time::timeout(Duration::from_secs(5), blocking(move || repo.rules())).await {
        Ok(Ok(_)) => (StatusCode::OK, "ready").into_response(),
        Ok(Err(e)) => (StatusCode::SERVICE_UNAVAILABLE, format!("repository: {}", e.message)).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "repository check timed out").into_response(),
    }
}

async fn metrics_main(State(st): St) -> Response {
    if st.metrics_separate {
        return (StatusCode::NOT_FOUND, "metrics are served on server.metrics_listen").into_response();
    }
    metrics(State(st)).await
}

/// The ops-only router for `server.metrics_listen`.
pub fn ops_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn metrics(State(st): St) -> Response {
    let mut r = st.metrics.render().into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/openmetrics-text; version=1.0.0; charset=utf-8"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hv(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 10), Ok(None));
        assert_eq!(parse_range(Some(&hv("bytes=0-3")), 10), Ok(Some(0..4)));
        assert_eq!(parse_range(Some(&hv("bytes=5-")), 10), Ok(Some(5..10)));
        assert_eq!(parse_range(Some(&hv("bytes=-3")), 10), Ok(Some(7..10)));
        assert_eq!(parse_range(Some(&hv("bytes=-30")), 10), Ok(Some(0..10)));
        assert_eq!(parse_range(Some(&hv("bytes=8-100")), 10), Ok(Some(8..10)));
        assert_eq!(parse_range(Some(&hv("bytes=0-1,4-5")), 10), Ok(None));
        assert_eq!(parse_range(Some(&hv("items=0-1")), 10), Ok(None));
        for bad in ["bytes=10-", "bytes=5-4", "bytes=-0", "bytes=x-1", "bytes=1", "bytes=18446744073709551615-"] {
            assert_eq!(parse_range(Some(&hv(bad)), 10), Err(Unsatisfiable), "{bad}");
        }
        assert_eq!(parse_range(Some(&hv("bytes=0-")), 0), Err(Unsatisfiable));
        assert_eq!(parse_range(Some(&hv("bytes=0-18446744073709551615")), 10), Ok(Some(0..10)));
    }
}
