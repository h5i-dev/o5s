//! Revoked tokens: refused before any handler runs. The local backend keeps
//! them in `revocations.json` next to its log, which `o5s-server revoke`
//! rewrites while the server runs; PostgreSQL keeps them in two tables
//! shared by every replica and the Worker (`o5s_pgwire::revoke`).
use crate::{AppState, backend::Failure, pg::PgBackend};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use o5s_pgwire::revoke::{Revocation, Revocations, digest};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

/// How long a "not revoked" answer is reused. A revocation takes effect
/// within this time on every server.
const FRESH: Duration = Duration::from_secs(5);

pub enum Store {
    File {
        path: PathBuf,
        /// The file's modification time when last read, and its contents.
        cache: Mutex<(Option<SystemTime>, Revocations)>,
    },
    Postgres {
        pg: Arc<PgBackend>,
        /// Token digests recently found not revoked.
        cache: Mutex<HashMap<String, Instant>>,
    },
}

pub const FILE: &str = "revocations.json";

fn unavailable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(format!("revocations unavailable: {e}"))
}

/// Read a revocations file; a missing file has none.
pub fn read(path: &std::path::Path) -> Result<Revocations, Failure> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(unavailable),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Revocations::default()),
        Err(e) => Err(unavailable(e)),
    }
}

/// Add `r` to the file, replacing it atomically.
pub fn write(path: &std::path::Path, r: &Revocation, now: u64) -> Result<(), Failure> {
    let mut all = read(path)?;
    all.add(r);
    all.prune(now);
    let staged = path.with_extension("tmp");
    std::fs::write(
        &staged,
        serde_json::to_vec_pretty(&all).map_err(unavailable)?,
    )
    .map_err(unavailable)?;
    std::fs::rename(&staged, path).map_err(unavailable)
}

impl Store {
    pub fn file(path: PathBuf) -> Self {
        Store::File {
            path,
            cache: Mutex::new((None, Revocations::default())),
        }
    }

    pub fn postgres(pg: Arc<PgBackend>) -> Self {
        Store::Postgres {
            pg,
            cache: Mutex::default(),
        }
    }

    pub async fn revoked(
        &self,
        tenant: u64,
        user: u64,
        exp: u64,
        token: &str,
    ) -> Result<bool, Failure> {
        match self {
            Store::File { path, cache } => {
                let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                let mut c = cache.lock().unwrap();
                if c.0 != modified || modified.is_none() {
                    *c = (modified, read(path)?);
                }
                Ok(c.1.revoked(tenant, user, exp, &digest(token)))
            }
            Store::Postgres { pg, cache } => {
                let d = digest(token);
                if cache
                    .lock()
                    .unwrap()
                    .get(&d)
                    .is_some_and(|t| t.elapsed() < FRESH)
                {
                    return Ok(false);
                }
                let revoked = pg
                    .engine
                    .revoked(tenant, user, exp, token)
                    .await
                    .map_err(unavailable)?;
                if !revoked {
                    let mut c = cache.lock().unwrap();
                    if c.len() > 100_000 {
                        c.clear();
                    }
                    c.insert(d, Instant::now());
                }
                Ok(revoked)
            }
        }
    }

    pub async fn revoke(&self, r: &Revocation, now: u64) -> Result<(), Failure> {
        match self {
            Store::File { path, .. } => write(path, r, now),
            Store::Postgres { pg, cache } => {
                pg.engine.revoke(r).await.map_err(unavailable)?;
                cache.lock().unwrap().clear();
                Ok(())
            }
        }
    }
}

/// Refuse a request whose valid token has been revoked. Requests without a
/// valid token pass on, and their handlers refuse them.
pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let now = h5i_app_core::Timestamp::now().secs();
    let token = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned);
    if let Some(token) = token
        && let Ok((tenant, user, exp)) = state.auth.claims(&token, now)
    {
        match state.revocations.revoked(tenant, user, exp, &token).await {
            Ok(false) => {}
            Ok(true) => {
                return crate::json(
                    StatusCode::UNAUTHORIZED,
                    serde_json::json!({"error":"token revoked"}),
                );
            }
            Err(f) => return crate::failure(f),
        }
    }
    next.run(request).await
}
