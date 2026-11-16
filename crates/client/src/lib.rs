//! Rust client for `o5s-server`.
//!
//! [`RemoteRepo`] implements `o5s_core::Repository` over the HTTP API
//! (see [`wire`]), so code written against the trait works the same on a
//! local repository and on a server. The CLI's `--remote` mode uses it.
//!
//! * **End-to-end integrity:** `put` sends the BLAKE3 hash, and the server
//!   verifies it. `put_stream` hashes while streaming and checks the server's
//!   answer. Whole-object `get`/`open` re-hash the bytes and fail with
//!   `Error::Corrupt` on a mismatch.
//! * **Retries:** idempotent requests (reads, `put` of a byte slice) are retried
//!   on transport errors and 502/503/504 with jittered backoff. `replace` is
//!   retried only when the connection failed before the request was sent, and
//!   a one-shot `put_stream` is never retried.
//! * **Streaming:** `put_stream` and `open` move data through bounded channels
//!   (about 1 MiB in flight), whatever the object size.
//!
//! The API is synchronous. A small internal tokio runtime does the I/O, so
//! calls work from any thread except an async worker thread. From async code,
//! use `spawn_blocking`.

pub mod wire;

use bytes::Bytes;
use o5s_core::{
    Error, Hash, Name, ObjectStore, Observed, RefValue, ReplaceManyOutcome, ReplaceOutcome, Repository, Result, Rule,
};
use reqwest::{Method, StatusCode};
use std::future::Future;
use std::io::Read;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Client tunables.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub connect_timeout: Duration,
    /// Timeout for requests without large bodies, and idle-read timeout for
    /// streaming transfers.
    pub request_timeout: Duration,
    /// Attempts for retryable requests (including the first).
    pub max_attempts: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            max_attempts: 5,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

/// A `Repository` served by `o5s-server`. Cheap to clone.
#[derive(Clone)]
pub struct RemoteRepo {
    inner: Arc<Inner>,
}

struct Inner {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
    cfg: ClientConfig,
    rt: Option<tokio::runtime::Runtime>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

fn transport(e: reqwest::Error) -> Error {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    Error::Backend { transient: true, msg: format!("server unreachable: {msg}") }
}

/// What a failed attempt means for retrying.
fn retryable_status(s: StatusCode) -> bool {
    matches!(s, StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT)
}

/// Context used to rebuild typed core errors from wire errors.
#[derive(Default)]
struct Ctx<'a> {
    hash: Option<Hash>,
    name: Option<&'a Name>,
    range: Option<Range<u64>>,
}

/// Converts an error response into a core error.
async fn error_from(resp: reqwest::Response, ctx: &Ctx<'_>) -> Error {
    let status = resp.status();
    let content_range = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).map(str::to_string);
    let body = resp.bytes().await.unwrap_or_default();
    let detail =
        serde_json::from_slice::<wire::ErrorBody>(&body).map(|b| b.error).unwrap_or_else(|_| wire::ErrorDetail {
            code: String::new(),
            message: format!("HTTP {status}: {}", String::from_utf8_lossy(&body[..body.len().min(256)])),
            rule: None,
            subject: None,
        });
    let msg = detail.message.clone();
    let subj_hash = || detail.subject.as_deref().and_then(|s| s.parse::<Hash>().ok());
    let subj_name = || detail.subject.as_deref().and_then(|s| Name::new(s).ok()).or_else(|| ctx.name.cloned());
    match detail.code.as_str() {
        "not_found" => match ctx.hash.or_else(subj_hash) {
            Some(h) => Error::NotFound(h),
            None => Error::Parse(msg),
        },
        "invalid_range" => {
            let len = content_range.as_deref().and_then(|v| v.rsplit('/').next()).and_then(|v| v.parse().ok());
            match (&ctx.range, len) {
                (Some(r), Some(len)) => Error::InvalidRange { start: r.start, end: r.end, len },
                _ => Error::Parse(msg),
            }
        }
        "invalid_name" => Error::InvalidName(detail.subject.clone().unwrap_or_default(), "rejected by server"),
        "target_not_in_read_set" => match subj_name() {
            Some(n) => Error::TargetNotInReadSet(n),
            None => Error::Parse(msg),
        },
        "missing_object" => match subj_hash() {
            Some(h) => Error::MissingObject(h),
            None => Error::Parse(msg),
        },
        "forbidden" => match (subj_name(), detail.rule.as_deref().and_then(|r| r.parse::<Rule>().ok())) {
            (Some(name), Some(rule)) => Error::Forbidden { name, rule, why: "rejected by the namespace rule (server)" },
            _ => Error::Parse(msg),
        },
        "rule_change_on_non_empty" => Error::RuleChangeOnNonEmpty(detail.subject.clone().unwrap_or_default()),
        "corrupt" => Error::Corrupt(msg),
        "parse" | wire::codes::BAD_REQUEST | wire::codes::HASH_MISMATCH => Error::Parse(msg),
        "unsupported" => Error::Unsupported("operation not supported by the server"),
        "locked" => Error::Locked,
        wire::codes::UNAUTHENTICATED => Error::Unauthorized(format!("unauthenticated: {msg}")),
        wire::codes::PERMISSION_DENIED | "unauthorized" => Error::Unauthorized(format!("permission denied: {msg}")),
        wire::codes::TOO_LARGE => Error::Backend { transient: false, msg: format!("payload too large: {msg}") },
        _ => Error::Backend { transient: status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS, msg },
    }
}

impl RemoteRepo {
    /// Builds a client for `url` (e.g. `https://o5s.example.com`) without contacting it.
    pub fn new(url: &str, token: Option<String>, cfg: ClientConfig) -> Result<RemoteRepo> {
        let base = url.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::Parse(format!("server URL must start with http:// or https://, got {url:?}")));
        }
        reqwest::Url::parse(&base).map_err(|e| Error::Parse(format!("bad server URL {url:?}: {e}")))?;
        if let Some(t) = &token
            && (t.is_empty() || t.bytes().any(|b| b.is_ascii_control() || b == b' '))
        {
            return Err(Error::Parse("token must be non-empty and contain no spaces or control characters".into()));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.request_timeout)
            .pool_max_idle_per_host(16)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|e| Error::Backend { transient: false, msg: format!("http client: {e}") })?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("o5s-client")
            .enable_all()
            .build()?;
        Ok(RemoteRepo { inner: Arc::new(Inner { base, token, http, cfg, rt: Some(rt) }) })
    }

    /// Like [`RemoteRepo::new`] with default settings, then checks that the
    /// server is reachable and accepts the credentials (`GET /v1/whoami`).
    pub fn connect(url: &str, token: Option<String>) -> Result<RemoteRepo> {
        let r = Self::new(url, token, ClientConfig::default())?;
        r.whoami()?;
        Ok(r)
    }

    /// The principal the server sees for this client.
    pub fn whoami(&self) -> Result<wire::WhoAmI> {
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::GET, "/v1/whoami", |r| r).await?;
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            resp.json().await.map_err(transport)
        })
    }

    /// Triggers one GC invocation on the server (needs admin on `""`).
    pub fn gc(&self, dry_run: bool) -> Result<wire::GcReportJson> {
        let this = self.clone();
        self.block(async move {
            let rb = this
                .request(Method::POST, "/v1/admin/gc")
                .query(&[("dry_run", if dry_run { "true" } else { "false" })])
                .timeout(Duration::from_secs(24 * 3600));
            let resp = rb.send().await.map_err(transport)?;
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            resp.json().await.map_err(transport)
        })
    }

    /// POSTs an atomic multi-name update (not idempotent: retried only when the
    /// connection was never established).
    fn post_multi(&self, path: &'static str, body: Vec<u8>, first: Option<Name>) -> Result<ReplaceManyOutcome> {
        let this = self.clone();
        self.block(async move {
            let mut attempt = 0;
            let resp = loop {
                let rb = this
                    .request(Method::POST, path)
                    .header("content-type", "application/json")
                    .body(body.clone())
                    .timeout(this.inner.cfg.request_timeout);
                match rb.send().await {
                    Ok(r) => break r,
                    Err(e) if e.is_connect() && attempt + 1 < this.inner.cfg.max_attempts => {
                        tokio::time::sleep(this.backoff(attempt)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(transport(e)),
                }
            };
            match resp.status() {
                StatusCode::OK | StatusCode::CONFLICT => {
                    let r: wire::ReplaceManyResponse = resp.json().await.map_err(transport)?;
                    r.to_core()
                }
                _ => Err(error_from(resp, &Ctx { name: first.as_ref(), ..Default::default() }).await),
            }
        })
    }

    /// Streams `src` to `path` (PUT), hashing as it goes.
    fn put_stream_to(&self, path: String, src: &mut dyn Read) -> Result<Hash> {
        let rt = self.inner.rt.as_ref().ok_or(Error::Unsupported("client runtime is shut down"))?;
        let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
        let body = reqwest::Body::wrap_stream(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|i| (i, rx))
        }));
        let rb = self.request(Method::PUT, &path).header("content-type", "application/octet-stream").body(body);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        rt.spawn(async move {
            let _ = done_tx.send(rb.send().await);
        });
        // Read on the caller's thread; a full channel blocks us (backpressure).
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut read_err = None;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = tx.blocking_send(Err(std::io::Error::new(e.kind(), e.to_string())));
                    read_err = Some(e);
                    break;
                }
            };
            hasher.update(&buf[..n]);
            if tx.blocking_send(Ok(Bytes::copy_from_slice(&buf[..n]))).is_err() {
                break; // request ended early (rejected); its response explains why
            }
        }
        drop(tx);
        let local = Hash(*hasher.finalize().as_bytes());
        let resp =
            done_rx.recv().map_err(|_| Error::Backend { transient: true, msg: "client runtime stopped".into() })?;
        if let Some(e) = read_err {
            return Err(Error::Io(e));
        }
        let resp = resp.map_err(transport)?;
        self.block(put_result(resp, local))
    }

    /// Streams an object in using the chunker configured for `name`'s namespace on the
    /// server (API v1.11). Doesn't create the name. Needs write access to `name`.
    pub fn put_stream_for(&self, name: &Name, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_to(format!("/v1/objects?for_name={}", name), src)
    }

    /// [`Self::put_stream_for`] with byte grouping of `width`-byte elements (API v1.12).
    pub fn put_stream_for_grouped(&self, name: &Name, src: &mut dyn Read, width: u8) -> Result<Hash> {
        self.put_stream_to(format!("/v1/objects?for_name={name}&group_width={width}"), src)
    }

    /// Same contract as `LocalRepo::put_named`: the server applies the profile of `name`'s
    /// namespace (front encoding plan, chunker, `derive = "previous"` against the object `name`
    /// points at now). Write access on `name` is required. The name itself is not moved.
    pub fn put_named(&self, name: &Name, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_for(name, src)
    }

    /// [`Self::put_named`] with a byte-grouping width hint, used when the name's front doesn't
    /// choose one (same as `LocalRepo::put_named_hinted`).
    pub fn put_named_hinted(&self, name: &Name, src: &mut dyn Read, width: Option<u8>) -> Result<Hash> {
        match width {
            Some(w) => self.put_stream_for_grouped(name, src, w),
            None => self.put_stream_for(name, src),
        }
    }

    /// `POST /v1/attestations`: stores a DSSE envelope and links it to its subjects. Returns the
    /// stored envelope's hash and the server-trusted keys that verify it (possibly none: any
    /// writer may attest, and promotion only counts trusted keys).
    pub fn attest(&self, envelope: &o5s_attest::Envelope) -> Result<(Hash, Vec<String>)> {
        #[derive(serde::Deserialize)]
        struct Resp {
            envelope: String,
            trusted_keys: Vec<String>,
        }
        let resp = self.post_once("/v1/attestations", "application/json", envelope.to_json())?;
        self.block(async move {
            if resp.status() != StatusCode::CREATED {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            let r: Resp = resp.json().await.map_err(transport)?;
            Ok((r.envelope.parse()?, r.trusted_keys))
        })
    }

    /// `GET /v1/attestations?ref=<id>`: the envelopes about an object (`ref_id` is an external
    /// id such as `name:<name>`, `blake3:<hex>`, `git:<repo>@<oid>`), each with its hash and the
    /// server-trusted keys that verify it.
    pub fn attestations(&self, ref_id: &str) -> Result<Vec<(Hash, o5s_attest::Envelope, Vec<String>)>> {
        #[derive(serde::Deserialize)]
        struct Item {
            hash: String,
            envelope: serde_json::Value,
            trusted_keys: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            attestations: Vec<Item>,
        }
        let r: Resp = self.get_json("/v1/attestations", vec![("ref", ref_id.to_string())])?;
        r.attestations
            .into_iter()
            .map(|i| {
                let raw = serde_json::to_vec(&i.envelope).map_err(|e| Error::Parse(format!("envelope: {e}")))?;
                Ok((i.hash.parse()?, o5s_attest::Envelope::from_json(&raw)?, i.trusted_keys))
            })
            .collect()
    }

    /// `POST /v1/promote`: points `dst` at what `src` names if the server's `[[promotion]]` rule
    /// for `dst` holds. A refusal by the policy is `Ok(Refused(reasons))`; missing access or no
    /// rule covering `dst` is an error.
    pub fn promote(&self, src: &Name, dst: &Name) -> Result<o5s_attest::PromoteOutcome> {
        use o5s_attest::PromoteOutcome;
        #[derive(serde::Deserialize)]
        struct Resp {
            promoted: Option<String>,
            record: Option<String>,
            refused: Option<Vec<String>>,
        }
        let body = serde_json::json!({ "src": src.as_str(), "dst": dst.as_str() }).to_string().into_bytes();
        let resp = self.post_once("/v1/promote", "application/json", body)?;
        let dst = dst.clone();
        self.block(async move {
            match resp.status() {
                StatusCode::CONFLICT => Ok(PromoteOutcome::Conflict),
                s if s.is_success() || s == StatusCode::FORBIDDEN => {
                    let bytes = resp.bytes().await.map_err(transport)?;
                    match serde_json::from_slice::<Resp>(&bytes) {
                        Ok(Resp { promoted: Some(o), record: Some(r), .. }) if s.is_success() => {
                            Ok(PromoteOutcome::Promoted { object: o.parse()?, record: r.parse()? })
                        }
                        Ok(Resp { refused: Some(reasons), .. }) => Ok(PromoteOutcome::Refused(reasons)),
                        _ => {
                            let msg = serde_json::from_slice::<wire::ErrorBody>(&bytes).map_or_else(
                                |_| String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).into_owned(),
                                |b| b.error.message,
                            );
                            Err(Error::Unauthorized(format!("promote to {dst}: HTTP {s}: {msg}")))
                        }
                    }
                }
                _ => Err(error_from(resp, &Ctx { name: Some(&dst), ..Default::default() }).await),
            }
        })
    }

    /// `GET /v1/provenance?ref=<id>`: producers, consumers, lineage and impact.
    pub fn provenance(&self, ref_id: &str) -> Result<wire::ProvenanceJson> {
        self.get_json("/v1/provenance", vec![("ref", ref_id.to_string())])
    }

    /// Every object `ref_id` was transitively derived from (provenance lineage).
    pub fn lineage(&self, ref_id: &str) -> Result<Vec<Hash>> {
        self.provenance(ref_id)?.lineage.iter().map(|h| h.parse()).collect()
    }

    /// Every object transitively derived from `ref_id` (provenance impact).
    pub fn impact(&self, ref_id: &str) -> Result<Vec<Hash>> {
        self.provenance(ref_id)?.impact.iter().map(|h| h.parse()).collect()
    }

    /// `GET /v1/search`: names, front metadata and provenance, limited to names the caller can
    /// read (at most 200 hits). A `&str` searches text; see [`Search`] for prefix/exact.
    pub fn search(&self, query: impl Into<Search>) -> Result<Vec<SearchHit>> {
        let (k, v) = match query.into() {
            Search::Text(t) => ("q", t),
            Search::Prefix(p) => ("prefix", p),
            Search::Exact(e) => ("exact", e),
        };
        let r: wire::SearchResults = self.get_json("/v1/search", vec![(k, v)])?;
        r.results
            .into_iter()
            .map(|h| {
                Ok(SearchHit {
                    object: h.object.parse()?,
                    names: h.names,
                    meta: h.meta.into_iter().map(|m| (m.key, m.value, m.op)).collect(),
                })
            })
            .collect()
    }

    /// An idempotent JSON GET with query parameters.
    fn get_json<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        path: &'static str,
        query: Vec<(&'static str, String)>,
    ) -> Result<T> {
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::GET, path, |r| r.query(&query)).await?;
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            resp.json().await.map_err(transport)
        })
    }

    /// A non-idempotent POST: retried only when the connection was never established.
    fn post_once(&self, path: &'static str, content_type: &'static str, body: Vec<u8>) -> Result<reqwest::Response> {
        let this = self.clone();
        self.block(async move {
            let mut attempt = 0;
            loop {
                let rb = this
                    .request(Method::POST, path)
                    .header("content-type", content_type)
                    .body(body.clone())
                    .timeout(this.inner.cfg.request_timeout);
                match rb.send().await {
                    Ok(r) => return Ok(r),
                    Err(e) if e.is_connect() && attempt + 1 < this.inner.cfg.max_attempts => {
                        tokio::time::sleep(this.backoff(attempt)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(transport(e)),
                }
            }
        })
    }

    /// Base URL of the server.
    pub fn url(&self) -> &str {
        &self.inner.base
    }

    fn block<T, F>(&self, fut: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let rt = self.inner.rt.as_ref().ok_or(Error::Unsupported("client runtime is shut down"))?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv().map_err(|_| Error::Backend { transient: true, msg: "client runtime stopped".into() })?
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self.inner.http.request(method, format!("{}{path}", self.inner.base));
        match &self.inner.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let c = &self.inner.cfg;
        let cap = c.base_backoff.saturating_mul(1u32 << attempt.min(16)).min(c.max_backoff);
        Duration::from_nanos(fastrand::u64(0..=cap.as_nanos().min(u64::MAX as u128) as u64))
    }

    /// Sends an idempotent request, retrying transport errors and 502/503/504.
    async fn send_retry(
        &self,
        method: Method,
        path: &str,
        build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            let rb = build(self.request(method.clone(), path)).timeout(self.inner.cfg.request_timeout);
            let last = attempt + 1 >= self.inner.cfg.max_attempts;
            match rb.send().await {
                Ok(r) if retryable_status(r.status()) && !last => {
                    warn!(path, status = %r.status(), attempt, "server busy; retrying");
                }
                Ok(r) => return Ok(r),
                Err(e) if !last && (e.is_connect() || e.is_timeout() || e.is_request()) => {
                    warn!(path, error = %e, attempt, "request failed; retrying");
                }
                Err(e) => return Err(transport(e)),
            }
            tokio::time::sleep(self.backoff(attempt)).await;
            attempt += 1;
        }
    }

    /// Lists every ref under `prefix` as `wire::RefJson` (the trait method converts).
    fn refs_wire(&self, prefix: &str) -> Result<Vec<wire::RefJson>> {
        let this = self.clone();
        let prefix = prefix.to_string();
        self.block(async move {
            let resp = this.send_retry(Method::GET, "/v1/refs", |r| r.query(&[("prefix", prefix.as_str())])).await?;
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            let l: wire::RefList = resp.json().await.map_err(transport)?;
            Ok(l.refs)
        })
    }
}

/// A [`RemoteRepo::search`] query.
#[derive(Clone, Debug)]
pub enum Search {
    /// Free text over names, front metadata and provenance.
    Text(String),
    /// Names starting with this prefix.
    Prefix(String),
    /// One exact name.
    Exact(String),
}

impl From<&str> for Search {
    fn from(s: &str) -> Search {
        Search::Text(s.to_string())
    }
}

/// One [`RemoteRepo::search`] result: the object, the readable names it has, and its metadata
/// as `(key, value, op)`.
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub object: Hash,
    pub names: Vec<String>,
    pub meta: Vec<(String, String, String)>,
}

/// Parses a `PutResponse` and checks it against the locally computed hash.
async fn put_result(resp: reqwest::Response, local: Hash) -> Result<Hash> {
    if !resp.status().is_success() {
        return Err(error_from(resp, &Ctx::default()).await);
    }
    let pr: wire::PutResponse = resp.json().await.map_err(transport)?;
    let h: Hash = pr.hash.parse()?;
    if h != local {
        return Err(Error::Corrupt(format!("server stored {h}, but the data hashes to {local}")));
    }
    Ok(h)
}

impl ObjectStore for RemoteRepo {
    fn put(&self, data: &[u8]) -> Result<Hash> {
        let local = Hash::of(data);
        let body = Bytes::copy_from_slice(data);
        let this = self.clone();
        self.block(async move {
            let resp = this
                .send_retry(Method::PUT, "/v1/objects", |r| {
                    r.header(wire::EXPECT_HASH_HEADER, local.to_hex())
                        .header("content-type", "application/octet-stream")
                        .body(body.clone())
                })
                .await?;
            put_result(resp, local).await
        })
    }

    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_to("/v1/objects".to_string(), src)
    }

    fn put_stream_grouped(&self, src: &mut dyn Read, width: u8) -> Result<Hash> {
        self.put_stream_to(format!("/v1/objects?group_width={width}"), src)
    }

    fn put_delta(&self, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
        self.put_stream_to(format!("/v1/objects?delta_base={}", base.to_hex()), src)
    }
    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>> {
        let mut r = self.open(hash, range)?;
        let mut v = Vec::new();
        r.read_to_end(&mut v).map_err(unwrap_io)?;
        Ok(v)
    }

    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>> {
        self.open_with(hash, range, None)
    }

    /// Sends `?verify=` (API v1.17); the server's repository decides whether `eof` is allowed.
    fn open_verified(
        &self,
        hash: &Hash,
        range: Option<Range<u64>>,
        verify: o5s_core::ReadVerify,
    ) -> Result<Box<dyn Read + Send>> {
        self.open_with(hash, range, Some(verify))
    }

    fn size(&self, hash: &Hash) -> Result<u64> {
        let hash = *hash;
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::HEAD, &format!("/v1/objects/{}", hash.to_hex()), |r| r).await?;
            match resp.status() {
                s if s.is_success() => resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| Error::Corrupt("HEAD response without Content-Length".into())),
                StatusCode::NOT_FOUND => Err(Error::NotFound(hash)),
                _ => Err(error_from(resp, &Ctx { hash: Some(hash), ..Default::default() }).await),
            }
        })
    }

    fn contains(&self, hash: &Hash) -> Result<bool> {
        match self.size(hash) {
            Ok(_) => Ok(true),
            Err(Error::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn flush(&self) -> Result<()> {
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::POST, "/v1/flush", |r| r).await?;
            if resp.status().is_success() { Ok(()) } else { Err(error_from(resp, &Ctx::default()).await) }
        })
    }
}

/// Unwraps an io::Error that carries a core::Error.
fn unwrap_io(e: std::io::Error) -> Error {
    match e.downcast::<Error>() {
        Ok(inner) => inner,
        Err(e) => Error::Io(e),
    }
}

/// Streams a response body, checking its length and (for whole objects) its hash.
struct VerifyingReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Bytes>>,
    cur: Bytes,
    hasher: Option<blake3::Hasher>,
    expect: Hash,
    total: Option<u64>,
    seen: u64,
    done: bool,
}

impl VerifyingReader {
    fn finish(&mut self) -> std::io::Result<()> {
        self.done = true;
        if let Some(t) = self.total
            && t != self.seen
        {
            return Err(std::io::Error::other(Error::Corrupt(format!(
                "object {} truncated: got {} of {t} bytes",
                self.expect, self.seen
            ))));
        }
        if let Some(h) = self.hasher.take() {
            let got = Hash(*h.finalize().as_bytes());
            if got != self.expect {
                return Err(std::io::Error::other(Error::Corrupt(format!(
                    "object {} failed verification (content hashes to {got})",
                    self.expect
                ))));
            }
        }
        Ok(())
    }
}

impl Read for VerifyingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.cur.is_empty() {
            if self.done {
                return Ok(0);
            }
            match self.rx.blocking_recv() {
                Some(Ok(b)) => {
                    self.seen += b.len() as u64;
                    if let Some(h) = &mut self.hasher {
                        h.update(&b);
                    }
                    self.cur = b;
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                None => {
                    self.finish()?;
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.cur.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        let _ = self.cur.split_to(n);
        Ok(n)
    }
}

impl Repository for RemoteRepo {
    fn resolve(&self, name: &Name) -> Result<Option<RefValue>> {
        let name = name.clone();
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::GET, &format!("/v1/refs/{name}"), |r| r).await?;
            if resp.status() == StatusCode::NOT_FOUND {
                let body = resp.bytes().await.map_err(transport)?;
                return match serde_json::from_slice::<wire::ErrorBody>(&body) {
                    Ok(b) if b.error.code == wire::codes::REF_NOT_FOUND => Ok(None),
                    _ => Err(Error::Backend {
                        transient: false,
                        msg: format!("unexpected 404: {}", String::from_utf8_lossy(&body)),
                    }),
                };
            }
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx { name: Some(&name), ..Default::default() }).await);
            }
            let r: wire::RefJson = resp.json().await.map_err(transport)?;
            let (n, v) = r.to_core()?;
            if n != name {
                return Err(Error::Corrupt(format!("asked for {name}, server answered for {n}")));
            }
            Ok(Some(v))
        })
    }

    fn replace(&self, name: &Name, read_set: &[(Name, Observed)], new: Hash) -> Result<ReplaceOutcome> {
        let body = serde_json::to_vec(&wire::ReplaceRequest::from_core(name, read_set, new))
            .map_err(|e| Error::Parse(e.to_string()))?;
        let this = self.clone();
        let name = name.clone();
        self.block(async move {
            // Not idempotent: retry only when the connection was never established.
            let mut attempt = 0;
            let resp = loop {
                let rb = this
                    .request(Method::POST, "/v1/replace")
                    .header("content-type", "application/json")
                    .body(body.clone())
                    .timeout(this.inner.cfg.request_timeout);
                match rb.send().await {
                    Ok(r) => break r,
                    Err(e) if e.is_connect() && attempt + 1 < this.inner.cfg.max_attempts => {
                        tokio::time::sleep(this.backoff(attempt)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(transport(e)),
                }
            };
            match resp.status() {
                StatusCode::OK | StatusCode::CONFLICT => {
                    let r: wire::ReplaceResponse = resp.json().await.map_err(transport)?;
                    r.to_core()
                }
                _ => Err(error_from(resp, &Ctx { name: Some(&name), hash: Some(new), range: None }).await),
            }
        })
    }

    fn replace_many(&self, updates: &[(Name, Hash)], read_set: &[(Name, Observed)]) -> Result<ReplaceManyOutcome> {
        let body = serde_json::to_vec(&wire::ReplaceManyRequest::from_core(updates, read_set))
            .map_err(|e| Error::Parse(e.to_string()))?;
        self.post_multi("/v1/replace_many", body, updates.first().map(|(n, _)| n.clone()))
    }

    fn update_many(
        &self,
        updates: &[(Name, Option<Hash>)],
        read_set: &[(Name, Observed)],
    ) -> Result<ReplaceManyOutcome> {
        let body = serde_json::to_vec(&wire::UpdateManyRequest::from_core(updates, read_set))
            .map_err(|e| Error::Parse(e.to_string()))?;
        self.post_multi("/v1/update_many", body, updates.first().map(|(n, _)| n.clone()))
    }

    fn delete_ref(&self, name: &Name, read_set: &[(Name, Observed)]) -> Result<ReplaceOutcome> {
        let body = serde_json::to_vec(&wire::DeleteRefRequest {
            read_set: read_set.iter().map(|(n, o)| wire::ReadSetEntry::from_core(n, *o)).collect(),
        })
        .map_err(|e| Error::Parse(e.to_string()))?;
        let this = self.clone();
        let name = name.clone();
        self.block(async move {
            // OCC with an explicit read set: retrying after a lost response
            // yields Conflict (or Ok version 0), never a wrong delete. Still, only
            // retry when the connection was never established.
            let mut attempt = 0;
            let resp = loop {
                let rb = this
                    .request(Method::DELETE, &format!("/v1/refs/{name}"))
                    .header("content-type", "application/json")
                    .body(body.clone())
                    .timeout(this.inner.cfg.request_timeout);
                match rb.send().await {
                    Ok(r) => break r,
                    Err(e) if e.is_connect() && attempt + 1 < this.inner.cfg.max_attempts => {
                        tokio::time::sleep(this.backoff(attempt)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(transport(e)),
                }
            };
            match resp.status() {
                StatusCode::OK | StatusCode::CONFLICT => {
                    let r: wire::ReplaceResponse = resp.json().await.map_err(transport)?;
                    r.to_core()
                }
                _ => Err(error_from(resp, &Ctx { name: Some(&name), ..Default::default() }).await),
            }
        })
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(Name, RefValue)>> {
        self.refs_wire(prefix)?.iter().map(wire::RefJson::to_core).collect()
    }

    fn set_rule(&self, prefix: &str, rule: Rule) -> Result<()> {
        o5s_core::validate_path(prefix)?;
        let body = serde_json::to_vec(&wire::SetRuleRequest { rule: rule.as_str().to_string() })
            .map_err(|e| Error::Parse(e.to_string()))?;
        let this = self.clone();
        let path = format!("/v1/namespaces/{prefix}");
        self.block(async move {
            // Idempotent: setting the same rule twice is harmless.
            let resp = this
                .send_retry(Method::PUT, &path, |r| r.header("content-type", "application/json").body(body.clone()))
                .await?;
            if resp.status().is_success() { Ok(()) } else { Err(error_from(resp, &Ctx::default()).await) }
        })
    }

    fn rules(&self) -> Result<Vec<(String, Rule)>> {
        let this = self.clone();
        self.block(async move {
            let resp = this.send_retry(Method::GET, "/v1/namespaces", |r| r).await?;
            if !resp.status().is_success() {
                return Err(error_from(resp, &Ctx::default()).await);
            }
            let l: wire::RuleList = resp.json().await.map_err(transport)?;
            l.rules.into_iter().map(|r| Ok((r.prefix, r.rule.parse()?))).collect()
        })
    }
}

impl RemoteRepo {
    /// `GET /v1/objects/<hash>`, optionally with `?verify=`.
    fn open_with(
        &self,
        hash: &Hash,
        range: Option<Range<u64>>,
        verify: Option<o5s_core::ReadVerify>,
    ) -> Result<Box<dyn Read + Send>> {
        let hash = *hash;
        if let Some(r) = &range {
            if r.start > r.end {
                return Err(Error::InvalidRange { start: r.start, end: r.end, len: self.size(&hash)? });
            }
            if r.start == r.end {
                // HTTP cannot express an empty range; validate against the size.
                let len = self.size(&hash)?;
                o5s_core::check_range(Some(r.clone()), len)?;
                return Ok(Box::new(std::io::empty()));
            }
        }
        let this = self.clone();
        let rr = range.clone();
        let resp = self.block(async move {
            let path = match verify {
                Some(v) => format!("/v1/objects/{}?verify={}", hash.to_hex(), v.as_str()),
                None => format!("/v1/objects/{}", hash.to_hex()),
            };
            let resp = this
                .send_retry(Method::GET, &path, |rb| match &rr {
                    Some(r) => rb.header("range", format!("bytes={}-{}", r.start, r.end - 1)),
                    None => rb,
                })
                .await?;
            let ok = match &rr {
                Some(_) => resp.status() == StatusCode::PARTIAL_CONTENT,
                None => resp.status() == StatusCode::OK,
            };
            if !ok {
                if resp.status().is_success() {
                    return Err(Error::Corrupt(format!("unexpected status {} for object read", resp.status())));
                }
                return Err(error_from(resp, &Ctx { hash: Some(hash), name: None, range: rr.clone() }).await);
            }
            Ok(resp)
        })?;
        let expect_len = range.as_ref().map(|r| r.end - r.start);
        if let (Some(want), Some(got)) = (expect_len, resp.content_length())
            && want != got
        {
            return Err(Error::InvalidRange {
                start: range.as_ref().map_or(0, |r| r.start),
                end: range.as_ref().map_or(0, |r| r.end),
                len: got,
            });
        }
        let total = resp.content_length();
        let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
        let rt = self.inner.rt.as_ref().ok_or(Error::Unsupported("client runtime is shut down"))?;
        rt.spawn(async move {
            let mut resp = resp;
            loop {
                match resp.chunk().await {
                    Ok(Some(c)) => {
                        if tx.send(Ok(c)).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(std::io::Error::other(transport(e)))).await;
                        return;
                    }
                }
            }
        });
        Ok(Box::new(VerifyingReader {
            rx,
            cur: Bytes::new(),
            hasher: range.is_none().then(blake3::Hasher::new),
            expect: hash,
            total,
            seen: 0,
            done: false,
        }))
    }
}
