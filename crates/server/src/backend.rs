//! One interface over the local log store and PostgreSQL.
use crate::{local::Store, pg::PgBackend};
use o5s_platform::{Actor, Command, Repository};
use std::sync::{Arc, Mutex};

pub use o5s_pgwire::service::Failure;

#[derive(Clone)]
pub enum Backend {
    Local(Arc<Mutex<Store>>),
    Postgres(Arc<PgBackend>),
}

fn joined<T>(r: Result<Result<T, Failure>, tokio::task::JoinError>) -> Result<T, Failure> {
    r.unwrap_or_else(|_| Err(Failure::Unavailable("transaction failed".into())))
}

impl Backend {
    pub fn local(store: Store) -> Self {
        Backend::Local(Arc::new(Mutex::new(store)))
    }

    pub async fn execute(
        &self,
        actor: Actor,
        command: Command,
        key: Option<String>,
    ) -> Result<serde_json::Value, Failure> {
        match self {
            Backend::Local(store) => {
                let store = store.clone();
                joined(
                    tokio::task::spawn_blocking(move || {
                        store
                            .lock()
                            .unwrap()
                            .execute_keyed(&actor, command, key.as_deref())
                    })
                    .await,
                )
            }
            Backend::Postgres(pg) => pg.execute(&actor, command, key.as_deref()).await,
        }
    }

    pub async fn repository(&self, actor: &Actor, name: &str) -> Result<Arc<Repository>, Failure> {
        match self {
            Backend::Local(store) => Ok(store.lock().unwrap().db.repo_shared(actor, name)?),
            Backend::Postgres(pg) => pg.repository(actor, name).await,
        }
    }

    pub async fn repositories(&self, actor: &Actor) -> Result<Vec<String>, Failure> {
        match self {
            Backend::Local(store) => Ok(store.lock().unwrap().db.repositories(actor)),
            Backend::Postgres(pg) => pg.repositories(actor).await,
        }
    }

    /// Storage counters in Prometheus text format.
    pub fn metrics(&self) -> String {
        match self {
            Backend::Local(store) => {
                let store = store.lock().unwrap();
                format!(
                    "# TYPE o5s_local_log_bytes gauge\no5s_local_log_bytes {}\n# TYPE o5s_local_poisoned gauge\no5s_local_poisoned {}\n",
                    store.log_length(),
                    u8::from(store.poisoned())
                )
            }
            Backend::Postgres(pg) => {
                use std::sync::atomic::Ordering::Relaxed;
                let s = &pg.engine.stats;
                let mut groups = format!(
                    "# TYPE o5s_pg_groups_total counter\no5s_pg_groups_total {}\n# TYPE o5s_pg_grouped_commands_total counter\no5s_pg_grouped_commands_total {}\n# TYPE o5s_pg_group_write_statements_total counter\no5s_pg_group_write_statements_total {}\n# TYPE o5s_pg_group_seconds_total counter\n",
                    s.groups.load(Relaxed),
                    s.grouped.load(Relaxed),
                    s.group_writes.load(Relaxed)
                );
                for (phase, us) in o5s_pgwire::GROUP_PHASES.iter().zip(&s.group_us) {
                    groups.push_str(&format!(
                        "o5s_pg_group_seconds_total{{phase=\"{phase}\"}} {}\n",
                        us.load(Relaxed) as f64 / 1e6
                    ));
                }
                groups
                    + &format!(
                        "# TYPE o5s_pg_attempts_total counter\no5s_pg_attempts_total {}\n# TYPE o5s_pg_retries_total counter\no5s_pg_retries_total {}\n# TYPE o5s_pg_commits_total counter\no5s_pg_commits_total {}\n# TYPE o5s_pg_replays_total counter\no5s_pg_replays_total {}\n",
                        s.attempts.load(Relaxed),
                        s.retries.load(Relaxed),
                        s.commits.load(Relaxed),
                        s.replays.load(Relaxed)
                    )
            }
        }
    }

    pub async fn ready(&self) -> Result<(), Failure> {
        match self {
            Backend::Local(_) => Ok(()),
            Backend::Postgres(pg) => pg.ready().await,
        }
    }
}
