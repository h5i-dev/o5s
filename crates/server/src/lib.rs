//! The pico2peta HTTP server.
//!
//! The API is in [`api`], and the wire format is in `pico2peta_client::wire`.
//! [`build_state`] opens the repository (local disk, or S3 for sealed packs) and
//! the audit log, and [`serve`] runs the HTTP server until `shutdown` resolves,
//! then drains in-flight requests and flushes.

pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod error;
pub mod metrics;
pub mod oci;
pub mod segmap;

use crate::api::AppState;
use crate::config::{BackendSection, Config};
use axum::http::{HeaderName, Request};
use pico2peta_core::Repository;
use pico2peta_core::backend::Backend;
use pico2peta_repo::LocalRepo;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),
    #[error("repository: {0}")]
    Repo(#[from] pico2peta_core::Error),
    #[error("audit log {0}: {1}")]
    Audit(std::path::PathBuf, std::io::Error),
    #[error("s3 backend: {0}")]
    S3(String),
}

/// Opens everything the server needs.
pub fn build_state(cfg: &Config) -> Result<Arc<AppState>, StartError> {
    cfg.validate()?;
    let mut metrics = metrics::Metrics::new();
    let backend: Option<Arc<dyn Backend>> = match &cfg.backend {
        BackendSection::Local => None,
        BackendSection::S3(s) => {
            let mut c = pico2peta_backend_s3::S3Config::new(&s.endpoint, &s.region, &s.bucket).with_prefix(&s.prefix);
            c.path_style = s.path_style;
            c.conditional_writes = s.conditional_writes;
            if let Some(n) = s.max_in_flight {
                c.max_in_flight = n;
                c.pool_max_idle = n;
                c.multipart_concurrency = c.multipart_concurrency.min(n);
            }
            let creds = pico2peta_backend_s3::Credentials::from_env().map_err(|e| StartError::S3(e.to_string()))?;
            let b = Arc::new(pico2peta_backend_s3::S3Backend::new(c, Arc::new(creds), 2)?);
            let client = b.client().clone();
            metrics = metrics.with_s3(Arc::new(move || client.stats().snapshot()));
            Some(b)
        }
    };
    let path = &cfg.repo.path;
    if !path.join("config.toml").exists() {
        if !cfg.repo.create {
            return Err(StartError::Repo(pico2peta_core::Error::Parse(format!(
                "{} is not a repository (set repo.create = true to initialize it)",
                path.display()
            ))));
        }
        info!(path = %path.display(), "initializing repository");
        drop(LocalRepo::init(path, &pico2peta_pack::PackConfig::default())?);
    }
    let local = match backend {
        None => LocalRepo::open(path)?,
        Some(b) => LocalRepo::open_with_backend(path, b)?,
    };
    let repo: Arc<dyn Repository> = Arc::new(local.clone());
    let oci = if cfg.oci.enabled {
        let dir = cfg.oci.upload_dir.clone().unwrap_or_else(|| {
            let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.clone());
            parent.join(format!("{}.oci-uploads", path.file_name().and_then(|n| n.to_str()).unwrap_or("repo")))
        });
        let st = oci::OciState::new(repo.clone(), dir.clone(), cfg.oci.max_upload_sessions)
            .map_err(|e| StartError::Audit(dir, e))?;
        Some(Arc::new(st))
    } else {
        None
    };
    let audit = audit::AuditLog::open(&cfg.audit.path).map_err(|e| StartError::Audit(cfg.audit.path.clone(), e))?;
    Ok(Arc::new(AppState {
        repo,
        authn: Arc::new(auth::TokenAuthenticator::new(&cfg.auth.tokens, cfg.auth.allow_anonymous)),
        authz: auth::Authorizer::new(cfg.policy.clone()),
        audit,
        log_reads: cfg.audit.log_reads,
        metrics: Arc::new(metrics),
        max_upload_bytes: cfg.server.max_upload_bytes,
        shutting_down: Arc::new(AtomicBool::new(false)),
        limiter: Arc::new(tokio::sync::Semaphore::new(cfg.server.max_concurrent_requests)),
        trust_forwarded_for: cfg.server.trust_forwarded_for,
        local: Some(local),
        gc: cfg.gc.clone(),
        gc_running: Arc::new(AtomicBool::new(false)),
        oci,
    }))
}

#[derive(Clone, Default)]
struct RandomRequestId;

impl MakeRequestId for RandomRequestId {
    fn make_request_id<B>(&mut self, _: &Request<B>) -> Option<RequestId> {
        let id = format!("{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..));
        axum::http::HeaderValue::from_str(&id).ok().map(RequestId::new)
    }
}

/// The full service: API router plus request-id, tracing, panic and timeout layers.
pub fn app(state: Arc<AppState>, request_timeout: Duration) -> axum::Router {
    use tower_http::catch_panic::CatchPanicLayer;
    use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
    use tower_http::timeout::TimeoutLayer;
    use tower_http::trace::TraceLayer;
    let rid = HeaderName::from_static(pico2peta_client::wire::REQUEST_ID_HEADER);
    let trace = TraceLayer::new_for_http()
        .on_response(
            tower_http::trace::DefaultOnResponse::new()
                .level(tracing::Level::INFO)
                .latency_unit(tower_http::LatencyUnit::Micros),
        )
        .on_failure(tower_http::trace::DefaultOnFailure::new().level(tracing::Level::WARN))
        .make_span_with(|req: &Request<axum::body::Body>| {
            let id = req.headers().get("x-request-id").and_then(|v| v.to_str().ok()).unwrap_or("-");
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path(), request_id = %id)
        });
    api::router(state)
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, request_timeout))
        .layer(CatchPanicLayer::new())
        .layer(trace)
        .layer(SetSensitiveRequestHeadersLayer::new([axum::http::header::AUTHORIZATION]))
        .layer(PropagateRequestIdLayer::new(rid.clone()))
        .layer(SetRequestIdLayer::new(rid, RandomRequestId))
}

/// Serves on `listener` until `shutdown` resolves. Then readiness turns to 503,
/// new requests get 503, in-flight requests get `grace` to finish, and the
/// repository and audit log are flushed.
pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    request_timeout: Duration,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = app(state.clone(), request_timeout);
    let gc_task = (state.gc.interval_secs > 0).then(|| tokio::spawn(gc_loop(state.clone())));
    let reaper = state.oci.clone().map(|o| {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                o.reap().await;
            }
        })
    });
    let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
    let st = state.clone();
    let graceful = async move {
        shutdown.await;
        st.shutting_down.store(true, Ordering::Release);
        info!("shutdown requested; draining");
        let _ = stopping_tx.send(());
    };
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(graceful);
    let server = server.into_future();
    tokio::pin!(server);
    tokio::select! {
        r = &mut server => r?,
        _ = async { if stopping_rx.await.is_ok() { tokio::time::sleep(grace).await } else { std::future::pending::<()>().await } } => {
            warn!(?grace, "grace period expired; closing remaining connections");
        }
    }
    if let Some(t) = gc_task {
        t.abort();
    }
    if let Some(t) = reaper {
        t.abort();
    }
    let repo = state.repo.clone();
    match tokio::task::spawn_blocking(move || repo.flush()).await {
        Ok(Ok(())) => info!("repository flushed"),
        Ok(Err(e)) => warn!(error = %e, "flush on shutdown failed"),
        Err(e) => warn!(error = %e, "flush task failed"),
    }
    state.audit.shutdown().await;
    info!("stopped");
    Ok(())
}

/// Runs GC every `gc.interval_secs` (with jitter) until the server stops.
async fn gc_loop(state: Arc<AppState>) {
    let period = Duration::from_secs(state.gc.interval_secs);
    loop {
        let jitter = Duration::from_millis(fastrand::u64(0..=period.as_millis() as u64 / 10));
        tokio::time::sleep(period + jitter).await;
        if state.shutting_down.load(Ordering::Acquire) {
            return;
        }
        match api::run_gc(&state, false).await {
            Ok(r) => info!(
                planned = ?r.planned,
                swept = r.swept.len(),
                condemned_packs = r.condemned_packs,
                deleted_packs = r.deleted_packs,
                deleted_bytes = r.deleted_bytes,
                live = r.live_hashes,
                dangling_refs = r.dangling_refs.len(),
                "gc run"
            ),
            Err(e) => warn!(code = e.code, error = %e.message, "gc run failed"),
        }
        if !state.audit.is_healthy() {
            return;
        }
        state.audit.record_nowait(audit::Event {
            request_id: "-".into(),
            principal: "system".into(),
            action: "gc",
            target: "repository".into(),
            outcome: "scheduled".into(),
            detail: None,
            remote: None,
        });
    }
}

/// Resolves on SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

/// Installs the global tracing subscriber.
pub fn init_logging(cfg: &config::LogSection) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.filter));
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    let _ = match cfg.format {
        config::LogFormat::Json => b.json().with_current_span(true).try_init(),
        config::LogFormat::Text => b.try_init(),
    };
}
