//! The o5s registry on PostgreSQL, for native servers and Cloudflare Workers.
//!
//! Each command runs in one transaction on one tenant scope:
//!
//! ```text
//! BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL search_path TO "schema";
//! SELECT pg_advisory_xact_lock(scope);
//! -- idempotency check, narrow loads, o5s_registry::transition, writes
//! COMMIT
//! ```
//!
//! The transaction-scoped lock orders all commands of a scope, and every later
//! statement reads with a fresh snapshot that includes every earlier holder's
//! commit. A command reads and writes only rows of its own scope, so commands
//! of different scopes share no rows. Every interleaving is therefore
//! equivalent to running the commands one at a time in lock order, the model
//! the Lean storage theorems assume. The lock is released at commit or
//! rollback, so it also works through transaction-mode connection pools such
//! as Hyperdrive. Serializable isolation would add page-grained predicate locks
//! on shared index pages, which abort unrelated repositories.
//!
//! All SQL on registry tables is planned by `h5i_app_sql` and compiled by
//! `h5i_app_pgsql` (both extracted to Lean). Statements are sent unnamed with
//! their parameter types, as Hyperdrive needs.

pub mod service;

use h5i_app_pgsql::{Column, Kind, Query, Table};
use h5i_app_sql::{Val, Write};
use o5s_registry as kernel;
use std::{
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_postgres::{
    Client, Row,
    error::SqlState,
    types::{IsNull, ToSql, Type, to_sql_checked},
};

#[derive(Debug)]
pub enum DbError {
    Postgres(tokio_postgres::Error),
    Connect(String),
    Decode(String),
    IdempotencyConflict,
    RetriesExhausted,
    /// The connection dropped during COMMIT, so it may or may not have committed.
    CommitUnknown(tokio_postgres::Error),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The driver's Display omits the server's message; show its code and text.
            DbError::Postgres(e) => match e.as_db_error() {
                Some(d) => write!(f, "postgres {}: {}", d.code().code(), d.message()),
                None => write!(f, "postgres: {e}"),
            },
            DbError::Connect(e) => write!(f, "connect: {e}"),
            DbError::Decode(e) => write!(f, "decode: {e}"),
            DbError::IdempotencyConflict => {
                write!(f, "idempotency key reused for a different command")
            }
            DbError::RetriesExhausted => write!(f, "gave up after too many retryable failures"),
            DbError::CommitUnknown(e) => write!(f, "commit outcome unknown: {e}"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<tokio_postgres::Error> for DbError {
    fn from(e: tokio_postgres::Error) -> Self {
        DbError::Postgres(e)
    }
}

impl DbError {
    fn code(&self) -> Option<&SqlState> {
        match self {
            DbError::Postgres(e) => e.code(),
            _ => None,
        }
    }

    /// Safe to rerun from BEGIN: the attempt certainly did not commit.
    fn is_retryable(&self) -> bool {
        match self {
            DbError::Postgres(e) => {
                e.is_closed()
                    || matches!(e.code(), Some(c) if *c == SqlState::T_R_SERIALIZATION_FAILURE
                        || *c == SqlState::T_R_DEADLOCK_DETECTED
                        || *c == SqlState::ADMIN_SHUTDOWN
                        || *c == SqlState::CRASH_SHUTDOWN)
            }
            DbError::Connect(_) => true,
            _ => false,
        }
    }
}

fn commit_error(e: tokio_postgres::Error) -> DbError {
    if e.code().is_some() {
        DbError::Postgres(e)
    } else {
        DbError::CommitUnknown(e)
    }
}

/// Opens connections: TCP natively, a Workers socket through Hyperdrive.
pub trait Connector {
    fn connect(&self) -> impl Future<Output = Result<Client, DbError>>;
}

/// Sleeping between retries: tokio natively, `worker::Delay` on Workers.
pub trait Runtime {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
}

/// The registry's tables, numbered like `schema!`'s `TABLE` constants. A
/// server test checks this equals what `h5i_app_pg` derives from `schema!`.
pub fn tables() -> Vec<Table> {
    fn col(name: &str, kind: Kind, nullable: bool) -> Column {
        Column {
            name: name.as_bytes().to_vec(),
            kind,
            nullable,
        }
    }
    fn table(name: &str, key_len: u32, columns: Vec<Column>) -> Table {
        Table {
            name: name.as_bytes().to_vec(),
            columns,
            key_len,
        }
    }
    let int = |n| col(n, Kind::Int, false);
    let bytes = |n| col(n, Kind::Bytes, false);
    let bool_ = |n| col(n, Kind::Bool, false);
    vec![
        table(
            "o5s_repository",
            0,
            vec![
                int("org"),
                bytes("label"),
                bool_("created"),
                int("patches"),
                int("channels"),
                int("paths"),
                int("events"),
            ],
        ),
        table(
            "o5s_patches",
            1,
            vec![int("id"), bytes("hash"), int("author")],
        ),
        table("o5s_dependencies", 2, vec![int("patch"), int("dependency")]),
        table("o5s_approvals", 2, vec![int("patch"), int("reviewer")]),
        table(
            "o5s_checks",
            1,
            vec![int("patch"), bool_("passed"), int("reporter")],
        ),
        table(
            "o5s_channels",
            1,
            vec![int("id"), bytes("label"), int("size")],
        ),
        table(
            "o5s_members",
            2,
            vec![int("channel"), int("position"), int("patch")],
        ),
        table(
            "o5s_paths",
            1,
            vec![int("id"), bytes("path"), bool_("graph")],
        ),
        table(
            "o5s_events",
            1,
            vec![
                int("seq"),
                int("actor"),
                int("action"),
                col("patch", Kind::Int, true),
                col("channel", Kind::Int, true),
            ],
        ),
        table("o5s_bodies", 1, vec![int("patch"), bytes("bytes")]),
        table("o5s_listings", 1, vec![int("repo"), bytes("label")]),
    ]
}

const IDEMPOTENCY_DDL: &str = "CREATE TABLE IF NOT EXISTS h5i_idempotency (
  tenant_id BIGINT NOT NULL,
  key TEXT NOT NULL,
  fingerprint BYTEA NOT NULL,
  reply BYTEA NOT NULL,
  PRIMARY KEY (tenant_id, key)
)";

/// Advisory lock key held while installing the schema ("o5s\0", 1), in the
/// two-int key space so it never equals a tenant's bigint lock.
const SCHEMA_LOCK: (i32, i32) = (0x6f35_7300, 1);

/// `schema` quoted; plain lowercase names only.
pub fn ident(schema: &str) -> Result<String, DbError> {
    let ok = !schema.is_empty()
        && schema.len() <= 63
        && schema
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !schema.as_bytes()[0].is_ascii_digit();
    if ok {
        Ok(format!("\"{schema}\""))
    } else {
        Err(DbError::Decode(format!("bad schema name {schema:?}")))
    }
}

/// The statements that create the schema and its tables; idempotent.
pub fn ddl(schema: &str) -> Result<Vec<String>, DbError> {
    let name = ident(schema)?;
    let tables = tables();
    if !h5i_app_pgsql::valid(&tables) {
        return Err(DbError::Decode("invalid table schema".into()));
    }
    let mut out = vec![
        format!("CREATE SCHEMA IF NOT EXISTS {name}"),
        format!("SET LOCAL search_path TO {name}"),
    ];
    out.push(IDEMPOTENCY_DDL.into());
    for t in &tables {
        out.push(text(&h5i_app_pgsql::create(t))?);
    }
    Ok(out)
}

#[derive(Default, Debug)]
pub struct Stats {
    pub attempts: AtomicU64,
    pub retries: AtomicU64,
    pub commits: AtomicU64,
    pub replays: AtomicU64,
}

/// What a stored idempotency key protects.
pub struct Idem<'a> {
    pub key: &'a str,
    pub fingerprint: Vec<u8>,
}

pub enum Outcome {
    Done(Result<kernel::Reply, kernel::Error>),
    Replayed(Vec<u8>),
}

/// A command waiting for its repository's next transaction.
struct Queued {
    principal: kernel::Principal,
    cmd: kernel::Command,
    idem: Option<(String, Vec<u8>)>,
    encode: Box<dyn Fn(&kernel::Reply) -> Vec<u8> + Send + Sync>,
    done: futures::channel::oneshot::Sender<Result<Outcome, DbError>>,
}

/// At most this many commands share one transaction.
const MAX_GROUP: usize = 32;

pub struct Engine<C: Connector, R: Runtime> {
    connector: C,
    runtime: R,
    schema: String,
    idle: Mutex<Vec<Client>>,
    permits: async_lock::Semaphore,
    pub max_attempts: u32,
    pub stats: Stats,
    /// Run the commands waiting for one repository's lock in one
    /// transaction: one `BEGIN`, lock and `COMMIT` for up to `MAX_GROUP`.
    pub group_commit: bool,
    /// Per repository tenant: whether a leader is draining it, and the queue.
    groups: Mutex<std::collections::HashMap<i64, (bool, Vec<Queued>)>>,
}

/// A connection checked out of the engine; returned on drop unless closed.
struct Lease<'a, C: Connector, R: Runtime> {
    engine: &'a Engine<C, R>,
    client: Option<Client>,
    _permit: async_lock::SemaphoreGuard<'a>,
}

impl<C: Connector, R: Runtime> Drop for Lease<'_, C, R> {
    fn drop(&mut self) {
        if let Some(c) = self.client.take()
            && !c.is_closed()
        {
            self.engine.idle.lock().unwrap().push(c);
        }
    }
}

impl<C: Connector, R: Runtime> Lease<'_, C, R> {
    fn client(&self) -> &Client {
        self.client.as_ref().expect("leased client")
    }
}

impl<C: Connector, R: Runtime> Engine<C, R> {
    /// At most `connections` connections at a time; the tables live in `schema`.
    pub fn new(
        connector: C,
        runtime: R,
        schema: &str,
        connections: usize,
    ) -> Result<Self, DbError> {
        ident(schema)?;
        Ok(Engine {
            connector,
            runtime,
            schema: schema.into(),
            idle: Mutex::new(Vec::new()),
            permits: async_lock::Semaphore::new(connections.max(1)),
            max_attempts: 64,
            stats: Stats::default(),
            group_commit: false,
            groups: Mutex::default(),
        })
    }

    async fn lease(&self) -> Result<Lease<'_, C, R>, DbError> {
        let permit = self.permits.acquire().await;
        let idle = self.idle.lock().unwrap().pop();
        let client = match idle {
            Some(c) if !c.is_closed() => c,
            _ => self.connector.connect().await?,
        };
        Ok(Lease {
            engine: self,
            client: Some(client),
            _permit: permit,
        })
    }

    /// Create the schema and tables; idempotent and safe to race.
    pub async fn install(&self) -> Result<(), DbError> {
        let lease = self.lease().await?;
        let client = lease.client();
        client.batch_execute("BEGIN").await?;
        let result = async {
            client
                .batch_execute(&format!(
                    "SELECT pg_advisory_xact_lock({}, {})",
                    SCHEMA_LOCK.0, SCHEMA_LOCK.1
                ))
                .await?;
            for statement in ddl(&self.schema)? {
                client.batch_execute(&statement).await?;
            }
            client.batch_execute("COMMIT").await?;
            Ok::<_, DbError>(())
        }
        .await;
        if result.is_err() {
            let _ = client.batch_execute("ROLLBACK").await;
        }
        result
    }

    /// The rows `load` names, in a consistent read-only snapshot.
    pub async fn snapshot(
        &self,
        scope: u64,
        cmd: &kernel::Command,
    ) -> Result<kernel::Snapshot, DbError> {
        let tid = tenant(scope)?;
        for attempt in 0..self.max_attempts {
            let lease = self.lease().await?;
            let client = lease.client();
            let result = async {
                client
                    .batch_execute(&format!(
                        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL search_path TO {}",
                        ident(&self.schema)?
                    ))
                    .await?;
                let snap = service::load(&Tx { client, tid }, cmd).await?;
                client.batch_execute("COMMIT").await?;
                Ok::<_, DbError>(snap)
            }
            .await;
            match result {
                Ok(s) => return Ok(s),
                Err(e) if e.is_retryable() => {
                    drop(lease);
                    self.runtime.sleep(backoff(attempt)).await;
                }
                Err(e) => {
                    let _ = client.batch_execute("ROLLBACK").await;
                    return Err(e);
                }
            }
        }
        Err(DbError::RetriesExhausted)
    }

    /// Every row of one table of a tenant, in a read-only snapshot.
    pub async fn table(&self, scope: u64, table: u32) -> Result<Vec<Vec<Val>>, DbError> {
        let tid = tenant(scope)?;
        let lease = self.lease().await?;
        let client = lease.client();
        client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL search_path TO {}",
                ident(&self.schema)?
            ))
            .await?;
        let rows = Tx { client, tid }.rows(table, None).await;
        let _ = client
            .batch_execute(if rows.is_ok() { "COMMIT" } else { "ROLLBACK" })
            .await;
        rows
    }

    /// Run `cmd` for `actor` in a locked transaction, retrying from BEGIN
    /// when it certainly did not commit. With `idem`, a repeat returns the
    /// stored reply bytes instead of running again. With `group_commit`,
    /// commands that arrive while their repository's transaction runs share
    /// the next one; each still runs on the rows the earlier ones left.
    pub async fn execute(
        &self,
        actor: &kernel::Principal,
        cmd: &kernel::Command,
        idem: Option<Idem<'_>>,
        encode: impl Fn(&kernel::Reply) -> Vec<u8> + Send + Sync + 'static,
    ) -> Result<Outcome, DbError> {
        if !self.group_commit {
            return self.execute_one(actor, cmd, idem, &encode).await;
        }
        let tid = tenant(actor.scope)?;
        let (done, rx) = futures::channel::oneshot::channel();
        let lead = {
            let mut groups = self.groups.lock().unwrap();
            let entry = groups.entry(tid).or_insert_with(|| (false, Vec::new()));
            entry.1.push(Queued {
                principal: *actor,
                cmd: cmd.clone(),
                idem: idem.map(|i| (i.key.to_string(), i.fingerprint)),
                encode: Box::new(encode),
                done,
            });
            !std::mem::replace(&mut entry.0, true)
        };
        if lead {
            self.drain(tid).await;
        }
        rx.await
            .unwrap_or_else(|_| Err(DbError::Connect("grouped transaction dropped".into())))
    }

    /// Run the queue of tenant `tid` in groups until it is empty.
    async fn drain(&self, tid: i64) {
        loop {
            let batch: Vec<Queued> = {
                let mut groups = self.groups.lock().unwrap();
                let Some(entry) = groups.get_mut(&tid) else {
                    return;
                };
                if entry.1.is_empty() {
                    groups.remove(&tid);
                    return;
                }
                let n = entry.1.len().min(MAX_GROUP);
                entry.1.drain(..n).collect()
            };
            self.run_group(tid, batch).await;
        }
    }

    async fn run_group(&self, tid: i64, batch: Vec<Queued>) {
        if batch.len() > 1
            && let Ok(lease) = self.lease().await
        {
            self.stats.attempts.fetch_add(1, Ordering::Relaxed);
            match self.attempt_group(lease.client(), tid, &batch).await {
                Ok(results) => {
                    for (q, r) in batch.into_iter().zip(results) {
                        if let Ok(o) = &r {
                            self.count(o);
                        }
                        let _ = q.done.send(r);
                    }
                    return;
                }
                // The outcome is unknown, so rerunning commands without an
                // idempotency key could apply them twice: report it.
                Err(DbError::CommitUnknown(e)) => {
                    let message = e.to_string();
                    for q in batch {
                        let _ = q.done.send(Err(DbError::Connect(format!(
                            "commit outcome unknown: {message}"
                        ))));
                    }
                    return;
                }
                // Nothing committed: run each command on its own.
                Err(_) => {
                    let _ = lease.client().batch_execute("ROLLBACK").await;
                }
            }
        }
        for q in batch {
            let idem = q.idem.as_ref().map(|(key, fingerprint)| Idem {
                key,
                fingerprint: fingerprint.clone(),
            });
            let r = self
                .execute_one(&q.principal, &q.cmd, idem, &q.encode)
                .await;
            let _ = q.done.send(r);
        }
    }

    fn count(&self, outcome: &Outcome) {
        match outcome {
            Outcome::Done(Ok(_)) => self.stats.commits.fetch_add(1, Ordering::Relaxed),
            Outcome::Replayed(_) => self.stats.replays.fetch_add(1, Ordering::Relaxed),
            Outcome::Done(Err(_)) => 0,
        };
    }

    /// Several commands in one locked transaction, in order. A refused
    /// command writes nothing; a database error rolls back all of them.
    async fn attempt_group(
        &self,
        client: &Client,
        tid: i64,
        batch: &[Queued],
    ) -> Result<Vec<Result<Outcome, DbError>>, DbError> {
        client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL search_path TO {}; SELECT pg_advisory_xact_lock({tid})",
                ident(&self.schema)?
            ))
            .await?;
        let tx = Tx { client, tid };
        let mut out = Vec::with_capacity(batch.len());
        for q in batch {
            if let Some((key, fingerprint)) = &q.idem {
                let stored = client
                    .query_typed(
                        "SELECT fingerprint, reply FROM h5i_idempotency WHERE tenant_id = $1 AND key = $2",
                        &[(&tid, Type::INT8), (key, Type::TEXT)],
                    )
                    .await?;
                if let Some(row) = stored.first() {
                    let fp: Vec<u8> = row.try_get(0)?;
                    out.push(if &fp != fingerprint {
                        Err(DbError::IdempotencyConflict)
                    } else {
                        Ok(Outcome::Replayed(row.try_get(1)?))
                    });
                    continue;
                }
            }
            let snap = service::load(&tx, &q.cmd).await?;
            match kernel::transition(&q.principal, &snap, &q.cmd) {
                Err(refusal) => out.push(Ok(Outcome::Done(Err(refusal)))),
                Ok((ws, reply)) => {
                    store(client, tid, &kernel::sql_writes(&ws)).await?;
                    if let Some((key, fingerprint)) = &q.idem {
                        let bytes = (q.encode)(&reply);
                        client
                            .query_typed(
                                "INSERT INTO h5i_idempotency (tenant_id, key, fingerprint, reply) VALUES ($1, $2, $3, $4)",
                                &[(&tid, Type::INT8), (key, Type::TEXT), (fingerprint, Type::BYTEA), (&bytes, Type::BYTEA)],
                            )
                            .await?;
                    }
                    out.push(Ok(Outcome::Done(Ok(reply))));
                }
            }
        }
        client.batch_execute("COMMIT").await.map_err(commit_error)?;
        Ok(out)
    }

    async fn execute_one(
        &self,
        actor: &kernel::Principal,
        cmd: &kernel::Command,
        idem: Option<Idem<'_>>,
        encode: &(dyn Fn(&kernel::Reply) -> Vec<u8> + Sync),
    ) -> Result<Outcome, DbError> {
        let tid = tenant(actor.scope)?;
        for attempt in 0..self.max_attempts {
            self.stats.attempts.fetch_add(1, Ordering::Relaxed);
            let lease = self.lease().await?;
            let result = self
                .attempt(lease.client(), tid, actor, cmd, idem.as_ref(), encode)
                .await;
            match result {
                Ok(outcome) => {
                    match &outcome {
                        Outcome::Done(Ok(_)) => self.stats.commits.fetch_add(1, Ordering::Relaxed),
                        Outcome::Replayed(_) => self.stats.replays.fetch_add(1, Ordering::Relaxed),
                        Outcome::Done(Err(_)) => 0,
                    };
                    return Ok(outcome);
                }
                // With a key, a retry after an unknown commit replays or reruns safely.
                Err(e)
                    if e.is_retryable()
                        || (idem.is_some()
                            && (e.code() == Some(&SqlState::UNIQUE_VIOLATION)
                                || matches!(e, DbError::CommitUnknown(_)))) =>
                {
                    let _ = lease.client().batch_execute("ROLLBACK").await;
                    drop(lease);
                    self.stats.retries.fetch_add(1, Ordering::Relaxed);
                    self.runtime.sleep(backoff(attempt)).await;
                }
                Err(e) => {
                    let _ = lease.client().batch_execute("ROLLBACK").await;
                    return Err(e);
                }
            }
        }
        Err(DbError::RetriesExhausted)
    }

    async fn attempt(
        &self,
        client: &Client,
        tid: i64,
        actor: &kernel::Principal,
        cmd: &kernel::Command,
        idem: Option<&Idem<'_>>,
        encode: &(dyn Fn(&kernel::Reply) -> Vec<u8> + Sync),
    ) -> Result<Outcome, DbError> {
        // One flush: BEGIN and the lock, the idempotency lookup and the
        // command's loads. PostgreSQL runs them in order on this connection,
        // so every read happens under the lock.
        let begin = format!(
            "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL search_path TO {}; SELECT pg_advisory_xact_lock({tid})",
            ident(&self.schema)?
        );
        let tx = Tx { client, tid };
        let lookup = async {
            match idem {
                Some(idem) => client
                    .query_typed(
                        "SELECT fingerprint, reply FROM h5i_idempotency WHERE tenant_id = $1 AND key = $2",
                        &[(&tid, Type::INT8), (&idem.key, Type::TEXT)],
                    )
                    .await
                    .map(Some),
                None => Ok(None),
            }
        };
        let (begun, stored, snap) = futures::join!(
            client.batch_execute(&begin),
            lookup,
            service::load(&tx, cmd)
        );
        begun?;
        if let (Some(idem), Some(row)) = (idem, stored?.as_ref().and_then(|r| r.first())) {
            let fp: Vec<u8> = row.try_get(0)?;
            if fp != idem.fingerprint {
                let _ = client.batch_execute("ROLLBACK").await;
                return Err(DbError::IdempotencyConflict);
            }
            let bytes: Vec<u8> = row.try_get(1)?;
            client.batch_execute("COMMIT").await?;
            return Ok(Outcome::Replayed(bytes));
        }
        match kernel::transition(actor, &snap?, cmd) {
            Err(refusal) => {
                client.batch_execute("ROLLBACK").await?;
                Ok(Outcome::Done(Err(refusal)))
            }
            Ok((ws, reply)) => {
                // One flush: the writes, the idempotency record and COMMIT.
                // If a write fails, PostgreSQL turns the COMMIT into a
                // rollback, and the write's error is what is returned.
                let bytes = encode(&reply);
                let writes = kernel::sql_writes(&ws);
                let record = async {
                    match idem {
                        Some(idem) => client
                            .query_typed(
                                "INSERT INTO h5i_idempotency (tenant_id, key, fingerprint, reply) VALUES ($1, $2, $3, $4)",
                                &[(&tid, Type::INT8), (&idem.key, Type::TEXT), (&idem.fingerprint, Type::BYTEA), (&bytes, Type::BYTEA)],
                            )
                            .await
                            .map(|_| ()),
                        None => Ok(()),
                    }
                };
                let (stored, recorded, committed) = futures::join!(
                    store(client, tid, &writes),
                    record,
                    client.batch_execute("COMMIT")
                );
                stored?;
                recorded?;
                committed.map_err(commit_error)?;
                Ok(Outcome::Done(Ok(reply)))
            }
        }
    }
}

fn tenant(scope: u64) -> Result<i64, DbError> {
    i64::try_from(scope).map_err(|_| DbError::Decode(format!("scope {scope} out of range")))
}

fn backoff(attempt: u32) -> Duration {
    let cap_us = 1000u64 << attempt.min(8);
    // Jitter from a counter is enough to spread retries; no clock is needed on Workers.
    static SPREAD: AtomicU64 = AtomicU64::new(0x9e37_79b9);
    let n = SPREAD.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    Duration::from_micros(n % cap_us + 100)
}

/// One open transaction on one tenant: the row source of `service::load`.
pub struct Tx<'a> {
    client: &'a Client,
    tid: i64,
}

impl Tx<'_> {
    /// The tenant's rows of `table`, optionally where column `filter.0` is not
    /// distinct from `filter.1`, in key order.
    pub async fn rows(
        &self,
        table: u32,
        filter: Option<(u32, Val)>,
    ) -> Result<Vec<Vec<Val>>, DbError> {
        let tables = tables();
        let Some(q) = h5i_app_pgsql::select(&tables, self.tid, table, filter) else {
            return Err(DbError::Decode(format!(
                "no table {table} or column to filter on"
            )));
        };
        let (sql, vals) = bind(&q)?;
        let kinds: Vec<Kind> = tables[table as usize]
            .columns
            .iter()
            .map(|c| c.kind)
            .collect();
        let rows = self.client.query_typed(&sql, &typed(&vals)).await?;
        rows.iter().map(|r| read_row(&kinds, r)).collect()
    }
}

/// Plan (`h5i_app_sql::plan`), compile (`h5i_app_pgsql::compile`) and run each
/// write in order. The statements are pipelined on the connection.
async fn store(client: &Client, tid: i64, ws: &Vec<Write>) -> Result<(), DbError> {
    let tables = tables();
    let mut statements = Vec::new();
    for stmt in h5i_app_sql::plan(ws) {
        let Some(q) = h5i_app_pgsql::compile(&tables, tid, &stmt) else {
            return Err(DbError::Decode(format!(
                "statement does not fit the schema: {stmt:?}"
            )));
        };
        statements.push(bind(&q)?);
    }
    let pending = statements.iter().map(|(sql, vals)| async move {
        let params = typed(vals);
        client.query_typed(sql, &params).await
    });
    for result in futures::future::join_all(pending).await {
        result?;
    }
    Ok(())
}

/// A column value as the driver sends it.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Int(i64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
    Null,
}

impl ToSql for Value {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            Value::Int(v) => v.to_sql(ty, out),
            Value::Bool(v) => v.to_sql(ty, out),
            Value::Text(v) => v.to_sql(ty, out),
            Value::Bytes(v) => v.to_sql(ty, out),
            Value::Null => Ok(IsNull::Yes),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(
            *ty,
            Type::INT8 | Type::BOOL | Type::TEXT | Type::BYTEA | Type::UNKNOWN
        )
    }

    to_sql_checked!();
}

fn text(sql: &h5i_app_pgsql::Sql) -> Result<String, DbError> {
    String::from_utf8(h5i_app_pgsql::render(sql)).map_err(|e| DbError::Decode(e.to_string()))
}

fn bind(q: &Query) -> Result<(String, Vec<Value>), DbError> {
    Ok((
        text(&q.sql)?,
        q.params
            .iter()
            .map(from_val)
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

/// Each parameter with the type it is sent as. `NULL` is sent as `unknown`,
/// for the server to infer from its column.
fn typed(vals: &[Value]) -> Vec<(&(dyn ToSql + Sync), Type)> {
    vals.iter()
        .map(|v| {
            let ty = match v {
                Value::Int(_) => Type::INT8,
                Value::Bool(_) => Type::BOOL,
                Value::Text(_) => Type::TEXT,
                Value::Bytes(_) => Type::BYTEA,
                Value::Null => Type::UNKNOWN,
            };
            (v as &(dyn ToSql + Sync), ty)
        })
        .collect()
}

fn read_row(kinds: &[Kind], row: &Row) -> Result<Vec<Val>, DbError> {
    let mut out = Vec::with_capacity(kinds.len());
    for (i, k) in kinds.iter().enumerate() {
        let v = match k {
            Kind::Int => row.try_get::<_, Option<i64>>(i)?.map(Val::Int),
            Kind::Bool => row.try_get::<_, Option<bool>>(i)?.map(Val::Bool),
            Kind::Text => row
                .try_get::<_, Option<String>>(i)?
                .map(|s| Val::Text(s.into_bytes())),
            Kind::Bytes => row.try_get::<_, Option<Vec<u8>>>(i)?.map(Val::Bytes),
        };
        out.push(v.unwrap_or(Val::Null));
    }
    Ok(out)
}

fn from_val(v: &Val) -> Result<Value, DbError> {
    Ok(match v {
        Val::Int(i) => Value::Int(*i),
        Val::Bool(b) => Value::Bool(*b),
        Val::Text(b) => {
            Value::Text(String::from_utf8(b.clone()).map_err(|e| DbError::Decode(e.to_string()))?)
        }
        Val::Bytes(b) => Value::Bytes(b.clone()),
        Val::Null => Value::Null,
    })
}

/// Native connections and runtime.
#[cfg(feature = "native")]
pub mod native {
    use super::*;

    pub struct Tcp {
        pub url: String,
    }

    impl Connector for Tcp {
        async fn connect(&self) -> Result<Client, DbError> {
            let (client, connection) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
                .await
                .map_err(|e| DbError::Connect(e.to_string()))?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            Ok(client)
        }
    }

    #[derive(Clone, Copy)]
    pub struct Tokio;

    impl Runtime for Tokio {
        async fn sleep(&self, d: Duration) {
            tokio::time::sleep(d).await
        }
    }

    pub type NativeEngine = Engine<Tcp, Tokio>;

    /// An engine on `url` with tables in `schema`, after creating them.
    pub async fn connect(
        url: &str,
        schema: &str,
        connections: usize,
    ) -> Result<NativeEngine, DbError> {
        let engine = Engine::new(Tcp { url: url.into() }, Tokio, schema, connections)?;
        engine.install().await?;
        Ok(engine)
    }
}
