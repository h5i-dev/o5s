//! Mapping of errors to HTTP responses (`ErrorBody` JSON).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pico2peta_client::wire::{ErrorBody, ErrorDetail, codes};
use pico2peta_core::Error;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
    pub www_authenticate: bool,
    pub rule: Option<String>,
    pub subject: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after: None,
            www_authenticate: false,
            rule: None,
            subject: None,
        }
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST, m)
    }
    pub fn denied(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED, m)
    }
    pub fn unauthenticated(m: impl Into<String>) -> Self {
        ApiError { www_authenticate: true, ..Self::new(StatusCode::UNAUTHORIZED, codes::UNAUTHENTICATED, m) }
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, codes::INTERNAL, m)
    }
    pub fn unavailable(code: &'static str, m: impl Into<String>) -> Self {
        ApiError { retry_after: Some(1), ..Self::new(StatusCode::SERVICE_UNAVAILABLE, code, m) }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        // An io::Error may wrap a core::Error (streaming readers); unwrap it.
        let e = match e {
            Error::Io(io) => match io.downcast::<Error>() {
                Ok(inner) => inner,
                Err(io) => Error::Io(io),
            },
            e => e,
        };
        let code = e.code();
        let status = match &e {
            Error::NotFound(_) | Error::KeyNotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidRange { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
            Error::InvalidName(..) | Error::Parse(_) | Error::TargetNotInReadSet(_) => StatusCode::BAD_REQUEST,
            Error::MissingObject(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::Forbidden { .. } | Error::Unauthorized(_) => StatusCode::FORBIDDEN,
            Error::RuleChangeOnNonEmpty(_) => StatusCode::CONFLICT,
            Error::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            Error::Locked => StatusCode::SERVICE_UNAVAILABLE,
            Error::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Error::Backend { transient: true, .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::Backend { .. } | Error::Corrupt(_) | Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(error = %e, code, "request failed");
        }
        let retry_after = (status == StatusCode::SERVICE_UNAVAILABLE).then_some(1);
        let (rule, subject) = match &e {
            Error::Forbidden { name, rule, .. } => (Some(rule.as_str().to_string()), Some(name.to_string())),
            Error::NotFound(h) | Error::MissingObject(h) => (None, Some(h.to_hex())),
            Error::InvalidName(n, _) | Error::RuleChangeOnNonEmpty(n) | Error::KeyNotFound(n) => {
                (None, Some(n.clone()))
            }
            Error::TargetNotInReadSet(n) => (None, Some(n.to_string())),
            _ => (None, None),
        };
        ApiError { status, code, message: e.to_string(), retry_after, www_authenticate: false, rule, subject }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_string(),
                message: self.message,
                rule: self.rule,
                subject: self.subject,
            },
        };
        let mut r = (self.status, Json(body)).into_response();
        if let Some(s) = self.retry_after {
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        if self.www_authenticate {
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"pico2peta\""));
        }
        r
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
