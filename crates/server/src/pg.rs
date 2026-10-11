//! PostgreSQL storage: the shared registry service (`o5s_pgwire`) over native
//! connections. Each command runs in one transaction under its repository's
//! transaction-scoped lock; see `o5s_pgwire` for why that is serializable and
//! `docs/storage.md` for operation.
pub use o5s_pgwire::service::{Failure, unavailable};
use o5s_pgwire::{DbError, native};

pub type PgBackend = o5s_pgwire::service::Service<native::Tcp, native::Tokio>;

/// Connect with tables in PostgreSQL schema `schema`, creating them, with at
/// most `connections` connections. `O5S_DATABASE_ATTEMPTS` bounds retries.
pub async fn connect(url: &str, schema: &str, connections: usize) -> Result<PgBackend, DbError> {
    let mut engine = native::connect(url, schema, connections).await?;
    if let Some(attempts) = std::env::var("O5S_DATABASE_ATTEMPTS")
        .ok()
        .and_then(|a| a.parse().ok())
    {
        engine.max_attempts = attempts;
    }
    Ok(o5s_pgwire::service::Service::new(engine))
}
