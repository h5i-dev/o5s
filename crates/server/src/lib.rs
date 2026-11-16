//! The o5s HTTP server.
//!
//! The API is in [`api`], and the wire format is in `o5s_client::wire`.
//! [`build_state`] opens the repository (local disk, or S3 for sealed packs) and
//! the audit log, and [`serve`] runs the HTTP server until `shutdown` resolves,
//! then drains in-flight requests and flushes.

pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod error;
pub mod fronts;
pub mod git;
pub(crate) mod gitgraph;
pub mod h5iapp;
mod hexs;
pub mod hf;
pub mod limits;
pub mod maven;
pub mod metrics;
pub mod npm;
pub mod oci;
pub mod oidc;
pub(crate) mod pkg;
pub(crate) mod prov_api;
pub mod pypi;
pub mod segmap;
pub mod tls;
pub mod xet;

use crate::api::AppState;
use crate::config::{BackendSection, Config};
use axum::http::{HeaderName, Request};
use o5s_core::Repository;
use o5s_core::backend::Backend;
use o5s_repo::LocalRepo;
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
    Repo(#[from] o5s_core::Error),
    #[error("audit log {0}: {1}")]
    Audit(std::path::PathBuf, std::io::Error),
    #[error("s3 backend: {0}")]
    S3(String),
    #[error("hf front: {0}")]
    Hf(String),
}

/// Opens everything the server needs.
pub fn build_state(cfg: &Config) -> Result<Arc<AppState>, StartError> {
    cfg.validate()?;
    let mut metrics = metrics::Metrics::new();
    let backend: Option<Arc<dyn Backend>> = match &cfg.backend {
        BackendSection::Local => None,
        BackendSection::S3(s) => {
            let mut c = o5s_backend_s3::S3Config::new(&s.endpoint, &s.region, &s.bucket).with_prefix(&s.prefix);
            c.path_style = s.path_style;
            c.conditional_writes = s.conditional_writes;
            if let Some(n) = s.max_in_flight {
                c.max_in_flight = n;
                c.pool_max_idle = n;
                c.multipart_concurrency = c.multipart_concurrency.min(n);
            }
            // Env keys, then web identity (EKS/OIDC), then the EC2 instance profile.
            let b = Arc::new(o5s_backend_s3::S3Backend::with_default_chain(c, 2)?);
            let client = b.client().clone();
            metrics = metrics.with_s3(Arc::new(move || client.stats().snapshot()));
            Some(b)
        }
    };
    let path = &cfg.repo.path;
    if !path.join("config.toml").exists() {
        if !cfg.repo.create {
            return Err(StartError::Repo(o5s_core::Error::Parse(format!(
                "{} is not a repository (set repo.create = true to initialize it)",
                path.display()
            ))));
        }
        info!(path = %path.display(), refs = ?cfg.repo.refs, "initializing repository");
        drop(LocalRepo::init_with(path, &o5s_pack::PackConfig::default(), cfg.repo.refs)?);
    }
    let existing = LocalRepo::read_config(path)?;
    if existing.refs.store != cfg.repo.refs {
        return Err(StartError::Repo(o5s_core::Error::Parse(format!(
            "repo.refs = {:?} in the server config, but {} was created with refs = {:?}",
            cfg.repo.refs,
            path.display(),
            existing.refs.store
        ))));
    }
    if existing.refs.store == o5s_repo::RefsStore::Backend && matches!(cfg.backend, BackendSection::Local) {
        warn!("refs on the backend with a local backend: only processes on this host can share the repository");
    }
    let local = match backend {
        None => LocalRepo::open(path)?,
        Some(b) => LocalRepo::open_with_backend(path, b)?,
    };
    // Register the domain fronts: names under git/ and oci/ can only be moved through them.
    let fronts = Arc::new(fronts::Fronts::open(local)?);
    let local = fronts.repo.clone();
    let repo: Arc<dyn Repository> = Arc::new(local.clone());
    // Object-read authorization: which policy cells reach each object (core's reachability
    // index, kept current by this handle's ref writes). Opened after the fronts, whose tracers
    // it walks.
    let cells = api::policy_cells(&cfg.policy);
    let reach = o5s_repo::reach::ReachIndex::open(&local, cells.clone())?;
    let oci = if cfg.oci.enabled {
        let dir = cfg.oci.upload_dir.clone().unwrap_or_else(|| {
            let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.clone());
            parent.join(format!("{}.oci-uploads", path.file_name().and_then(|n| n.to_str()).unwrap_or("repo")))
        });
        let st = oci::OciState::new(fronts.handle("oci")?, dir.clone(), cfg.oci.max_upload_sessions)
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
        let st = git::GitState::new(fronts.handle("git")?, dir.clone(), cfg.server.max_upload_bytes)
            .map_err(|e| StartError::Audit(dir, e))?
            .with_sha256_repos(cfg.git.sha256_repos.clone())
            .with_clone_pack_cache(cfg.git.clone_pack_cache);
        Some(Arc::new(st))
    } else {
        None
    };
    let hf = match (&git, cfg.hf.enabled) {
        (Some(g), true) => Some(Arc::new(hf::HfState::new(g.clone(), Some(local.clone())).map_err(StartError::Hf)?)),
        _ => None,
    };
    let pypi = if cfg.pypi.enabled {
        let st = pypi::PypiState::new(fronts.handle("pypi")?, Some(local.clone()), cfg.pypi.upstream.clone())
            .map_err(StartError::Hf)?;
        Some(Arc::new(st))
    } else {
        None
    };
    let npm = if cfg.npm.enabled {
        let st = npm::NpmState::new(fronts.handle("npm")?, Some(local.clone()), cfg.npm.upstream.clone())
            .map_err(StartError::Hf)?;
        Some(Arc::new(st))
    } else {
        None
    };
    let maven = if cfg.maven.enabled {
        let st = maven::MavenState::new(fronts.handle("maven")?, Some(local.clone()), cfg.maven.upstream.clone())
            .map_err(StartError::Hf)?;
        Some(Arc::new(st))
    } else {
        None
    };
    let token_key = match &cfg.auth.token_key_file {
        Some(f) => {
            let k = std::fs::read(f).map_err(|e| StartError::Audit(f.clone(), e))?;
            if k.len() < 32 {
                return Err(StartError::Audit(
                    f.clone(),
                    std::io::Error::other("auth.token_key_file must hold at least 32 bytes"),
                ));
            }
            Some(k)
        }
        None => None,
    };
    let tokens =
        Arc::new(auth::TokenAuthenticator::new(&cfg.auth.tokens, cfg.auth.allow_anonymous).with_key(token_key.clone()));
    let audit = audit::AuditLog::open_with(&cfg.audit.path, cfg.audit.rotate_bytes)
        .map_err(|e| StartError::Audit(cfg.audit.path.clone(), e))?;
    let authz = Arc::new(auth::Authorizer::new(cfg.policy.clone()));
    let shared_cells = Arc::new(std::sync::RwLock::new(cells));
    Ok(Arc::new(AppState {
        repo,
        authn: match &cfg.auth.oidc {
            Some(o) => Arc::new(oidc::ChainAuthenticator {
                tokens: tokens.clone(),
                oidc: Arc::new(oidc::OidcAuthenticator::new(o.clone()).map_err(StartError::S3)?),
            }),
            None => tokens.clone(),
        },
        tokens: Some(tokens.clone()),
        authz: authz.clone(),
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
                    key: token_key.clone(),
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
        hf,
        pypi,
        npm,
        maven,
        search: Default::default(),
        promotion: cfg.promotion.clone(),
        kernel: h5i_app_http::H5iApp::with_executor(
            Arc::new(h5iapp::NetExecutor { reach: Some(reach.clone()), cells: shared_cells.clone() }),
            h5iapp::NetAuth { tokens: Some(tokens.clone()), authz: authz.clone() },
        ),
        reach: Some(reach),
        cells: shared_cells,
        ns_limits: (!cfg.limits.is_empty()).then(|| Arc::new(limits::NamespaceLimits::new(cfg.limits.clone()))),
        limits_scan: Duration::from_secs(cfg.server.limits_scan_secs.max(1)),
        fronts: Some(fronts),
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
    let rid = HeaderName::from_static(o5s_client::wire::REQUEST_ID_HEADER);
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
        .layer(axum::middleware::from_fn(body_timing))
        .layer(trace)
        .layer(SetSensitiveRequestHeadersLayer::new([axum::http::header::AUTHORIZATION]))
        .layer(PropagateRequestIdLayer::new(rid.clone()))
        .layer(SetRequestIdLayer::new(rid, RandomRequestId))
}

/// With INFO logging: one `response body done` event per request when its response body is
/// dropped (fully sent, or the connection went away), carrying the time since the request came
/// in. A request's server-side interval for the HF benchmark (PLAN §6.8 HF amendment): a
/// streamed download's body counts, not just the time to its first byte.
async fn body_timing(req: Request<axum::body::Body>, next: axum::middleware::Next) -> axum::response::Response {
    if !tracing::enabled!(tracing::Level::INFO) {
        return next.run(req).await;
    }
    let start = std::time::Instant::now();
    let (method, path) = (req.method().to_string(), req.uri().path().to_string());
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    axum::response::Response::from_parts(parts, axum::body::Body::new(TimedBody { inner: body, start, method, path }))
}

struct TimedBody {
    inner: axum::body::Body,
    start: std::time::Instant,
    method: String,
    path: String,
}

impl http_body::Body for TimedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<axum::body::Bytes>, axum::Error>>> {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for TimedBody {
    fn drop(&mut self) {
        tracing::info!(
            method = %self.method,
            path = %self.path,
            elapsed_us = self.start.elapsed().as_micros() as u64,
            "response body done"
        );
    }
}

/// Serves the ops router (`/metrics`, `/healthz`, `/readyz`) on `listener`
/// until `shutdown` resolves.
pub async fn serve_ops(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_http(listener, api::ops_router(state), shutdown).await
}

/// How long a client may take to send a request's headers (also bounds idle keep-alive
/// connections waiting for their next request). Without it, a client trickling headers
/// (slowloris) holds a connection forever.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Accepts connections on `listener` and serves `app` on each (HTTP/1.1 and HTTP/2), with a
/// header read timeout and HTTP/2 keep-alive pings. Stops accepting when `shutdown`
/// resolves, then waits for open connections to finish (callers bound that wait).
pub async fn serve_http<L>(
    mut listener: L,
    app: axum::Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()>
where
    L: axum::serve::Listener<Addr = std::net::SocketAddr>,
{
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use tower::ServiceExt;
    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(HEADER_READ_TIMEOUT);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(Some(Duration::from_secs(30)))
        .keep_alive_timeout(Duration::from_secs(20));
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            (io, addr) = listener.accept() => {
                let app = app.clone();
                let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(axum::extract::ConnectInfo(api::PeerAddr(addr)));
                    app.clone().oneshot(req.map(axum::body::Body::new))
                });
                let conn = builder.serve_connection_with_upgrades(TokioIo::new(io), svc).into_owned();
                let conn = graceful.watch(conn);
                tokio::spawn(async move {
                    let _ = conn.await;
                });
            }
            _ = &mut shutdown => break,
        }
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
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
{
    let app = app(state.clone(), request_timeout);
    let gc_task = (state.gc.interval_secs > 0).then(|| tokio::spawn(gc_loop(state.clone())));
    let quota_task =
        state.ns_limits.as_ref().is_some_and(|l| l.has_quotas()).then(|| tokio::spawn(quota_loop(state.clone())));
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
    let server = serve_http(listener, app, graceful);
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
    if let Some(t) = quota_task {
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
    // New policy prefixes mean new cells for object-read authorization.
    let cells = api::policy_cells(&cfg.policy);
    if let Some(r) = state.reach.clone() {
        let c = cells.clone();
        tokio::task::spawn_blocking(move || r.on_policy_reload(c))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
    }
    if let Ok(mut g) = state.cells.write() {
        *g = cells;
    }
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

/// Measures `[[limits]]` quotas at start and then every `server.limits_scan_secs`.
async fn quota_loop(state: Arc<AppState>) {
    loop {
        if state.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let (Some(l), Some(local)) = (state.ns_limits.clone(), state.local.clone()) {
            let t = std::time::Instant::now();
            let r = tokio::task::spawn_blocking(move || {
                let tracer = local.fronts().tracer(&local)?;
                l.scan(&local, &tracer)?;
                Ok::<_, o5s_core::Error>(l.usage())
            })
            .await;
            match r {
                Ok(Ok(u)) => info!(usage = ?u, secs = t.elapsed().as_secs_f64(), "namespace quota scan"),
                Ok(Err(e)) => warn!(error = %e, "namespace quota scan failed"),
                Err(e) => warn!(error = %e, "namespace quota scan panicked"),
            }
        }
        tokio::time::sleep(state.limits_scan).await;
    }
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
    pub fn temp_repo() -> (tempfile::TempDir, Arc<o5s_repo::LocalRepo>) {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let r =
            o5s_repo::LocalRepo::init(d.path().join("r"), &Default::default()).unwrap_or_else(|e| panic!("init: {e}"));
        (d, Arc::new(r))
    }

    /// The whole receive-pack path (commands + pack) on raw request bytes.
    pub fn git_push(repo: Arc<o5s_repo::LocalRepo>, spool: &std::path::Path, data: &[u8]) {
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

    /// HF (LFS pointers, Xet xorbs/shards/records), PyPI (names, wheel zip) and npm (names) parsers.
    pub fn pkg_parsers(data: &[u8]) {
        crate::xet::fuzz_parsers(data);
        let _ = crate::hf::parse_lfs_pointer(data);
        crate::hf::fuzz_paths(data);
        crate::gitgraph::fuzz_decode(data);
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = crate::hexs::decode32(s);
        }
        let text = String::from_utf8_lossy(data);
        let _ = crate::pypi::fuzz_names(&text);
        let _ = crate::npm::fuzz_names(&text);
        let read = |r: std::ops::Range<u64>| -> o5s_core::Result<Vec<u8>> {
            data.get(r.start as usize..r.end as usize)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| o5s_core::Error::Corrupt("out of range".into()))
        };
        let _ = crate::pkg::wheel_metadata_with(data.len() as u64, &read);
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
