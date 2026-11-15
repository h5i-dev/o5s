//! S3 and S3-compatible (MinIO, Ceph RGW, R2, …) storage backend for pico2peta.
//!
//! * [`S3Client`]: async client with SigV4 signing, create-only writes
//!   (`If-None-Match: *`), multipart upload from memory or a file, ranged and
//!   streaming reads (resumed after mid-body failures), listing, deletes,
//!   retries with jittered exponential backoff and a retry budget, per-request
//!   timeouts and a bounded number of in-flight requests/connections.
//! * [`S3Backend`]: the synchronous `pico2peta_core::backend::Backend`
//!   implementation on top of it.

mod backend;
mod client;
mod config;
pub mod credentials;
mod error;
mod retry;
mod sigv4;
mod xml;

pub use backend::{S3Backend, to_core};
pub use client::{Body, Created, ObjectMeta, S3Client, S3Stats, S3StatsSnapshot, validate_key};
pub use config::{Credentials, CredentialsProvider, MAX_PART_SIZE, MIN_PART_SIZE, RetryConfig, S3Config};
pub use error::{S3Error, S3Result};

/// Entry points for fuzzing and never-panic tests. Not a stable API.
#[doc(hidden)]
pub mod fuzzing {
    /// Runs every XML response parser on `data`.
    pub fn xml(data: &[u8]) {
        let _ = crate::xml::parse_list(data);
        let _ = crate::xml::parse_error(data);
        let _ = crate::xml::parse_upload_id(data);
        let _ = crate::xml::parse_complete(data);
        let _ = crate::xml::find_text(data, "AccessKeyId");
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = crate::credentials::parse_rfc3339(s);
        }
    }
}
