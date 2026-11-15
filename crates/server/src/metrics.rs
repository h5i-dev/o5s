//! Prometheus metrics (OpenMetrics text at `GET /metrics`).

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use std::fmt::Write;
use std::sync::Arc;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    pub method: String,
    pub route: String,
    pub status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub route: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct OutcomeLabels {
    pub outcome: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct FrontLabels {
    pub front: String,
}

pub struct Metrics {
    registry: Registry,
    pub http_requests: Family<HttpLabels, Counter>,
    pub http_duration: Family<RouteLabels, Histogram>,
    pub in_flight: Gauge,
    pub bytes_in: Counter,
    pub bytes_out: Counter,
    pub replace: Family<OutcomeLabels, Counter>,
    pub auth_failures: Family<OutcomeLabels, Counter>,
    pub denied: Counter,
    pub audit_failures: Counter,
    pub gc_runs: Family<OutcomeLabels, Counter>,
    pub gc_deleted_bytes: Counter,
    pub rate_limited: Counter,
    /// Files fetched from an upstream by a proxy-cache front (each should be fetched once).
    pub upstream_fetches: Family<FrontLabels, Counter>,
    s3: Option<Arc<dyn Fn() -> pico2peta_backend_s3::S3StatsSnapshot + Send + Sync>>,
}

impl Metrics {
    pub fn new() -> Metrics {
        let mut registry = Registry::with_prefix("p2p");
        let http_requests = Family::<HttpLabels, Counter>::default();
        registry.register("http_requests", "HTTP requests by method, route and status", http_requests.clone());
        let http_duration = Family::<RouteLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.0005, 2.0, 20))
        });
        registry.register(
            "http_request_duration_seconds",
            "HTTP request latency (headers to response)",
            http_duration.clone(),
        );
        let in_flight = Gauge::default();
        registry.register("http_in_flight", "HTTP requests in flight", in_flight.clone());
        let bytes_in = Counter::default();
        registry.register("object_bytes_uploaded", "Object bytes received", bytes_in.clone());
        let bytes_out = Counter::default();
        registry.register("object_bytes_downloaded", "Object bytes sent", bytes_out.clone());
        let replace = Family::<OutcomeLabels, Counter>::default();
        registry.register("replace", "replace outcomes", replace.clone());
        let auth_failures = Family::<OutcomeLabels, Counter>::default();
        registry.register("auth_failures", "authentication failures by reason", auth_failures.clone());
        let denied = Counter::default();
        registry.register("authz_denied", "requests denied by policy", denied.clone());
        let audit_failures = Counter::default();
        registry.register("audit_write_failures", "failed audit log writes", audit_failures.clone());
        let gc_runs = Family::<OutcomeLabels, Counter>::default();
        registry.register("gc_runs", "GC invocations by outcome", gc_runs.clone());
        let gc_deleted_bytes = Counter::default();
        registry.register("gc_deleted_bytes", "bytes deleted by GC", gc_deleted_bytes.clone());
        let rate_limited = Counter::default();
        registry.register("rate_limited", "requests rejected by the per-principal rate limit", rate_limited.clone());
        let upstream_fetches = Family::<FrontLabels, Counter>::default();
        registry.register(
            "upstream_fetches",
            "files fetched from an upstream by a proxy-cache front",
            upstream_fetches.clone(),
        );
        Metrics {
            registry,
            upstream_fetches,
            rate_limited,
            gc_runs,
            gc_deleted_bytes,
            http_requests,
            http_duration,
            in_flight,
            bytes_in,
            bytes_out,
            replace,
            auth_failures,
            denied,
            audit_failures,
            s3: None,
        }
    }

    /// Exports the S3 client counters as well.
    pub fn with_s3(mut self, stats: Arc<dyn Fn() -> pico2peta_backend_s3::S3StatsSnapshot + Send + Sync>) -> Self {
        self.s3 = Some(stats);
        self
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if encode(&mut out, &self.registry).is_err() {
            return String::from("# encoding error\n");
        }
        if let Some(s3) = &self.s3 {
            // Hand-written counters, placed before the trailing `# EOF`.
            let s = s3();
            let body = out.strip_suffix("# EOF\n").unwrap_or(&out).to_string();
            out = body;
            for (name, help, v) in [
                ("p2p_s3_requests", "S3 HTTP requests", s.requests),
                ("p2p_s3_retries", "S3 request retries", s.retries),
                ("p2p_s3_throttled", "S3 throttling responses", s.throttled),
                ("p2p_s3_failures", "S3 requests that failed after retries", s.failures),
                ("p2p_s3_bytes_sent", "S3 bytes sent", s.bytes_sent),
                ("p2p_s3_bytes_received", "S3 bytes received", s.bytes_received),
            ] {
                let _ = write!(out, "# HELP {name} {help}.\n# TYPE {name} counter\n{name}_total {v}\n");
            }
            out.push_str("# EOF\n");
        }
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
