pub mod artifacts;
pub mod auth;
pub mod backend;
pub mod legacy;
pub mod local;
pub mod metrics;
pub mod pg;
mod queries;
pub mod revocation;

pub use backend::{Backend, Failure};
pub use local::Store;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use h5i_app_json::Value;
use o5s_platform::{Actor, Command, IncrementalView, Refusal, Repository};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Channel caches by repository scope and channel name. Channel membership is
/// append-only, so a cache only integrates the suffix it has not seen.
type Views = Arc<Mutex<BTreeMap<(u64, String), IncrementalView>>>;

#[derive(Clone)]
pub struct AppState {
    pub backend: Backend,
    pub auth: Arc<auth::HmacAuth>,
    pub root: PathBuf,
    views: Views,
    pub metrics: Arc<metrics::Metrics>,
    /// Print one JSON line per request to stderr.
    pub log_requests: bool,
    pub revocations: Arc<revocation::Store>,
    pub rate: Arc<o5s_platform::limits::RateLimiter>,
}

impl AppState {
    pub fn new(backend: Backend, auth: auth::HmacAuth, root: PathBuf) -> Self {
        let revocations = Arc::new(match &backend {
            Backend::Postgres(pg) => revocation::Store::postgres(pg.clone()),
            Backend::Local(_) => revocation::Store::file(root.join(revocation::FILE)),
        });
        Self {
            revocations,
            rate: Arc::default(),
            backend,
            auth: Arc::new(auth),
            root,
            views: Arc::default(),
            metrics: Arc::default(),
            log_requests: false,
        }
    }

    /// The local log store's state, for tests and tooling.
    pub fn store(&self) -> Option<Arc<Mutex<Store>>> {
        match &self.backend {
            Backend::Local(store) => Some(store.clone()),
            Backend::Postgres(_) => None,
        }
    }
}

/// Refuse every request but `GET` that carries a read-only token.
async fn read_only_guard(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !state.auth.permits(request.method(), request.headers()) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error":"read-only token"}),
        );
    }
    next.run(request).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/api/repositories", get(repositories))
        .route("/api/state", get(repo_state))
        .route("/api/view", get(compact_view))
        .route("/api/snapshot", get(patch_snapshot))
        .route("/api/document", get(document_view))
        .route("/api/patches", get(queries::patches))
        .route("/api/patch", get(queries::patch))
        .route("/api/command", post(command))
        .route("/api/export", post(export))
        .route("/api/worktree", post(worktree))
        .route("/api/worktree/propose", post(worktree_propose))
        .route("/api/tokens/revoke", post(revoke_token))
        .route("/metrics", get(metrics::endpoint))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            revocation::guard,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            read_only_guard,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            metrics::observe,
        ))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(state)
}

fn actor(state: &AppState, headers: &HeaderMap) -> Result<Actor, Response> {
    let now = h5i_app_core::Timestamp::now().secs();
    state
        .auth
        .authenticate(headers, now)
        .map(|(tenant, user)| Actor { tenant, user })
        .map_err(|e| json(StatusCode::UNAUTHORIZED, serde_json::json!({"error":e.0})))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, Response> {
    match headers.get("idempotency-key") {
        None => Ok(None),
        Some(value) => match value.to_str() {
            Ok(key) if local::valid_key(key) => Ok(Some(key.to_string())),
            _ => Err(json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":"invalid Idempotency-Key"}),
            )),
        },
    }
}

fn out(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => n.as_u64().expect("API has only unsigned integers").into(),
        serde_json::Value::String(s) => Value::str(&s),
        serde_json::Value::Array(a) => Value::Arr(a.into_iter().map(out).collect()),
        serde_json::Value::Object(m) => Value::Obj(
            m.into_iter()
                .map(|(k, v)| (k.into_bytes(), out(v)))
                .collect(),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    /// Also every other token of the caller that expires no later.
    #[serde(default)]
    all: bool,
}

/// Revoke the presented token, or with `all`, every token of the caller
/// expiring no later than it.
async fn revoke_token(State(s): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let now = h5i_app_core::Timestamp::now().secs();
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let (tenant, user, exp) = match s.auth.claims(&token, now) {
        Ok(c) => c,
        Err(e) => return json(StatusCode::UNAUTHORIZED, serde_json::json!({"error":e.0})),
    };
    let q: RevokeRequest = if bytes.is_empty() {
        RevokeRequest { all: false }
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(q) => q,
            Err(e) => {
                return json(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error":e.to_string()}),
                );
            }
        }
    };
    let r = if q.all {
        o5s_pgwire::revoke::Revocation::User {
            tenant,
            user,
            not_after: exp,
        }
    } else {
        o5s_pgwire::revoke::Revocation::Token {
            digest: o5s_pgwire::revoke::digest(&token),
            expires: exp,
        }
    };
    match s.revocations.revoke(&r, now).await {
        Ok(()) => json(StatusCode::OK, serde_json::json!({"revoked":r})),
        Err(f) => failure(f),
    }
}

pub(crate) fn json(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        out(value).to_bytes(),
    )
        .into_response()
}

pub(crate) fn failure(f: Failure) -> Response {
    match f {
        Failure::Refused(e) => json(
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"error":e.0}),
        ),
        Failure::Conflict(m) => json(StatusCode::CONFLICT, serde_json::json!({"error":m})),
        Failure::Unavailable(m) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":m}),
        ),
    }
}

fn result(r: Result<serde_json::Value, Refusal>) -> Response {
    reply(r.map_err(Failure::Refused))
}

fn reply(r: Result<serde_json::Value, Failure>) -> Response {
    match r {
        Ok(v) => json(StatusCode::OK, v),
        Err(f) => failure(f),
    }
}

async fn ready(State(s): State<AppState>) -> Response {
    reply(
        s.backend
            .ready()
            .await
            .map(|()| serde_json::json!({"ready":true})),
    )
}

async fn command(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let key = match idempotency_key(&headers) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let c = match serde_json::from_slice::<Command>(&body) {
        Ok(c) => c,
        Err(e) => {
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":e.to_string()}),
            );
        }
    };
    if let Err(e) = admit(&s, &a, &c).await {
        return e;
    }
    reply(s.backend.execute(a, c, key).await)
}

/// The operator's rate limit and repository quota (`o5s_platform::limits`).
async fn admit(s: &AppState, a: &Actor, c: &Command) -> Result<(), Response> {
    let now = h5i_app_core::Timestamp::now().micros() / 1000;
    if !s.rate.allow(a, now) {
        return Err(json(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({"error":"too many commands; retry later"}),
        ));
    }
    if matches!(c, Command::Create { .. }) && o5s_platform::limits::get().max_repositories.is_some()
    {
        let count = s.backend.repositories(a).await.map_err(failure)?.len();
        o5s_platform::limits::check_repositories(count)
            .map_err(|e| failure(Failure::Refused(e)))?;
    }
    Ok(())
}

async fn repositories(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    reply(s.backend.repositories(&a).await.map(
        |names| serde_json::json!({"repositories":names,"actor":{"tenant":a.tenant,"user":a.user}}),
    ))
}

#[derive(Deserialize)]
struct RepoQuery {
    repo: String,
    #[serde(default = "main_channel")]
    channel: String,
}

fn main_channel() -> String {
    "main".into()
}

/// The authenticated actor and a fresh view of the named repository.
async fn load(
    s: &AppState,
    headers: &HeaderMap,
    name: &str,
) -> Result<(Actor, Arc<Repository>), Response> {
    let a = actor(s, headers)?;
    let repo = s.backend.repository(&a, name).await.map_err(failure)?;
    Ok((a, repo))
}

/// Run `f` on the repository's channel cache off the async runtime.
/// The channel's view from the server's incremental cache, as `with_view`
/// keeps it. The cache lock is released before the caller's Git work.
fn cached_view(
    views: &Views,
    a: &Actor,
    name: &str,
    channel: &str,
    repo: &Repository,
) -> Result<o5s_platform::View, Refusal> {
    let key = (
        o5s_platform::repository_key(a.tenant, name),
        channel.to_string(),
    );
    let mut views = views.lock().unwrap();
    let cache = views.entry(key).or_default();
    cache.sync_append_only(repo, channel)?;
    cache.view()
}

async fn with_view<F>(
    s: &AppState,
    a: &Actor,
    name: &str,
    channel: &str,
    repo: Arc<Repository>,
    f: F,
) -> Response
where
    F: FnOnce(&Repository, &mut IncrementalView) -> Result<serde_json::Value, Refusal>
        + Send
        + 'static,
{
    let views = s.views.clone();
    let key = (
        o5s_platform::repository_key(a.tenant, name),
        channel.to_string(),
    );
    match tokio::task::spawn_blocking(move || {
        let mut views = views.lock().unwrap();
        let cache = views.entry(key).or_default();
        f(&repo, cache)
    })
    .await
    {
        Ok(r) => result(r),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"view failed"}),
        ),
    }
}

async fn repo_state(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let (a, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    let channel = q.channel.clone();
    with_view(&s, &a, &q.repo, &q.channel, repo, move |repo, cache| {
        let work = cache.sync_append_only(repo, &channel)?;
        let view = cache.view()?;
        Ok(serde_json::json!({"repository":repo,"view":view,"work":work}))
    })
    .await
}

async fn compact_view(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let (a, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    let channel = q.channel.clone();
    with_view(&s, &a, &q.repo, &q.channel, repo, move |repo, cache| {
        let work = cache.sync_append_only(repo, &channel)?;
        let view = cache.content()?;
        Ok(serde_json::json!({"files":view.files,"conflicts":view.conflicts,"executable":view.executable,"binary":view.binary,"symlinks":view.symlinks,"submodules":view.submodules,"patch_count":cache.patch_count(),"work":work,"generation":repo.generation}))
    })
    .await
}

#[derive(Deserialize)]
struct SnapshotQuery {
    repo: String,
    patch: u64,
}

/// The files of the snapshot a patch defines (the patch and its causal past),
/// for executed checks.
async fn patch_snapshot(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SnapshotQuery>,
) -> Response {
    let (_, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    blocking(
        move || {
            let view = repo.materialize_patch(q.patch)?;
            Ok(o5s_platform::snapshot_json(&repo, q.patch, &view))
        },
        "snapshot",
    )
    .await
}

async fn blocking(
    f: impl FnOnce() -> Result<serde_json::Value, Refusal> + Send + 'static,
    what: &str,
) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => result(r),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":format!("{what} failed")}),
        ),
    }
}

fn body<T: serde::de::DeserializeOwned>(bytes: &Bytes) -> Result<T, Response> {
    serde_json::from_slice(bytes).map_err(|e| {
        json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error":e.to_string()}),
        )
    })
}

async fn export(State(s): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let q: RepoQuery = match body(&bytes) {
        Ok(q) => q,
        Err(e) => return e,
    };
    let (a, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    let (root, views) = (s.root.clone(), s.views.clone());
    blocking(
        move || {
            let view = cached_view(&views, &a, &q.repo, &q.channel, &repo)?;
            artifacts::export_view(&root, a.tenant, &q.repo, &q.channel, &repo, view)
        },
        "export",
    )
    .await
}

async fn worktree(State(s): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let q: RepoQuery = match body(&bytes) {
        Ok(q) => q,
        Err(e) => return e,
    };
    let (a, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    let (root, views) = (s.root.clone(), s.views.clone());
    blocking(
        move || {
            let view = cached_view(&views, &a, &q.repo, &q.channel, &repo)?;
            artifacts::worktree_view(&root, a.tenant, a.user, &q.repo, &q.channel, &repo, view)
        },
        "workspace",
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceProposal {
    repo: String,
    #[serde(default = "main_channel")]
    channel: String,
    commit: String,
    intent: String,
}

async fn worktree_propose(State(s): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let q: WorkspaceProposal = match body(&bytes) {
        Ok(q) => q,
        Err(e) => return e,
    };
    let key = match idempotency_key(&headers) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let (a, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    let root = s.root.clone();
    let user = a.clone();
    let proposal = tokio::task::spawn_blocking(move || {
        artifacts::workspace_proposal(
            &root,
            user.tenant,
            user.user,
            &q.repo,
            &q.channel,
            &q.commit,
            &q.intent,
            &repo,
        )
    })
    .await;
    match proposal {
        Ok(Ok(command)) => reply(s.backend.execute(a, command, key).await),
        Ok(Err(e)) => result(Err(e)),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"workspace proposal failed"}),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentQuery {
    repo: String,
    #[serde(default)]
    patches: Option<String>,
    #[serde(default = "main_channel")]
    channel: String,
    path: String,
}

async fn document_view(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DocumentQuery>,
) -> Response {
    let (_, repo) = match load(&s, &headers, &q.repo).await {
        Ok(x) => x,
        Err(e) => return e,
    };
    blocking(move || {
        if !o5s_platform::valid_path(&q.path) { return Err(Refusal("invalid document path".into())); }
        let selected: Vec<u64> = match q.patches {
            Some(value) if value.is_empty() => Vec::new(),
            Some(value) => {
                if value.len() > 128 * 1024 { return Err(Refusal("snapshot selection too large".into())); }
                let ids: Result<Vec<u64>, _> = value.split(',').map(str::parse).collect();
                let ids = ids.map_err(|_| Refusal("invalid snapshot patch identifiers".into()))?;
                if ids.len() > 10_000 { return Err(Refusal("snapshot selection too large".into())); }
                ids
            },
            None => repo.channels.get(&q.channel).ok_or_else(|| Refusal("channel not found".into()))?.clone(),
        };
        let graph = repo.document_at(&q.path, &selected)?;
        let view = graph.materialize();
        let nodes: Vec<_> = view.layers.iter().flatten().map(|id| serde_json::json!({"id":id,"bytes":graph.atom(*id)})).collect();
        Ok(serde_json::json!({"path":q.path,"patches":selected,"graph_backed":graph.patch_count() > 0,"layers":view.layers,"nodes":nodes,"conflicts":view.conflicts,"bytes":view.bytes}))
    }, "document view").await
}
