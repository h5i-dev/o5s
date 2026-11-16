//! Error type of the S3 client.

use std::time::Duration;

/// Errors returned by [`crate::S3Client`].
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    /// The key does not exist (HTTP 404 `NoSuchKey`, or a HEAD 404).
    #[error("s3 key not found: {0}")]
    NotFound(String),
    /// The key is not a valid backend key (empty segment, `.`/`..`, control chars, …).
    #[error("invalid s3 key {0:?}: {1}")]
    InvalidKey(String, &'static str),
    /// The requested range lies (partly) beyond the end of the object.
    #[error("short read of {key}: wanted {offset}+{len}, object has {available} bytes")]
    ShortRead { key: String, offset: u64, len: u64, available: u64 },
    /// The service answered with an error status.
    #[error("s3 {method} {key}: HTTP {status} {code}: {message}")]
    Http {
        method: &'static str,
        key: String,
        status: u16,
        /// S3 error code from the XML body (`SlowDown`, `AccessDenied`, …), or empty.
        code: String,
        message: String,
    },
    /// Connection refused/reset, TLS failure, truncated body, … (always transient).
    #[error("s3 transport error: {0}")]
    Transport(String),
    /// A request did not complete within its timeout.
    #[error("s3 request timed out after {0:?}")]
    Timeout(Duration),
    /// The service returned something we cannot interpret.
    #[error("s3 protocol error: {0}")]
    Protocol(String),
    /// Bad client configuration (missing credentials, malformed endpoint, …).
    #[error("s3 config error: {0}")]
    Config(String),
    /// Local I/O (reading a file source for upload).
    #[error("local io: {0}")]
    LocalIo(#[from] std::io::Error),
    /// The retry policy gave up. `last` is the final attempt's error.
    #[error("s3 gave up after {attempts} attempts: {last}")]
    RetriesExhausted { attempts: u32, last: Box<S3Error> },
    /// The client's runtime has shut down.
    #[error("s3 client is shut down")]
    Shutdown,
}

impl S3Error {
    /// Whether retrying the same request may succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            S3Error::Transport(_) | S3Error::Timeout(_) => true,
            S3Error::Http { status, code, .. } => is_transient_status(*status, code),
            S3Error::RetriesExhausted { last, .. } => last.is_transient(),
            _ => false,
        }
    }

    /// Whether this is a throttling response (`SlowDown`, 429, 503).
    pub fn is_throttle(&self) -> bool {
        match self {
            S3Error::Http { status, code, .. } => {
                *status == 429
                    || *status == 503
                    || matches!(
                        code.as_str(),
                        "SlowDown" | "Throttling" | "ThrottlingException" | "RequestLimitExceeded" | "TooManyRequests"
                    )
            }
            _ => false,
        }
    }

    pub(crate) fn from_reqwest(e: reqwest::Error, timeout: Duration) -> S3Error {
        if e.is_timeout() {
            S3Error::Timeout(timeout)
        } else {
            // Walk the source chain so that "connection reset" etc. is visible in logs.
            let mut msg = e.to_string();
            let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
            while let Some(s) = src {
                msg.push_str(": ");
                msg.push_str(&s.to_string());
                src = s.source();
            }
            S3Error::Transport(msg)
        }
    }
}

/// HTTP statuses / S3 error codes that are worth retrying.
pub(crate) fn is_transient_status(status: u16, code: &str) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
        || matches!(
            code,
            "RequestTimeout"
                | "RequestTimeTooSkewed"
                | "InternalError"
                | "SlowDown"
                | "ServiceUnavailable"
                | "Throttling"
                | "ThrottlingException"
                | "RequestLimitExceeded"
                | "TooManyRequests"
                | "OperationAborted"
                // AWS: a concurrent conditional write on the same key is in flight.
                | "ConditionalRequestConflict"
                | "XMinioServerNotInitialized"
        )
        || (status == 409 && code == "ConditionalRequestConflict")
}

impl From<S3Error> for std::io::Error {
    fn from(e: S3Error) -> std::io::Error {
        use std::io::ErrorKind as K;
        let kind = match &e {
            S3Error::NotFound(_) => K::NotFound,
            S3Error::InvalidKey(..) => K::InvalidInput,
            S3Error::ShortRead { .. } => K::UnexpectedEof,
            S3Error::Timeout(_) => K::TimedOut,
            S3Error::Transport(_) => K::ConnectionAborted,
            S3Error::Protocol(_) => K::InvalidData,
            S3Error::LocalIo(io) => io.kind(),
            S3Error::Http { status: 403, .. } => K::PermissionDenied,
            _ => K::Other,
        };
        std::io::Error::new(kind, e)
    }
}

pub type S3Result<T> = std::result::Result<T, S3Error>;
