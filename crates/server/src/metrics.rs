//! Request IDs, one structured log line per request, and Prometheus metrics.
//! Metrics carry route templates and status codes only, never tenant data.
use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

/// Upper bounds of the latency histogram, in seconds.
const BUCKETS: [f64; 10] = [0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0];

#[derive(Default)]
struct Series {
    count: u64,
    seconds: f64,
    buckets: [u64; BUCKETS.len()],
}

#[derive(Default)]
pub struct Metrics {
    next_id: AtomicU64,
    requests: Mutex<BTreeMap<(String, String, u16), Series>>,
}

impl Metrics {
    fn record(&self, method: &str, route: &str, status: u16, seconds: f64) {
        let mut requests = self.requests.lock().unwrap();
        let s = requests
            .entry((method.into(), route.into(), status))
            .or_default();
        s.count += 1;
        s.seconds += seconds;
        for (i, bound) in BUCKETS.iter().enumerate() {
            if seconds <= *bound {
                s.buckets[i] += 1;
            }
        }
    }

    /// Prometheus text exposition, with extra pre-rendered lines appended.
    pub fn render(&self, extra: &str) -> String {
        let mut out = String::from(
            "# HELP o5s_http_requests_total HTTP requests by route and status.\n# TYPE o5s_http_requests_total counter\n",
        );
        let requests = self.requests.lock().unwrap();
        for ((method, route, status), s) in requests.iter() {
            let _ = writeln!(
                out,
                "o5s_http_requests_total{{method=\"{method}\",route=\"{route}\",status=\"{status}\"}} {}",
                s.count
            );
        }
        out.push_str("# HELP o5s_http_request_seconds Request latency.\n# TYPE o5s_http_request_seconds histogram\n");
        for ((method, route, status), s) in requests.iter() {
            let labels = format!("method=\"{method}\",route=\"{route}\",status=\"{status}\"");
            for (i, bound) in BUCKETS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "o5s_http_request_seconds_bucket{{{labels},le=\"{bound}\"}} {}",
                    s.buckets[i]
                );
            }
            let _ = writeln!(
                out,
                "o5s_http_request_seconds_bucket{{{labels},le=\"+Inf\"}} {}",
                s.count
            );
            let _ = writeln!(
                out,
                "o5s_http_request_seconds_sum{{{labels}}} {}",
                s.seconds
            );
            let _ = writeln!(
                out,
                "o5s_http_request_seconds_count{{{labels}}} {}",
                s.count
            );
        }
        out.push_str(extra);
        out
    }
}

/// Reuse a client's `x-request-id` (visible ASCII, at most 128 bytes) or mint one.
fn request_id(metrics: &Metrics, request: &Request) -> String {
    request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128 && v.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let n = metrics.next_id.fetch_add(1, Ordering::Relaxed);
            format!("{:x}-{n:x}", std::process::id())
        })
}

pub async fn observe(
    State(state): State<crate::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let id = request_id(&state.metrics, &request);
    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let mut response = next.run(request).await.into_response();
    let seconds = started.elapsed().as_secs_f64();
    let status = response.status().as_u16();
    state.metrics.record(&method, &route, status, seconds);
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }
    if state.log_requests {
        let line = serde_json::json!({
            "ts_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            "request_id": id, "method": method, "route": route, "status": status,
            "ms": (seconds * 1000.0 * 1000.0).round() / 1000.0,
        });
        eprintln!("{line}");
    }
    response
}

pub async fn endpoint(State(state): State<crate::AppState>) -> Response {
    let extra = state.backend.metrics();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(&extra),
    )
        .into_response()
}
