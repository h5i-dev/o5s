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
pub mod git;
pub mod metrics;
pub mod oci;
pub mod oidc;
pub mod segmap;
pub mod tls;

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
            // Env keys, then web identity (EKS/OIDC), then the EC2 instance profile.
            let b = Arc::new(pico2peta_backend_s3::S3Backend::with_default_chain(c, 2)?);
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
        info!(path = %path.display(), refs = ?cfg.repo.refs, "initializing repository");
        drop(LocalRepo::init_with(path, &pico2peta_pack::PackConfig::default(), cfg.repo.refs)?);
    }
    let existing = LocalRepo::read_config(path)?;
    if existing.refs.store != cfg.repo.refs {
        return Err(StartError::Repo(pico2peta_core::Error::Parse(format!(
            "repo.refs = {:?} in the server config, but {} was created with refs = {:?}",
            cfg.repo.refs,
            path.display(),
            existing.refs.store
        ))));
    }
    if existing.refs.store == pico2peta_repo::RefsStore::Backend && matches!(cfg.backend, BackendSection::Local) {
        warn!("refs on the backend with a local backend: only processes on this host can share the repository");
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
    let git = if cfg.git.enabled {
        let dir = cfg.git.spool_dir.clone().unwrap_or_else(|| {
            let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.clone());
            parent.join(format!("{}.git-spool", path.file_name().and_then(|n| n.to_str()).unwrap_or("repo")))
        });
        let st = git::GitState::new(repo.clone(), dir.clone(), cfg.server.max_upload_bytes)
            .map_err(|e| StartError::Audit(dir, e))?;
        Some(Arc::new(st))
    } else {
        None
    };
    let tokens = Arc::new(auth::TokenAuthenticator::new(&cfg.auth.tokens, cfg.auth.allow_anonymous));
    let audit = audit::AuditLog::open_with(&cfg.audit.path, cfg.audit.rotate_bytes)
        .map_err(|e| StartError::Audit(cfg.audit.path.clone(), e))?;
    Ok(Arc::new(AppState {
        repo,
        authn: match &cfg.auth.oidc {
            Some(o) => Arc::new(oidc::ChainAuthenticator {
                tokens: tokens.clone(),
                oidc: Arc::new(oidc::OidcAuthenticator::new(o.clone()).map_err(StartError::S3)?),
            }),
            None => tokens.clone(),
        },
        tokens: Some(tokens),
        authz: auth::Authorizer::new(cfg.policy.clone()),
        audit,
        log_reads: cfg.audit.log_reads,
        metrics: Arc::new(metrics),
        max_upload_bytes: cfg.server.max_upload_bytes,
        shutting_down: Arc::new(AtomicBool::new(false)),
        limiter: Arc::new(tokio::sync::Semaphore::new(cfg.server.max_concurrent_requests)),
        trust_forwarded_for: cfg.server.trust_forwarded_for,
        limits: cfg.server.rate_limit.map(api::RateLimiter::new),
        metrics_separate: cfg.server.metrics_listen.is_some(),
        local: Some(local),
        gc: cfg.gc.clone(),
        gc_running: Arc::new(AtomicBool::new(false)),
        token_admin: match cfg.auth.token_file.clone() {
            Some(file) => {
                let mut ta = api::TokenAdmin {
                    file,
                    inline: Vec::new(),
                    allow_anonymous: cfg.auth.allow_anonymous,
                    lock: tokio::sync::Mutex::new(()),
                };
                // Inline = configured tokens that are not in the token file.
                let in_file = ta.read_hashes().map_err(StartError::S3)?;
                ta.inline = cfg
                    .auth
                    .tokens
                    .iter()
                    .filter(|t| !in_file.contains(&t.sha256.to_ascii_lowercase()))
                    .cloned()
                    .collect();
                Some(ta)
            }
            None => None,
        },
        oci,
        git,
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

/// Serves the ops router (`/metrics`, `/healthz`, `/readyz`) on `listener`
/// until `shutdown` resolves.
pub async fn serve_ops(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, api::ops_router(state)).with_graceful_shutdown(shutdown).await
}

/// Serves plain HTTP on `listener` until `shutdown` resolves. See [`serve_on`].
pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    request_timeout: Duration,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_on(listener, state, request_timeout, grace, shutdown).await
}

/// Serves on any listener (plain TCP or [`tls::TlsListener`]) until `shutdown`
/// resolves. Then readiness turns to 503, new requests get 503, in-flight
/// requests get `grace` to finish, and the repository and audit log are flushed.
pub async fn serve_on<L>(
    listener: L,
    state: Arc<AppState>,
    request_timeout: Duration,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()>
where
    L: axum::serve::Listener<Addr = std::net::SocketAddr>,
    api::PeerAddr: for<'a> axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, L>>,
{
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
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<api::PeerAddr>())
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

/// Reloads tokens, policies and (if given) the TLS certificate from the config
/// file. Other settings need a restart. On any error nothing is changed.
pub async fn reload(
    state: &AppState,
    path: &std::path::Path,
    cert: Option<&tls::ReloadableCert>,
) -> Result<(), String> {
    let p = path.to_path_buf();
    let cfg = tokio::task::spawn_blocking(move || Config::load(&p))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if let Some(c) = cert {
        c.reload()?;
    }
    if let Some(t) = &state.tokens {
        t.reload(&cfg.auth.tokens, cfg.auth.allow_anonymous);
    }
    state.authz.reload(cfg.policy.clone());
    state.audit.record_nowait(audit::Event {
        request_id: "-".into(),
        principal: "system".into(),
        action: "config_reload",
        target: path.display().to_string(),
        outcome: "ok".into(),
        detail: Some(format!("tokens={} policies={}", cfg.auth.tokens.len(), cfg.policy.len())),
        remote: None,
    });
    info!(tokens = cfg.auth.tokens.len(), policies = cfg.policy.len(), "configuration reloaded");
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
        match api::run_gc(&state, false, false).await {
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

/// Entry points for fuzzing and never-panic tests. Not a stable API.
#[doc(hidden)]
pub mod fuzzing {
    use std::sync::Arc;

    /// A fresh local repository in a temp dir (kept alive by the returned guard).
    pub fn temp_repo() -> (tempfile::TempDir, Arc<pico2peta_repo::LocalRepo>) {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let r = pico2peta_repo::LocalRepo::init(d.path().join("r"), &Default::default())
            .unwrap_or_else(|e| panic!("init: {e}"));
        (d, Arc::new(r))
    }

    /// The whole receive-pack path (commands + pack) on raw request bytes.
    pub fn git_push(repo: Arc<pico2peta_repo::LocalRepo>, spool: &std::path::Path, data: &[u8]) {
        crate::git::fuzz_push(repo, spool, data);
    }

    /// Builds a valid receive-pack request body creating `refs/heads/main`.
    pub fn git_push_request(objects: &[(u8, Vec<u8>)], tip: usize) -> Vec<u8> {
        crate::git::fuzz_make_push(objects, tip)
    }

    /// `apply(base, encode(base, target)) == target` (panics otherwise).
    pub fn git_delta_roundtrip(base: &[u8], target: &[u8]) {
        let d = crate::git::encode_delta(base, target);
        let back = crate::git::apply_delta(base, &d).unwrap_or_else(|e| panic!("encoded delta does not apply: {e}"));
        assert!(back == target, "delta roundtrip mismatch");
    }

    pub fn git_parsers(data: &[u8]) {
        crate::git::fuzz_parsers(data);
    }

    pub fn oci_parsers(data: &[u8]) {
        crate::oci::fuzz_parsers(data);
    }

    pub fn range(header: &str, size: u64) {
        if let Ok(v) = axum::http::HeaderValue::from_str(header)
            && let Ok(Some(r)) = crate::api::parse_range(Some(&v), size)
        {
            assert!(r.start < r.end && r.end <= size, "range {r:?} outside 0..{size}");
        }
    }

    pub fn segmap(data: &[u8]) {
        let _ = crate::segmap::decode_segment(data);
        let _ = crate::segmap::Root::decode(data);
    }

    /// Opens an audit log whose existing content is `data`, then verifies it.
    pub fn audit(data: &[u8]) {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let p = d.path().join("audit.jsonl");
        if std::fs::write(&p, data).is_err() {
            return;
        }
        if let Ok(a) = crate::audit::AuditLog::open(&p) {
            drop(a);
        }
        let _ = crate::audit::verify(&p);
    }

    pub fn config(data: &[u8]) {
        if let Ok(s) = std::str::from_utf8(data)
            && let Ok(c) = toml::from_str::<crate::config::Config>(s)
        {
            let _ = c.validate();
        }
    }
}
