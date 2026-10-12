//! The o5s API on Cloudflare Workers. The registry kernel, command planning,
//! narrow loads and repository views are the same crates the native server
//! runs (`o5s_pgwire::service`); this crate binds them to Hyperdrive and to
//! the Workers fetch handler. Git export pushes snapshots to Cloudflare
//! Artifacts (`artifacts.rs`), where agent workspaces live as well.

pub mod artifacts;
#[path = "../../server/src/auth.rs"]
pub mod auth;
pub mod workspace;

use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use o5s_pgwire::{
    Connector, DbError, Engine, Runtime,
    service::{Failure, Service},
};
use o5s_platform::{Actor, Command, IncrementalView, Refusal, Repository};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tower_service::Service as _;
use worker::{
    Context, Env, HttpRequest, Hyperdrive, SecureTransport, Socket, event,
    postgres_tls::PassthroughTls,
};

/// PostgreSQL schema of the registry tables, as the native server uses.
pub const SCHEMA: &str = "o5s";

/// A new connection through Hyperdrive, which pools the real ones.
pub struct Hyper(Hyperdrive);

impl Connector for Hyper {
    async fn connect(&self) -> Result<tokio_postgres::Client, DbError> {
        let socket = Socket::builder()
            .secure_transport(SecureTransport::StartTls)
            .connect(self.0.host(), self.0.port())
            .map_err(|e| DbError::Connect(e.to_string()))?;
        let config = self
            .0
            .connection_string()
            .parse::<tokio_postgres::Config>()
            .map_err(|e| DbError::Connect(e.to_string()))?;
        let (client, connection) = config
            .connect_raw(socket, PassthroughTls)
            .await
            .map_err(|e| DbError::Connect(format!("startup: {e}")))?;
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = connection.await {
                worker::console_error!("postgres connection: {e}");
            }
        });
        Ok(client)
    }
}

pub struct Delay;

impl Runtime for Delay {
    async fn sleep(&self, d: Duration) {
        worker::Delay::from(d).await
    }
}

type Registry = Service<Hyper, Delay>;

#[derive(Clone)]
struct AppState {
    env: Arc<Env>,
    ctx: Arc<Context>,
}

impl AppState {
    fn registry(&self) -> Result<Registry, Failure> {
        let hyperdrive = self
            .env
            .hyperdrive("HYPERDRIVE")
            .map_err(|e| Failure::Unavailable(e.to_string()))?;
        // One Worker request runs its transactions one after another.
        let engine = Engine::new(Hyper(hyperdrive), Delay, SCHEMA, 1)
            .map_err(|e| Failure::Unavailable(e.to_string()))?;
        Ok(Service::new(engine))
    }

    fn auth(&self) -> Result<auth::HmacAuth, Response> {
        let secret = self.env.secret("H5I_APP_SECRET").map_err(|_| {
            json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({"error":"secret not configured"}),
            )
        })?;
        Ok(auth::HmacAuth::new(secret.to_string()))
    }

    /// The caller of a request whose token is valid and not revoked.
    async fn actor(&self, headers: &HeaderMap) -> Result<Actor, Response> {
        let now = (worker::Date::now().as_millis() / 1000) as u64;
        let token = bearer(headers);
        let (tenant, user, exp) = self
            .auth()?
            .claims(&token, now)
            .map_err(|e| json(StatusCode::UNAUTHORIZED, serde_json::json!({"error":e.0})))?;
        let revoked = async {
            self.registry()?
                .engine
                .revoked(tenant, user, exp, &token)
                .await
                .map_err(o5s_pgwire::service::unavailable)
        }
        .await;
        match revoked {
            Ok(false) => Ok(Actor { tenant, user }),
            Ok(true) => Err(json(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({"error":"token revoked"}),
            )),
            Err(f) => Err(reply(Err(f))),
        }
    }
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned()
}

/// Per isolate: requests reach many isolates, so the rate limit is per
/// isolate, not global.
fn rate() -> &'static o5s_platform::limits::RateLimiter {
    static RATE: std::sync::OnceLock<o5s_platform::limits::RateLimiter> =
        std::sync::OnceLock::new();
    RATE.get_or_init(Default::default)
}

fn router(env: Env, ctx: Context) -> Router {
    if let Ok(limits) =
        o5s_platform::limits::Limits::from_lookup(|k| env.var(k).ok().map(|v| v.to_string()))
    {
        o5s_platform::limits::set(limits);
    }
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../../server/src/index.html")) }),
        )
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/api/repositories", get(repositories))
        .route("/api/state", get(repo_state))
        .route("/api/view", get(compact_view))
        .route("/api/snapshot", get(patch_snapshot))
        .route("/api/document", get(document_view))
        .route("/api/patches", get(patches))
        .route("/api/patch", get(patch))
        .route("/api/command", post(command))
        .route("/api/export", post(export))
        .route("/api/artifacts", get(artifacts_access))
        .route("/api/tokens/revoke", post(revoke_token))
        .route("/api/worktree", post(worktree))
        .route("/api/worktree/propose", post(worktree_propose))
        .with_state(AppState {
            env: Arc::new(env),
            ctx: Arc::new(ctx),
        })
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    ctx: Context,
) -> worker::Result<axum::http::Response<axum::body::Body>> {
    Ok(router(env, ctx).call(req).await?)
}

fn out(v: serde_json::Value) -> h5i_app_json::Value {
    use h5i_app_json::Value;
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0).into(),
        serde_json::Value::String(s) => Value::str(&s),
        serde_json::Value::Array(a) => Value::Arr(a.into_iter().map(out).collect()),
        serde_json::Value::Object(m) => Value::Obj(
            m.into_iter()
                .map(|(k, v)| (k.into_bytes(), out(v)))
                .collect(),
        ),
    }
}

/// Every body goes through the verified writer (`h5i-app-json`).
fn json(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        out(value).to_bytes(),
    )
        .into_response()
}

fn reply(r: Result<serde_json::Value, Failure>) -> Response {
    match r {
        Ok(v) => json(StatusCode::OK, v),
        Err(Failure::Refused(e)) => json(
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"error":e.0}),
        ),
        Err(Failure::Conflict(m)) => json(StatusCode::CONFLICT, serde_json::json!({"error":m})),
        Err(Failure::Unavailable(m)) => {
            worker::console_error!("request failed: {m}");
            json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({"error":m}),
            )
        }
    }
}

#[worker::send]
async fn ready(State(s): State<AppState>) -> Response {
    let r = async { s.registry()?.ready().await }.await;
    reply(r.map(|()| serde_json::json!({"ready":true})))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, Response> {
    match headers.get("idempotency-key") {
        None => Ok(None),
        Some(v) => match v.to_str() {
            Ok(k) if !k.is_empty() && k.len() <= 128 && k.bytes().all(|b| b.is_ascii_graphic()) => {
                Ok(Some(k.into()))
            }
            _ => Err(json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":"invalid Idempotency-Key"}),
            )),
        },
    }
}

#[worker::send]
async fn command(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match s.actor(&headers).await {
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
    if !rate().allow(&a, worker::Date::now().as_millis()) {
        return json(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({"error":"too many commands; retry later"}),
        );
    }
    if matches!(c, Command::Create { .. }) && o5s_platform::limits::get().max_repositories.is_some()
    {
        let count = async { s.registry()?.repositories(&a).await }.await;
        if let Err(e) = count.and_then(|n| {
            o5s_platform::limits::check_repositories(n.len()).map_err(Failure::Refused)
        }) {
            return reply(Err(e));
        }
    }
    // A merge also refreshes the channel's Git projection, after the reply.
    let merged = match &c {
        Command::Merge { repo, channel, .. } | Command::MergeBatch { repo, channel, .. } => {
            Some((repo.clone(), channel.clone()))
        }
        _ => None,
    };
    let r = async { s.registry()?.execute(&a, c, key.as_deref()).await }.await;
    if r.is_ok()
        && let Some((repo, channel)) = merged
    {
        let state = s.clone();
        s.ctx.wait_until(async move {
            if let Err(e) = export_channel(&state, &a, &repo, &channel).await {
                worker::console_error!("Git export after merge failed: {e:?}");
            }
        });
    }
    reply(r)
}

/// Push a channel's admitted snapshot to its Artifacts repository.
async fn export_channel(
    s: &AppState,
    a: &Actor,
    repo: &str,
    channel: &str,
) -> Result<serde_json::Value, Failure> {
    let view_repo = s.registry()?.repository(a, repo).await?;
    let view = view_repo.materialize(channel).map_err(Failure::Refused)?;
    artifacts::export(
        &s.env,
        &artifact_name(a, repo, None),
        channel,
        &view_repo,
        &view,
    )
    .await
    .map_err(unavailable)
}

#[worker::send]
async fn export(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match s.actor(&headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let q: RepoQuery = match serde_json::from_slice(&body) {
        Ok(q) => q,
        Err(e) => {
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":e.to_string()}),
            );
        }
    };
    if !o5s_platform::valid_name(&q.channel) {
        return reply(Err(Failure::Refused(Refusal(
            "invalid channel name".into(),
        ))));
    }
    reply(export_channel(&s, &a, &q.repo, &q.channel).await)
}

#[worker::send]
async fn repositories(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let a = match s.actor(&headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let r = async { s.registry()?.repositories(&a).await }.await;
    reply(r.map(
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

async fn load(s: &AppState, headers: &HeaderMap, name: &str) -> Result<Arc<Repository>, Response> {
    let a = s.actor(headers).await?;
    let r = async { s.registry()?.repository(&a, name).await }.await;
    r.map_err(|f| reply(Err(f)))
}

#[worker::send]
async fn repo_state(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let mut cache = IncrementalView::default();
    reply(
        (|| {
            let work = cache.sync_append_only(&repo, &q.channel)?;
            let view = cache.view()?;
            Ok::<_, Refusal>(serde_json::json!({"repository":*repo,"view":view,"work":work}))
        })()
        .map_err(Failure::Refused),
    )
}

#[worker::send]
async fn compact_view(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let mut cache = IncrementalView::default();
    reply((|| {
        let work = cache.sync_append_only(&repo, &q.channel)?;
        let view = cache.content()?;
        Ok::<_, Refusal>(serde_json::json!({"files":view.files,"conflicts":view.conflicts,"executable":view.executable,
            "binary":view.binary,"patch_count":cache.patch_count(),"work":work,"generation":repo.generation}))
    })()
    .map_err(Failure::Refused))
}

#[derive(Deserialize)]
struct SnapshotQuery {
    repo: String,
    patch: u64,
}

/// The files of the snapshot a patch defines, for executed checks.
#[worker::send]
async fn patch_snapshot(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SnapshotQuery>,
) -> Response {
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    reply(
        repo.materialize_patch(q.patch)
            .map(|view| o5s_platform::snapshot_json(&repo, q.patch, &view))
            .map_err(Failure::Refused),
    )
}

#[derive(Deserialize)]
struct DocumentQuery {
    repo: String,
    #[serde(default)]
    patches: Option<String>,
    #[serde(default = "main_channel")]
    channel: String,
    path: String,
}

#[worker::send]
async fn document_view(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DocumentQuery>,
) -> Response {
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    reply((|| {
        if !o5s_platform::valid_path(&q.path) {
            return Err(Refusal("invalid document path".into()));
        }
        let selected: Vec<u64> = match &q.patches {
            Some(v) if v.is_empty() => Vec::new(),
            Some(v) => {
                if v.len() > 128 * 1024 {
                    return Err(Refusal("snapshot selection too large".into()));
                }
                v.split(',').map(str::parse).collect::<Result<_, _>>().map_err(|_| Refusal("invalid snapshot patch identifiers".into()))?
            }
            None => repo.channels.get(&q.channel).ok_or_else(|| Refusal("channel not found".into()))?.clone(),
        };
        let graph = repo.document_at(&q.path, &selected)?;
        let view = graph.materialize();
        let nodes: Vec<_> =
            view.layers.iter().flatten().map(|id| serde_json::json!({"id":id,"bytes":graph.atom(*id)})).collect();
        Ok(serde_json::json!({"path":q.path,"patches":selected,"graph_backed":graph.patch_count() > 0,"layers":view.layers,
            "nodes":nodes,"conflicts":view.conflicts,"bytes":view.bytes}))
    })()
    .map_err(Failure::Refused))
}

#[derive(Deserialize)]
struct PatchPage {
    repo: String,
    #[serde(default)]
    after: u64,
    #[serde(default = "page_size")]
    limit: usize,
}

fn page_size() -> usize {
    50
}

#[worker::send]
async fn patches(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PatchPage>,
) -> Response {
    if !(1..=100).contains(&q.limit) {
        return reply(Err(Failure::Refused(Refusal(
            "limit must be between 1 and 100".into(),
        ))));
    }
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let mut registry = repo.patches.range(q.after + 1..);
    let page: Vec<_> = registry
        .by_ref()
        .take(q.limit)
        .map(|(id, p)| {
            serde_json::json!({"id":id,"hash":p.hash,"author":p.author,"intent":p.intent,
                "dependency_count":p.dependencies.len(),"edit_count":p.edits.len(),
                "line_edit_count":p.line_edits.len(),"graph_edit_count":p.graph_edits.len(),
                "approval_count":p.approvals.len(),"checks_passed":p.checks_passed})
        })
        .collect();
    let next = if registry.next().is_some() {
        page.last().map(|p| p["id"].clone())
    } else {
        None
    };
    reply(Ok(serde_json::json!({"patches":page,"next_after":next})))
}

#[derive(Deserialize)]
struct PatchQuery {
    repo: String,
    id: u64,
}

#[worker::send]
async fn patch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PatchQuery>,
) -> Response {
    let repo = match load(&s, &headers, &q.repo).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    reply(
        repo.patches
            .get(&q.id)
            .map(|p| serde_json::json!({"patch":p}))
            .ok_or_else(|| Failure::Refused(Refusal("patch not found".into()))),
    )
}

fn unavailable(e: String) -> Failure {
    if e.contains("conflicts") {
        Failure::Refused(Refusal(e))
    } else {
        Failure::Unavailable(e)
    }
}

/// The Artifacts repository of `repo`'s projections, or of `user`'s workspace.
fn artifact_name(a: &Actor, repo: &str, user: Option<u64>) -> String {
    match user {
        None => format!("{}-{}", a.tenant, repo),
        Some(u) => format!("{}-{}.{}", a.tenant, repo, u),
    }
}

/// Clone access (remote and a one-hour read token) to a repository's Git
/// projection, for members of its organization.
#[worker::send]
async fn artifacts_access(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let a = match s.actor(&headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let r = async {
        // Only a repository the caller's organization has.
        s.registry()?.repository(&a, &q.repo).await?;
        artifacts::access(&s.env, &artifact_name(&a, &q.repo, None))
            .await
            .map_err(unavailable)
    }
    .await;
    match r {
        Ok(v) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            out(v).to_bytes(),
        )
            .into_response(),
        Err(f) => reply(Err(f)),
    }
}

/// The caller's workspace: the channel's snapshot on `o5s/<channel>` of an
/// Artifacts repository only this user is given write tokens for.
#[worker::send]
async fn worktree(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match s.actor(&headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let q: RepoQuery = match serde_json::from_slice(&body) {
        Ok(q) => q,
        Err(e) => {
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":e.to_string()}),
            );
        }
    };
    if !o5s_platform::valid_name(&q.channel) {
        return reply(Err(Failure::Refused(Refusal(
            "invalid channel name".into(),
        ))));
    }
    let r = async {
        let repo = s.registry()?.repository(&a, &q.repo).await?;
        let view = repo.materialize(&q.channel).map_err(Failure::Refused)?;
        let name = artifact_name(&a, &q.repo, Some(a.user));
        let mut v = artifacts::workspace(&s.env, &name, &q.channel, &repo, &view)
            .await
            .map_err(unavailable)?;
        v["agent"] = a.user.into();
        v["push"] = "work".into();
        Ok(v)
    }
    .await;
    match r {
        Ok(v) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            out(v).to_bytes(),
        )
            .into_response(),
        Err(f) => reply(Err(f)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceProposal {
    repo: String,
    #[serde(default = "main_channel")]
    channel: String,
    intent: String,
    /// The branch the agent pushed its commits to.
    #[serde(default = "work_branch")]
    branch: String,
    /// If given, the snapshot commit the changes must start from.
    #[serde(default)]
    commit: Option<String>,
}

fn work_branch() -> String {
    "work".into()
}

/// Propose what the caller's workspace branch changes since its snapshot.
#[worker::send]
async fn worktree_propose(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match s.actor(&headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let key = match idempotency_key(&headers) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let q: WorkspaceProposal = match serde_json::from_slice(&body) {
        Ok(q) => q,
        Err(e) => {
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error":e.to_string()}),
            );
        }
    };
    if !o5s_platform::valid_name(&q.channel) || !artifacts::valid_branch(&q.branch) {
        return reply(Err(Failure::Refused(Refusal(
            "invalid channel or branch name".into(),
        ))));
    }
    let r = async {
        let registry = s.registry()?;
        let repo = registry.repository(&a, &q.repo).await?;
        let name = artifact_name(&a, &q.repo, Some(a.user));
        let (base, message, edits) = artifacts::changes(&s.env, &name, &q.branch)
            .await
            .map_err(Failure::Refused)?;
        if q.commit.as_ref().is_some_and(|c| *c != base) {
            return Err(Failure::Refused(Refusal(
                "the branch does not start from that snapshot".into(),
            )));
        }
        let command = o5s_platform::workspace::proposal(
            &q.repo, &q.channel, &q.intent, &message, &repo, edits,
        )
        .map_err(Failure::Refused)?;
        registry.execute(&a, command, key.as_deref()).await
    }
    .await;
    reply(r)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    #[serde(default)]
    all: bool,
}

/// Revoke the presented token, or with `all`, every token of the caller
/// expiring no later than it.
#[worker::send]
async fn revoke_token(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = s.actor(&headers).await {
        return e;
    }
    let token = bearer(&headers);
    let now = (worker::Date::now().as_millis() / 1000) as u64;
    let (tenant, user, exp) = match s.auth().map(|a| a.claims(&token, now)) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return json(StatusCode::UNAUTHORIZED, serde_json::json!({"error":e.0})),
        Err(e) => return e,
    };
    let all = if body.is_empty() {
        false
    } else {
        match serde_json::from_slice::<RevokeRequest>(&body) {
            Ok(q) => q.all,
            Err(e) => {
                return json(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error":e.to_string()}),
                );
            }
        }
    };
    use o5s_pgwire::revoke::{Revocation, digest};
    let r = if all {
        Revocation::User {
            tenant,
            user,
            not_after: exp,
        }
    } else {
        Revocation::Token {
            digest: digest(&token),
            expires: exp,
        }
    };
    let done = async {
        s.registry()?
            .engine
            .revoke(&r)
            .await
            .map_err(o5s_pgwire::service::unavailable)
    }
    .await;
    reply(done.map(|()| serde_json::json!({"revoked":r})))
}
