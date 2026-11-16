//! Async S3 client: SigV4, bounded concurrency, retries, conditional and multipart writes.
//!
//! Keys passed to this client are *logical* keys. The configured prefix is
//! prepended on the way out and stripped from listings.

use crate::config::{CredentialsProvider, S3Config};
use crate::error::{S3Error, S3Result};
use crate::retry::{RetryBudget, backoff};
use crate::sigv4::{self, EMPTY_SHA256};
use crate::xml;
use bytes::Bytes;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use md5::{Digest as _, Md5};
use reqwest::{Method, StatusCode};
use std::future::Future;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

/// Result of a create-only write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Created {
    Created,
    AlreadyExists,
}

/// `HEAD` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub len: u64,
    /// Quoted ETag as returned by the service.
    pub etag: Option<String>,
}

/// Body for [`S3Client::put_if_absent`].
#[derive(Clone, Debug)]
pub enum Body {
    Bytes(Bytes),
    /// A local file that does not change during the upload.
    File(PathBuf),
}

/// Monotonic counters, for metrics export.
#[derive(Default, Debug)]
pub struct S3Stats {
    pub requests: AtomicU64,
    pub retries: AtomicU64,
    pub throttled: AtomicU64,
    pub failures: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
}

/// A point-in-time copy of [`S3Stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct S3StatsSnapshot {
    pub requests: u64,
    pub retries: u64,
    pub throttled: u64,
    pub failures: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl S3Stats {
    pub fn snapshot(&self) -> S3StatsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        S3StatsSnapshot {
            requests: l(&self.requests),
            retries: l(&self.retries),
            throttled: l(&self.throttled),
            failures: l(&self.failures),
            bytes_sent: l(&self.bytes_sent),
            bytes_received: l(&self.bytes_received),
        }
    }
}

/// Cheaply cloneable async S3 client.
#[derive(Clone)]
pub struct S3Client {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: S3Config,
    creds: Arc<dyn CredentialsProvider>,
    http: reqwest::Client,
    sem: Arc<Semaphore>,
    budget: RetryBudget,
    stats: S3Stats,
    scheme: String,
    /// `Host` header value (`host[:port]`).
    host: String,
    /// Encoded path prefix for objects: `/bucket` (path style) or `` (virtual host).
    bucket_path: String,
    prefix: String,
}

/// One HTTP request, before signing.
struct Req<'a> {
    method: Method,
    op: &'static str,
    /// Logical key (for errors/URL) or `None` for bucket-level requests.
    key: Option<&'a str>,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<Bytes>,
    payload_sha256: String,
    /// Total request timeout; `None` = headers timeout only (streaming bodies).
    timeout: Option<Duration>,
}

impl<'a> Req<'a> {
    fn new(method: Method, op: &'static str, key: Option<&'a str>) -> Self {
        Req {
            method,
            op,
            key,
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            payload_sha256: EMPTY_SHA256.to_string(),
            timeout: None,
        }
    }
    fn q(mut self, k: &str, v: impl Into<String>) -> Self {
        self.query.push((k.to_string(), v.into()));
        self
    }
    fn h(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }
}

/// Validates a logical key with the same rules as the core backend contract.
pub fn validate_key(key: &str) -> S3Result<()> {
    if key.is_empty() || key.len() > 1024 {
        return Err(S3Error::InvalidKey(key.into(), "length must be 1..=1024"));
    }
    for s in key.split('/') {
        if s.is_empty() || s == "." || s == ".." {
            return Err(S3Error::InvalidKey(key.into(), "empty, '.' or '..' segment"));
        }
        if !s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')) {
            return Err(S3Error::InvalidKey(key.into(), "segments must be [A-Za-z0-9._-]"));
        }
    }
    Ok(())
}

impl S3Client {
    pub fn new(cfg: S3Config, creds: Arc<dyn CredentialsProvider>) -> S3Result<S3Client> {
        cfg.validate()?;
        let url = reqwest::Url::parse(&cfg.endpoint).map_err(|e| S3Error::Config(format!("bad endpoint: {e}")))?;
        if url.path() != "/" && !url.path().is_empty() {
            return Err(S3Error::Config("endpoint must not contain a path".into()));
        }
        if url.query().is_some() {
            return Err(S3Error::Config("endpoint must not contain a query".into()));
        }
        let host_only = url.host_str().ok_or_else(|| S3Error::Config("endpoint has no host".into()))?;
        let host_port = match url.port() {
            Some(p) => format!("{host_only}:{p}"),
            None => host_only.to_string(),
        };
        let prefix = cfg.norm_prefix();
        if !prefix.is_empty() {
            validate_key(prefix.trim_end_matches('/'))
                .map_err(|_| S3Error::Config(format!("invalid prefix {prefix:?}")))?;
        }
        let (host, bucket_path) = if cfg.path_style {
            (host_port, format!("/{}", sigv4::uri_encode(&cfg.bucket, false)))
        } else {
            (format!("{}.{host_port}", cfg.bucket), String::new())
        };
        let http = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.request_timeout)
            .pool_idle_timeout(cfg.pool_idle_timeout)
            .pool_max_idle_per_host(cfg.pool_max_idle)
            .tcp_nodelay(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| S3Error::Config(format!("http client: {e}")))?;
        Ok(S3Client {
            inner: Arc::new(Inner {
                sem: Arc::new(Semaphore::new(cfg.max_in_flight)),
                budget: RetryBudget::new(cfg.retry.budget_tokens),
                stats: S3Stats::default(),
                scheme: url.scheme().to_string(),
                host,
                bucket_path,
                prefix,
                http,
                creds,
                cfg,
            }),
        })
    }

    pub fn config(&self) -> &S3Config {
        &self.inner.cfg
    }

    pub fn stats(&self) -> &S3Stats {
        &self.inner.stats
    }

    /// `s3://bucket/prefix` (plus endpoint), for logs.
    pub fn describe(&self) -> String {
        format!("s3://{}/{} ({})", self.inner.cfg.bucket, self.inner.prefix, self.inner.cfg.endpoint)
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}{key}", self.inner.prefix)
    }

    // -----------------------------------------------------------------------
    // Request plumbing
    // -----------------------------------------------------------------------

    /// Sends one attempt. The returned permit bounds concurrency and must be
    /// held until the body is consumed.
    async fn send(&self, req: &Req<'_>) -> S3Result<(reqwest::Response, OwnedSemaphorePermit)> {
        let inner = &*self.inner;
        let permit = inner.sem.clone().acquire_owned().await.map_err(|_| S3Error::Shutdown)?;
        let path = match req.key {
            Some(k) => format!("{}/{}", inner.bucket_path, sigv4::uri_encode(&self.full_key(k), true)),
            None if inner.bucket_path.is_empty() => "/".to_string(),
            None => inner.bucket_path.clone(),
        };
        let creds = inner.creds.credentials()?;
        let signed = sigv4::sign(
            &creds,
            &sigv4::SignInput {
                method: req.method.as_str(),
                canonical_uri: &path,
                query: &req.query,
                host: &inner.host,
                headers: &req.headers,
                payload_sha256: &req.payload_sha256,
                region: &inner.cfg.region,
                time: SystemTime::now(),
            },
        );
        let qs = sigv4::canonical_query(&req.query);
        let url = if qs.is_empty() {
            format!("{}://{}{path}", inner.scheme, inner.host)
        } else {
            format!("{}://{}{path}?{qs}", inner.scheme, inner.host)
        };
        let mut rb = inner.http.request(req.method.clone(), &url);
        for (k, v) in req.headers.iter().chain(signed.iter()) {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let body_len = req.body.as_ref().map_or(0, |b| b.len() as u64);
        if let Some(b) = &req.body {
            rb = rb.body(b.clone());
        } else if matches!(req.method, Method::PUT | Method::POST) {
            rb = rb.header("content-length", "0");
        }
        let timeout = req.timeout.unwrap_or(inner.cfg.request_timeout);
        if let Some(t) = req.timeout {
            rb = rb.timeout(t);
        }
        inner.stats.requests.fetch_add(1, Ordering::Relaxed);
        let fut = rb.send();
        let resp = match tokio::time::timeout(timeout, fut).await {
            Err(_) => return Err(S3Error::Timeout(timeout)),
            Ok(Err(e)) => return Err(S3Error::from_reqwest(e, timeout)),
            Ok(Ok(r)) => r,
        };
        inner.stats.bytes_sent.fetch_add(body_len, Ordering::Relaxed);
        Ok((resp, permit))
    }

    /// Turns an error status into `S3Error::Http` (reading at most 64 KiB of body).
    async fn http_error(&self, req: &Req<'_>, resp: reqwest::Response) -> S3Error {
        let status = resp.status().as_u16();
        let body = read_limited(resp, 64 * 1024).await.unwrap_or_default();
        let (code, message) = xml::parse_error(&body).unwrap_or_else(|| {
            let code = match status {
                404 => "NoSuchKey",
                412 => "PreconditionFailed",
                503 => "ServiceUnavailable",
                _ => "",
            };
            (code.to_string(), String::from_utf8_lossy(&body[..body.len().min(512)]).into_owned())
        });
        let key = req.key.unwrap_or("").to_string();
        if status == 404 && code == "NoSuchKey" {
            return S3Error::NotFound(key);
        }
        S3Error::Http { method: req.op, key, status, code, message }
    }

    /// Runs `f(attempt)` until it succeeds, fails permanently, or the policy gives up.
    async fn retry<T, F, Fut>(&self, op: &'static str, key: &str, mut f: F) -> S3Result<T>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = S3Result<T>>,
    {
        let rc = &self.inner.cfg.retry;
        let mut attempt = 0u32;
        loop {
            match f(attempt).await {
                Ok(v) => {
                    self.inner.budget.on_success();
                    return Ok(v);
                }
                Err(e) => {
                    if e.is_throttle() {
                        self.inner.stats.throttled.fetch_add(1, Ordering::Relaxed);
                    }
                    let can_retry = e.is_transient()
                        && attempt + 1 < rc.max_attempts
                        && self.inner.budget.try_acquire(rc.retry_cost);
                    if !can_retry {
                        self.inner.stats.failures.fetch_add(1, Ordering::Relaxed);
                        if attempt == 0 || !e.is_transient() {
                            return Err(e);
                        }
                        return Err(S3Error::RetriesExhausted { attempts: attempt + 1, last: Box::new(e) });
                    }
                    let d = backoff(rc, attempt, &e);
                    warn!(op, key, attempt, delay_ms = d.as_millis() as u64, error = %e, "s3 retry");
                    self.inner.stats.retries.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(d).await;
                    attempt += 1;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Operations
    // -----------------------------------------------------------------------

    /// `HEAD`: `None` if the key does not exist.
    pub async fn head(&self, key: &str) -> S3Result<Option<ObjectMeta>> {
        validate_key(key)?;
        self.retry("HEAD", key, |_| async {
            let req = Req::new(Method::HEAD, "HEAD", Some(key));
            let (resp, _p) = self.send(&req).await?;
            match resp.status() {
                s if s.is_success() => {
                    let len = resp
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| S3Error::Protocol("HEAD without Content-Length".into()))?;
                    let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
                    Ok(Some(ObjectMeta { len, etag }))
                }
                StatusCode::NOT_FOUND => Ok(None),
                _ => Err(self.http_error(&req, resp).await),
            }
        })
        .await
    }

    /// Reads `range` of `key`. Reading past the end is [`S3Error::ShortRead`].
    pub async fn get_range(&self, key: &str, range: Range<u64>) -> S3Result<Bytes> {
        validate_key(key)?;
        if range.start > range.end {
            return Err(S3Error::Protocol(format!("bad range {range:?}")));
        }
        let want = range.end - range.start;
        if want == 0 {
            // S3 cannot express an empty range; check existence and bounds instead.
            let meta = self.head(key).await?.ok_or_else(|| S3Error::NotFound(key.into()))?;
            if range.start > meta.len {
                return Err(S3Error::ShortRead { key: key.into(), offset: range.start, len: 0, available: meta.len });
            }
            return Ok(Bytes::new());
        }
        self.retry("GET", key, |_| async {
            let mut req =
                Req::new(Method::GET, "GET", Some(key)).h("range", format!("bytes={}-{}", range.start, range.end - 1));
            req.timeout = Some(self.inner.cfg.timeout_for(want));
            let (resp, _p) = self.send(&req).await?;
            let status = resp.status();
            if status == StatusCode::RANGE_NOT_SATISFIABLE {
                let available = content_range_total(&resp).unwrap_or(0);
                return Err(S3Error::ShortRead { key: key.into(), offset: range.start, len: want, available });
            }
            if !status.is_success() {
                return Err(self.http_error(&req, resp).await);
            }
            let total = content_range_total(&resp);
            let partial = status == StatusCode::PARTIAL_CONTENT;
            let body = self.read_body(resp, req.timeout).await?;
            let data = if partial {
                body
            } else {
                // Server ignored Range and sent the whole object.
                debug!(key, "server ignored Range header");
                let end = (range.end as usize).min(body.len());
                let start = (range.start as usize).min(end);
                body.slice(start..end)
            };
            if data.len() as u64 != want {
                let available = total.unwrap_or(range.start + data.len() as u64);
                if available >= range.end {
                    // Server promised more than it sent: truncated body.
                    return Err(S3Error::Transport(format!("truncated body: got {} of {want} bytes", data.len())));
                }
                return Err(S3Error::ShortRead { key: key.into(), offset: range.start, len: want, available });
            }
            Ok(data)
        })
        .await
    }

    /// Reads a whole (moderately sized) object into memory.
    pub async fn get(&self, key: &str) -> S3Result<Bytes> {
        validate_key(key)?;
        self.retry("GET", key, |_| async {
            let mut req = Req::new(Method::GET, "GET", Some(key));
            req.timeout = None;
            let (resp, _p) = self.send(&req).await?;
            if !resp.status().is_success() {
                return Err(self.http_error(&req, resp).await);
            }
            let expect = resp.content_length();
            let body = self.read_body(resp, None).await?;
            if let Some(n) = expect
                && n != body.len() as u64
            {
                return Err(S3Error::Transport(format!("truncated body: got {} of {n} bytes", body.len())));
            }
            Ok(body)
        })
        .await
    }

    /// Starts a streaming GET at `offset`. Used by [`crate::ObjectStream`];
    /// `if_match` pins the object version across resumed reads.
    pub(crate) async fn get_stream_from(
        &self,
        key: &str,
        offset: u64,
        if_match: Option<&str>,
    ) -> S3Result<(reqwest::Response, OwnedSemaphorePermit)> {
        let mut req = Req::new(Method::GET, "GET", Some(key));
        if offset > 0 {
            req = req.h("range", format!("bytes={offset}-"));
        }
        if let Some(e) = if_match {
            req = req.h("if-match", e);
        }
        let (resp, p) = self.send(&req).await?;
        if !resp.status().is_success() {
            return Err(self.http_error(&req, resp).await);
        }
        Ok((resp, p))
    }

    pub(crate) fn note_received(&self, n: usize) {
        self.inner.stats.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
    }

    async fn read_body(&self, resp: reqwest::Response, timeout: Option<Duration>) -> S3Result<Bytes> {
        let t = timeout.unwrap_or(self.inner.cfg.request_timeout);
        let b = resp.bytes().await.map_err(|e| S3Error::from_reqwest(e, t))?;
        self.note_received(b.len());
        Ok(b)
    }

    /// Lists `(key, size)` under logical `prefix`, sorted by key.
    pub async fn list(&self, prefix: &str) -> S3Result<Vec<(String, u64)>> {
        let full_prefix = self.full_key(prefix);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .retry("LIST", prefix, |_| {
                    let token = token.clone();
                    let full_prefix = full_prefix.clone();
                    async move {
                        let mut req = Req::new(Method::GET, "LIST", None)
                            .q("list-type", "2")
                            .q("encoding-type", "url")
                            .q("prefix", full_prefix);
                        if let Some(t) = token {
                            req = req.q("continuation-token", t);
                        }
                        let (resp, _p) = self.send(&req).await?;
                        if !resp.status().is_success() {
                            return Err(self.http_error(&req, resp).await);
                        }
                        let body = self.read_body(resp, None).await?;
                        xml::parse_list(&body)
                    }
                })
                .await?;
            for (k, size) in page.objects {
                match k.strip_prefix(&self.inner.prefix) {
                    Some(logical) if logical.starts_with(prefix) => out.push((logical.to_string(), size)),
                    _ => warn!(key = k, "listing returned a key outside the requested prefix"),
                }
            }
            match page.next_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        Ok(out)
    }

    /// Creates the configured bucket (admin/test helper). An existing bucket owned
    /// by the caller is not an error.
    pub async fn create_bucket(&self) -> S3Result<()> {
        self.retry("CreateBucket", "", |_| async {
            let req = Req::new(Method::PUT, "CreateBucket", None);
            let (resp, _p) = self.send(&req).await?;
            if resp.status().is_success() {
                return Ok(());
            }
            match self.http_error(&req, resp).await {
                S3Error::Http { code, .. } if code == "BucketAlreadyOwnedByYou" => Ok(()),
                e => Err(e),
            }
        })
        .await
    }

    /// Deletes `key`; deleting a missing key succeeds.
    pub async fn delete(&self, key: &str) -> S3Result<()> {
        validate_key(key)?;
        self.retry("DELETE", key, |_| async {
            let req = Req::new(Method::DELETE, "DELETE", Some(key));
            let (resp, _p) = self.send(&req).await?;
            if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                Ok(())
            } else {
                Err(self.http_error(&req, resp).await)
            }
        })
        .await
    }

    /// Creates `key` unless it exists. Uses multipart upload for bodies at
    /// least `multipart_threshold` long.
    pub async fn put_if_absent(&self, key: &str, body: Body) -> S3Result<Created> {
        validate_key(key)?;
        let len = match &body {
            Body::Bytes(b) => b.len() as u64,
            Body::File(p) => tokio::fs::metadata(p).await?.len(),
        };
        if len >= self.inner.cfg.multipart_threshold {
            return self.put_multipart(key, body, len).await;
        }
        let data = match body {
            Body::Bytes(b) => b,
            Body::File(p) => Bytes::from(tokio::fs::read(&p).await?),
        };
        if data.len() as u64 != len {
            return Err(S3Error::LocalIo(std::io::Error::other("source file changed size during upload")));
        }
        self.put_single(key, data).await
    }

    async fn put_single(&self, key: &str, data: Bytes) -> S3Result<Created> {
        let conditional = self.inner.cfg.conditional_writes;
        if !conditional && self.head(key).await?.is_some() {
            return Ok(Created::AlreadyExists);
        }
        let sha = hash_blocking(data.clone(), sigv4::sha256_hex).await?;
        let md5_cell: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let md5_cell = &md5_cell;
        self.retry("PUT", key, |attempt| {
            let data = data.clone();
            let sha = sha.clone();
            async move {
                let mut req = Req::new(Method::PUT, "PUT", Some(key));
                if conditional {
                    req = req.h("if-none-match", "*");
                }
                req.timeout = Some(self.inner.cfg.timeout_for(data.len() as u64));
                req.payload_sha256 = sha;
                req.body = Some(data.clone());
                let (resp, _p) = self.send(&req).await?;
                match resp.status() {
                    s if s.is_success() => Ok(Created::Created),
                    StatusCode::PRECONDITION_FAILED if attempt == 0 => Ok(Created::AlreadyExists),
                    StatusCode::PRECONDITION_FAILED => {
                        // An earlier attempt may have succeeded without us seeing the
                        // response. If the stored object is ours, report Created.
                        if md5_cell.get().is_none() {
                            let h = hash_blocking(data, |d| sigv4::hex(&Md5::digest(d))).await?;
                            let _ = md5_cell.set(h);
                        }
                        let ours = format!("\"{}\"", md5_cell.get().map(String::as_str).unwrap_or_default());
                        let meta = self.head(key).await?;
                        Ok(if meta.and_then(|m| m.etag).is_some_and(|e| e == ours) {
                            Created::Created
                        } else {
                            Created::AlreadyExists
                        })
                    }
                    _ => Err(self.http_error(&req, resp).await),
                }
            }
        })
        .await
    }

    async fn put_multipart(&self, key: &str, body: Body, len: u64) -> S3Result<Created> {
        let conditional = self.inner.cfg.conditional_writes;
        // Skip the upload entirely if the (content-addressed) key already exists.
        if self.head(key).await?.is_some() {
            return Ok(Created::AlreadyExists);
        }
        let upload_id = self
            .retry("CreateMultipartUpload", key, |_| async {
                let req = Req::new(Method::POST, "CreateMultipartUpload", Some(key)).q("uploads", "");
                let (resp, _p) = self.send(&req).await?;
                if !resp.status().is_success() {
                    return Err(self.http_error(&req, resp).await);
                }
                let b = self.read_body(resp, None).await?;
                xml::parse_upload_id(&b)
            })
            .await?;
        debug!(key, upload_id, len, "multipart upload started");
        let res = self.multipart_body(key, &upload_id, &body, len, conditional).await;
        if !matches!(res, Ok(Created::Created)) {
            // Covers both failures and AlreadyExists (412 on complete).
            if let Err(e) = self.abort_multipart(key, &upload_id).await {
                warn!(key, upload_id, error = %e, "failed to abort multipart upload; a lifecycle rule should clean it up");
            }
        }
        res
    }

    async fn multipart_body(
        &self,
        key: &str,
        upload_id: &str,
        body: &Body,
        len: u64,
        conditional: bool,
    ) -> S3Result<Created> {
        let part_size = self.inner.cfg.part_size_for(len);
        let n_parts = len.div_ceil(part_size).max(1);
        let parts: Vec<(u32, String)> = stream::iter(1..=n_parts)
            .map(|n| async move {
                let off = (n - 1) * part_size;
                let plen = part_size.min(len - off);
                let data = read_part(body, off, plen).await?;
                let sha = hash_blocking(data.clone(), sigv4::sha256_hex).await?;
                let etag = self
                    .retry("UploadPart", key, |_| {
                        let data = data.clone();
                        let sha = sha.clone();
                        async move {
                            let mut req = Req::new(Method::PUT, "UploadPart", Some(key))
                                .q("partNumber", n.to_string())
                                .q("uploadId", upload_id);
                            req.timeout = Some(self.inner.cfg.timeout_for(plen));
                            req.payload_sha256 = sha;
                            req.body = Some(data);
                            let (resp, _p) = self.send(&req).await?;
                            if !resp.status().is_success() {
                                return Err(self.http_error(&req, resp).await);
                            }
                            resp.headers()
                                .get("etag")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string)
                                .ok_or_else(|| S3Error::Protocol("UploadPart response without ETag".into()))
                        }
                    })
                    .await?;
                Ok::<_, S3Error>((n as u32, etag))
            })
            .buffered(self.inner.cfg.multipart_concurrency)
            .try_collect()
            .await?;
        let expected_etag = multipart_etag(&parts);
        let xml_body = Bytes::from(xml::complete_body(&parts));
        let sha = sigv4::sha256_hex(&xml_body);
        self.retry("CompleteMultipartUpload", key, |attempt| {
            let xml_body = xml_body.clone();
            let sha = sha.clone();
            let expected_etag = expected_etag.clone();
            async move {
                let mut req = Req::new(Method::POST, "CompleteMultipartUpload", Some(key)).q("uploadId", upload_id);
                if conditional {
                    req = req.h("if-none-match", "*");
                }
                // Completing a large upload can take a while server-side.
                req.timeout = Some(self.inner.cfg.timeout_for(len / 16));
                req.payload_sha256 = sha;
                req.body = Some(xml_body);
                let (resp, _p) = self.send(&req).await?;
                let status = resp.status();
                let lost_earlier_success =
                    |e: &S3Error| attempt > 0 && matches!(e, S3Error::Http { code, .. } if code == "NoSuchUpload");
                if status.is_success() {
                    let b = self.read_body(resp, None).await?;
                    return match xml::parse_complete(&b)? {
                        xml::CompleteResult::Ok => Ok(Created::Created),
                        xml::CompleteResult::Error { code, message } => Err(S3Error::Http {
                            method: "CompleteMultipartUpload",
                            key: key.into(),
                            status: 200,
                            code,
                            message,
                        }),
                    };
                }
                let e = if status == StatusCode::PRECONDITION_FAILED {
                    None
                } else {
                    Some(self.http_error(&req, resp).await)
                };
                match e {
                    Some(e) if !lost_earlier_success(&e) => Err(e),
                    // 412, or NoSuchUpload after a retry: decide by what is stored.
                    _ => {
                        let meta = self.head(key).await?;
                        match meta {
                            Some(m) if m.etag.as_deref() == Some(expected_etag.as_str()) && attempt > 0 => {
                                Ok(Created::Created)
                            }
                            Some(_) => Ok(Created::AlreadyExists),
                            None => Err(S3Error::Protocol("multipart upload vanished without completing".into())),
                        }
                    }
                }
            }
        })
        .await
    }

    async fn abort_multipart(&self, key: &str, upload_id: &str) -> S3Result<()> {
        self.retry("AbortMultipartUpload", key, |_| async {
            let req = Req::new(Method::DELETE, "AbortMultipartUpload", Some(key)).q("uploadId", upload_id);
            let (resp, _p) = self.send(&req).await?;
            if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                Ok(())
            } else {
                Err(self.http_error(&req, resp).await)
            }
        })
        .await
    }
}

/// Runs a hash over `data` on the blocking pool when it is large.
async fn hash_blocking(data: Bytes, f: fn(&[u8]) -> String) -> S3Result<String> {
    if data.len() < 256 * 1024 {
        return Ok(f(&data));
    }
    tokio::task::spawn_blocking(move || f(&data)).await.map_err(|e| S3Error::Protocol(format!("hash task: {e}")))
}

async fn read_part(body: &Body, off: u64, len: u64) -> S3Result<Bytes> {
    match body {
        Body::Bytes(b) => Ok(b.slice(off as usize..(off + len) as usize)),
        Body::File(p) => {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let mut f = tokio::fs::File::open(p).await?;
            f.seek(std::io::SeekFrom::Start(off)).await?;
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf).await?;
            Ok(Bytes::from(buf))
        }
    }
}

/// S3's ETag for a multipart object: `"md5(concat(md5(part_i)))-N"`.
fn multipart_etag(parts: &[(u32, String)]) -> String {
    let mut h = Md5::new();
    for (_, e) in parts {
        let hexs = e.trim_matches('"');
        let mut raw = Vec::with_capacity(16);
        for i in (0..hexs.len()).step_by(2) {
            if let Some(b) = hexs.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()) {
                raw.push(b);
            }
        }
        h.update(&raw);
    }
    format!("\"{}-{}\"", sigv4::hex(&h.finalize()), parts.len())
}

fn content_range_total(resp: &reqwest::Response) -> Option<u64> {
    let v = resp.headers().get("content-range")?.to_str().ok()?;
    v.rsplit('/').next()?.parse().ok()
}

async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while let Ok(Some(c)) = resp.chunk().await {
        let room = limit.saturating_sub(out.len());
        out.extend_from_slice(&c[..c.len().min(room)]);
        if out.len() >= limit {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert!(validate_key("packs/ab.pack").is_ok());
        for k in ["", "/a", "a/", "a/../b", "a b", "a//b", "./x"] {
            assert!(validate_key(k).is_err(), "{k}");
        }
    }

    #[test]
    fn multipart_etag_matches_s3() {
        // Two parts whose MD5s are md5("a") and md5("b").
        let pa = format!("\"{}\"", sigv4::hex(&Md5::digest(b"a")));
        let pb = format!("\"{}\"", sigv4::hex(&Md5::digest(b"b")));
        let mut cat = Md5::digest(b"a").to_vec();
        cat.extend_from_slice(&Md5::digest(b"b"));
        let want = format!("\"{}-2\"", sigv4::hex(&Md5::digest(&cat)));
        assert_eq!(multipart_etag(&[(1, pa), (2, pb)]), want);
    }
}
