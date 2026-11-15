//! Client configuration.

use crate::error::{S3Error, S3Result};
use std::time::Duration;

/// Static AWS-style credentials.
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// STS session token, if the credentials are temporary.
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print secrets.
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("session_token", &self.session_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Credentials {
    pub fn new(access_key_id: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Credentials {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token: None,
        }
    }

    /// Reads `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optional `AWS_SESSION_TOKEN`.
    pub fn from_env() -> S3Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        match (get("AWS_ACCESS_KEY_ID"), get("AWS_SECRET_ACCESS_KEY")) {
            (Some(a), Some(s)) => {
                Ok(Credentials { access_key_id: a, secret_access_key: s, session_token: get("AWS_SESSION_TOKEN") })
            }
            _ => Err(S3Error::Config("AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set".into())),
        }
    }
}

/// Supplies credentials for each request. Implement this for rotating
/// credentials (instance profile, STS web identity, Vault, …).
pub trait CredentialsProvider: Send + Sync + 'static {
    fn credentials(&self) -> S3Result<Credentials>;
}

impl CredentialsProvider for Credentials {
    fn credentials(&self) -> S3Result<Credentials> {
        Ok(self.clone())
    }
}

/// Retry policy: capped exponential backoff with full jitter
/// (`sleep = uniform(0, min(cap, base * 2^attempt))`), plus a shared retry
/// budget so that a struggling service is not hit by a retry storm.
#[derive(Clone, Debug)]
pub struct RetryConfig {
    /// Total attempts per request, including the first. `1` disables retries.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Base delay used for throttling responses (`SlowDown`, 429, 503).
    pub throttle_base_delay: Duration,
    /// Retry budget tokens; each retry costs [`Self::retry_cost`] tokens and each
    /// success refunds one. When exhausted, failures are returned without retrying.
    pub budget_tokens: u32,
    pub retry_cost: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            max_attempts: 8,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(10),
            throttle_base_delay: Duration::from_millis(500),
            budget_tokens: 500,
            retry_cost: 5,
        }
    }
}

/// Configuration of an [`crate::S3Client`].
#[derive(Clone, Debug)]
pub struct S3Config {
    /// e.g. `https://s3.us-east-1.amazonaws.com` or `http://127.0.0.1:9000`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// Key prefix inside the bucket (no leading `/`; a trailing `/` is added if missing).
    pub prefix: String,
    /// `true`: `endpoint/bucket/key` (MinIO, most S3-compatibles).
    /// `false`: `bucket.endpoint/key` (AWS virtual-hosted style).
    pub path_style: bool,
    /// Use `If-None-Match: *` for create-only writes. Disable only for stores that
    /// reject the header; the fallback is HEAD-then-PUT, which is safe only because
    /// keys are content-derived (racing writers write identical bytes).
    pub conditional_writes: bool,
    /// Objects at least this large are uploaded with multipart upload.
    pub multipart_threshold: u64,
    /// Multipart part size (≥ 5 MiB). Grown automatically to stay under 10,000 parts.
    pub part_size: u64,
    /// Parts of one multipart upload in flight at once.
    pub multipart_concurrency: usize,
    /// Upper bound on concurrent HTTP requests (and thus open connections) per client.
    pub max_in_flight: usize,
    pub connect_timeout: Duration,
    /// Timeout for a request without a large body.
    pub request_timeout: Duration,
    /// Large transfers get `request_timeout + bytes / min_throughput`.
    pub min_throughput_bytes_per_sec: u64,
    /// Idle connections kept for reuse (0 disables connection reuse).
    pub pool_max_idle: usize,
    /// Idle connections are closed after this long.
    pub pool_idle_timeout: Duration,
    pub retry: RetryConfig,
}

pub const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;
pub const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;
pub const MAX_PARTS: u64 = 10_000;

impl S3Config {
    /// Defaults for `bucket` at `endpoint`; MinIO-friendly (path style).
    pub fn new(endpoint: impl Into<String>, region: impl Into<String>, bucket: impl Into<String>) -> Self {
        S3Config {
            endpoint: endpoint.into(),
            region: region.into(),
            bucket: bucket.into(),
            prefix: String::new(),
            path_style: true,
            conditional_writes: true,
            multipart_threshold: 64 * 1024 * 1024,
            part_size: 16 * 1024 * 1024,
            multipart_concurrency: 4,
            max_in_flight: 32,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            min_throughput_bytes_per_sec: 1024 * 1024,
            pool_max_idle: 32,
            pool_idle_timeout: Duration::from_secs(60),
            retry: RetryConfig::default(),
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Checks invariants; called by [`crate::S3Client::new`].
    pub fn validate(&self) -> S3Result<()> {
        let bad = |m: String| Err(S3Error::Config(m));
        if !(self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://")) {
            return bad(format!("endpoint must start with http:// or https://, got {:?}", self.endpoint));
        }
        if self.bucket.is_empty()
            || !self.bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return bad(format!("invalid bucket name {:?}", self.bucket));
        }
        if !self.path_style && self.bucket.contains('.') && self.endpoint.starts_with("https://") {
            return bad("virtual-hosted style with a dotted bucket breaks TLS; set path_style".into());
        }
        if self.region.is_empty() {
            return bad("region must not be empty".into());
        }
        if self.prefix.starts_with('/') {
            return bad("prefix must not start with '/'".into());
        }
        if self.part_size < MIN_PART_SIZE || self.part_size > MAX_PART_SIZE {
            return bad(format!("part_size must be in [{MIN_PART_SIZE}, {MAX_PART_SIZE}]"));
        }
        if self.multipart_threshold < self.part_size {
            return bad("multipart_threshold must be >= part_size".into());
        }
        if self.multipart_concurrency == 0 || self.max_in_flight == 0 {
            return bad("concurrency limits must be >= 1".into());
        }
        if self.multipart_concurrency > self.max_in_flight {
            return bad("multipart_concurrency must be <= max_in_flight".into());
        }
        if self.retry.max_attempts == 0 {
            return bad("retry.max_attempts must be >= 1".into());
        }
        if self.min_throughput_bytes_per_sec == 0 {
            return bad("min_throughput_bytes_per_sec must be > 0".into());
        }
        Ok(())
    }

    /// Normalized prefix: empty, or ending in `/`.
    pub(crate) fn norm_prefix(&self) -> String {
        if self.prefix.is_empty() || self.prefix.ends_with('/') {
            self.prefix.clone()
        } else {
            format!("{}/", self.prefix)
        }
    }

    /// Part size for an object of `len` bytes (keeps the part count ≤ 10,000).
    pub(crate) fn part_size_for(&self, len: u64) -> u64 {
        let min_needed = len.div_ceil(MAX_PARTS);
        self.part_size.max(min_needed.next_multiple_of(1024 * 1024)).min(MAX_PART_SIZE)
    }

    pub(crate) fn timeout_for(&self, body_bytes: u64) -> std::time::Duration {
        self.request_timeout + Duration::from_secs(body_bytes / self.min_throughput_bytes_per_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        S3Config::new("http://127.0.0.1:9000", "us-east-1", "b").validate().unwrap();
    }

    #[test]
    fn rejects_bad() {
        assert!(S3Config::new("127.0.0.1:9000", "r", "b").validate().is_err());
        assert!(S3Config::new("http://x", "r", "Bad_Bucket").validate().is_err());
        let mut c = S3Config::new("http://x", "r", "b");
        c.part_size = 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn part_size_grows_for_huge_objects() {
        let c = S3Config::new("http://x", "r", "b");
        assert_eq!(c.part_size_for(100), c.part_size);
        let huge = 5u64 << 40; // 5 TiB
        let p = c.part_size_for(huge);
        assert!(huge.div_ceil(p) <= MAX_PARTS);
    }

    #[test]
    fn debug_redacts_secret() {
        let s = format!("{:?}", Credentials::new("AK", "SECRET"));
        assert!(!s.contains("SECRET"));
    }
}
