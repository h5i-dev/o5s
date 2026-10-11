pub mod artifacts;
pub mod auth;
mod queries;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use fs2::FileExt;
use h5i_app_json::Value;
use o5s_platform::{Actor, Command, Database, IncrementalView, Refusal};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct Store {
    pub db: Database,
    path: PathBuf,
    views: std::collections::BTreeMap<(u64, String, String), IncrementalView>,
    // Held for the process lifetime: no second writer can open the same store.
    _lock: File,
}

// Only the command's repository can change. Review/check commands change one
// fact plus the event tail, so their rollback state need not clone patch bodies.
enum Rollback {
    Registry {
        tenant: u64,
        repo: String,
        last_id: Option<u64>,
        events: usize,
    },
    Channel {
        tenant: u64,
        repo: String,
        channel: String,
        previous: Option<Vec<u64>>,
        events: usize,
    },
    Facts {
        tenant: u64,
        repo: String,
        patch: u64,
        approvals: Option<std::collections::BTreeSet<u64>>,
        checked: Option<bool>,
        events: usize,
    },
    Repository {
        tenant: u64,
        repo: String,
        existed: bool,
        previous: Option<o5s_platform::Repository>,
    },
}
impl Rollback {
    fn capture(db: &Database, actor: &Actor, command: &Command) -> Self {
        let name = command.repo();
        let repository = db.tenants.get(&actor.tenant).and_then(|t| t.get(name));
        if let Some(repository) = repository {
            match command {
                Command::Propose { .. }
                | Command::ProposeLines { .. }
                | Command::ProposeGraph { .. } => {
                    return Self::Registry {
                        tenant: actor.tenant,
                        repo: name.into(),
                        last_id: repository.patches.last_key_value().map(|(id, _)| *id),
                        events: repository.events.len(),
                    };
                }
                Command::Fork { channel, .. }
                | Command::Merge { channel, .. }
                | Command::MergeBatch { channel, .. } => {
                    return Self::Channel {
                        tenant: actor.tenant,
                        repo: name.into(),
                        channel: channel.clone(),
                        previous: repository.channels.get(channel).cloned(),
                        events: repository.events.len(),
                    };
                }
                _ => {}
            }
            let facts = match command {
                Command::Approve { patch, .. } => Some((
                    *patch,
                    repository.patches.get(patch).map(|p| p.approvals.clone()),
                    None,
                )),
                Command::Check { patch, .. } => Some((
                    *patch,
                    None,
                    repository.patches.get(patch).map(|p| p.checks_passed),
                )),
                _ => None,
            };
            if let Some((patch, approvals, checked)) = facts {
                return Self::Facts {
                    tenant: actor.tenant,
                    repo: name.into(),
                    patch,
                    approvals,
                    checked,
                    events: repository.events.len(),
                };
            }
        }
        Self::Repository {
            tenant: actor.tenant,
            repo: name.into(),
            existed: db.tenants.contains_key(&actor.tenant),
            previous: repository.cloned(),
        }
    }

    fn restore(self, db: &mut Database) {
        match self {
            Self::Registry {
                tenant,
                repo,
                last_id,
                events,
            } => {
                let repository = db.tenants.get_mut(&tenant).unwrap().get_mut(&repo).unwrap();
                while repository
                    .patches
                    .last_key_value()
                    .is_some_and(|(id, _)| last_id.is_none_or(|last| *id > last))
                {
                    repository.patches.pop_last();
                }
                repository.events.truncate(events);
            }
            Self::Channel {
                tenant,
                repo,
                channel,
                previous,
                events,
            } => {
                let repository = db.tenants.get_mut(&tenant).unwrap().get_mut(&repo).unwrap();
                if let Some(previous) = previous {
                    repository.channels.insert(channel, previous);
                } else {
                    repository.channels.remove(&channel);
                }
                repository.events.truncate(events);
            }
            Self::Facts {
                tenant,
                repo,
                patch,
                approvals,
                checked,
                events,
            } => {
                let repository = db.tenants.get_mut(&tenant).unwrap().get_mut(&repo).unwrap();
                if let Some(p) = repository.patches.get_mut(&patch) {
                    if let Some(approvals) = approvals {
                        p.approvals = approvals;
                    }
                    if let Some(checked) = checked {
                        p.checks_passed = checked;
                    }
                }
                repository.events.truncate(events);
            }
            Self::Repository {
                tenant,
                repo,
                existed,
                previous,
            } => {
                if let Some(previous) = previous {
                    db.tenants.entry(tenant).or_default().insert(repo, previous);
                } else if let Some(repositories) = db.tenants.get_mut(&tenant) {
                    repositories.remove(&repo);
                }
                if !existed {
                    db.tenants.remove(&tenant);
                }
            }
        }
    }
}
impl Store {
    pub fn open(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("store.lock"))?;
        lock.try_lock_exclusive()?;
        let path = root.join("state.json");
        let db = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Database::default()
        };
        Ok(Self {
            db,
            path,
            _lock: lock,
            views: Default::default(),
        })
    }
    pub fn execute(
        &mut self,
        actor: &Actor,
        command: Command,
    ) -> Result<serde_json::Value, Refusal> {
        if !o5s_platform::valid_name(command.repo()) {
            return Err(Refusal("invalid repository name".into()));
        }
        if matches!(&command, Command::Create { .. })
            && self
                .db
                .tenants
                .get(&actor.tenant)
                .is_some_and(|repos| repos.contains_key(command.repo()))
        {
            return Err(Refusal("repository already exists".into()));
        }
        let tmp = self.path.with_extension("json.tmp");
        let parent = File::open(self.path.parent().unwrap()).map_err(|e| Refusal(e.to_string()))?;
        let rollback = Rollback::capture(&self.db, actor, &command);
        // The caller holds the store mutex throughout execution and persistence.
        // No reader can see this tentative state before durable publication.
        let reply = match self.db.execute(actor, command) {
            Ok(reply) => reply,
            Err(error) => {
                rollback.restore(&mut self.db);
                return Err(error);
            }
        };
        // The registry returns before mutation for an existing canonical patch.
        // Preserve its idempotent result without rewriting identical storage.
        if reply.get("duplicate").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(reply);
        }
        let persist = || -> std::io::Result<()> {
            let mut file = BufWriter::with_capacity(64 * 1024, File::create(&tmp)?);
            serde_json::to_writer(&mut file, &self.db).map_err(std::io::Error::other)?;
            file.flush()?;
            file.get_ref().sync_all()?;
            drop(file);
            fs::rename(&tmp, &self.path)?;

            Ok(())
        };
        if let Err(error) = persist() {
            rollback.restore(&mut self.db);
            return Err(Refusal(format!("storage unavailable: {error}")));
        }
        parent
            .sync_all()
            .map_err(|e| Refusal(format!("committed; directory sync failed: {e}")))?;
        Ok(reply)
    }
}
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub auth: Arc<auth::HmacAuth>,
    pub root: PathBuf,
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/repositories", get(repositories))
        .route("/api/state", get(repo_state))
        .route("/api/view", get(compact_view))
        .route("/api/document", get(document_view))
        .route("/api/patches", get(queries::patches))
        .route("/api/patch", get(queries::patch))
        .route("/api/command", post(command))
        .route("/api/export", post(export))
        .route("/api/worktree", post(worktree))
        .route("/api/worktree/propose", post(worktree_propose))
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
fn json(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        out(value).to_bytes(),
    )
        .into_response()
}
fn result(r: Result<serde_json::Value, Refusal>) -> Response {
    match r {
        Ok(v) => json(StatusCode::OK, v),
        Err(e) => json(
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"error":e.0}),
        ),
    }
}
async fn command(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
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
    match tokio::task::spawn_blocking(move || s.store.lock().unwrap().execute(&a, c)).await {
        Ok(r) => result(r),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"transaction failed"}),
        ),
    }
}
async fn repositories(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let store = s.store.lock().unwrap();
    let names: Vec<&String> = store
        .db
        .tenants
        .get(&a.tenant)
        .map(|t| t.keys().collect())
        .unwrap_or_default();
    json(
        StatusCode::OK,
        serde_json::json!({"repositories":names,"actor":{"tenant":a.tenant,"user":a.user}}),
    )
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
async fn repo_state(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || {
        let mut store = s.store.lock().unwrap();
        let Store { db, views, .. } = &mut *store;
        let repo = db.repo(&a, &q.repo)?;
        let cache = views
            .entry((a.tenant, q.repo, q.channel.clone()))
            .or_default();
        let work = cache.sync_append_only(repo, &q.channel)?;
        let view = cache.view()?;
        Ok(serde_json::json!({"repository":repo,"view":view,"work":work}))
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
async fn compact_view(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RepoQuery>,
) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || {
        let mut store = s.store.lock().unwrap();
        let Store { db, views, .. } = &mut *store;
        let repo = db.repo(&a, &q.repo)?;
        let cache = views.entry((a.tenant, q.repo, q.channel.clone())).or_default();
        let work = cache.sync_append_only(repo, &q.channel)?;
        let view = cache.content()?;
        Ok(serde_json::json!({"files":view.files,"conflicts":view.conflicts,"patch_count":cache.patch_count(),"work":work}))
    }).await {
        Ok(r) => result(r),
        Err(_) => json(StatusCode::SERVICE_UNAVAILABLE,serde_json::json!({"error":"view failed"})),
    }
}
async fn export(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match actor(&s, &headers) {
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
    match tokio::task::spawn_blocking(move || {
        // Keep the transaction lock until ref update: exports have a single
        // consistent patch registry/channel and cannot race each other.
        let store = s.store.lock().unwrap();
        let repo = store.db.repo(&a, &q.repo)?;
        artifacts::export(&s.root, a.tenant, &q.repo, &q.channel, repo)
    })
    .await
    {
        Ok(r) => result(r),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"export failed"}),
        ),
    }
}

async fn worktree(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match actor(&s, &headers) {
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
    match tokio::task::spawn_blocking(move || {
        // Keep the transaction lock until ref update: exports have a single
        // consistent patch registry/channel and cannot race each other.
        let store = s.store.lock().unwrap();
        let repo = store.db.repo(&a, &q.repo)?;
        artifacts::worktree(&s.root, a.tenant, a.user, &q.repo, &q.channel, repo)
    })
    .await
    {
        Ok(r) => result(r),
        Err(_) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"workspace failed"}),
        ),
    }
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
async fn worktree_propose(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
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
    match tokio::task::spawn_blocking(move || {
        let mut store = s.store.lock().unwrap();
        let repo = store.db.repo(&a, &q.repo)?;
        let proposal = artifacts::workspace_proposal(
            &s.root, a.tenant, a.user, &q.repo, &q.channel, &q.commit, &q.intent, repo,
        )?;
        store.execute(&a, proposal)
    })
    .await
    {
        Ok(r) => result(r),
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
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || {
        let store = s.store.lock().unwrap();
        let repo = store.db.repo(&a, &q.repo)?;
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
    }).await {
        Ok(r) => result(r),
        Err(_) => json(StatusCode::SERVICE_UNAVAILABLE,serde_json::json!({"error":"document view failed"})),
    }
}
