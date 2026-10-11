pub mod artifacts;
pub mod auth;

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
    io::Write,
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
        let mut candidate = self.db.clone();
        let reply = candidate.execute(actor, command)?;
        let bytes = serde_json::to_vec(&candidate).map_err(|e| Refusal(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        let parent = File::open(self.path.parent().unwrap()).map_err(|e| Refusal(e.to_string()))?;
        let persist = || -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;

            Ok(())
        };
        persist().map_err(|e| Refusal(format!("storage unavailable: {e}")))?;
        self.db = candidate;
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
        .route("/api/command", post(command))
        .route("/api/export", post(export))
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
