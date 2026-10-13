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

pub mod outbox;
pub mod revoke;
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
    /// Microseconds on a monotonic clock, for `Stats`; 0 where there is none.
    fn now_us(&self) -> u64 {
        0
    }
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
        table("o5s_roles", 1, vec![int("user"), int("level")]),
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
    out.push(outbox::DDL.into());
    out.extend(revoke::DDL.iter().map(|d| d.to_string()));
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
    /// Grouped transactions that committed, and the commands in them.
    pub groups: AtomicU64,
    pub grouped: AtomicU64,
    /// Where a committed group's time went, in microseconds: `BEGIN` and
    /// the lock, idempotency lookups, loads, kernel transitions, writes,
    /// idempotency records and `COMMIT`. All but the first hold the lock.
    pub group_us: [AtomicU64; 7],
    /// Write statements sent by committed groups.
    pub group_writes: AtomicU64,
}

/// The phases of `Stats::group_us`, in order.
pub const GROUP_PHASES: [&str; 7] = [
    "begin_lock",
    "idempotency_lookup",
    "load",
    "kernel",
    "write",
    "idempotency_record",
    "commit",
];

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
    outbox: Option<outbox::Outbox>,
    encode: Box<dyn Fn(&kernel::Reply) -> Vec<u8> + Send + Sync>,
    done: futures::channel::oneshot::Sender<Result<Outcome, DbError>>,
}

/// At most this many commands share one transaction.
const MAX_GROUP: usize = 32;

pub struct Engine<C: Connector, R: Runtime> {
    connector: C,
    runtime: R,
    schema: String,
    idle: Mutex<Vec<Conn>>,
    permits: async_lock::Semaphore,
    pub max_attempts: u32,
    pub stats: Stats,
    /// Run the commands waiting for one repository's lock in one
    /// transaction: one `BEGIN`, lock and `COMMIT` for up to `MAX_GROUP`.
    pub group_commit: bool,
    /// Per repository tenant: whether a leader is draining it, and the queue.
    groups: Mutex<std::collections::HashMap<i64, (bool, Vec<Queued>)>>,
    /// Keep each connection's statements prepared (named), so PostgreSQL
    /// parses and plans each statement shape once per connection instead of
    /// on every execution. Off behind transaction-mode poolers (Hyperdrive),
    /// which may run a later transaction on another server connection.
    pub prepared: bool,
    /// Record each successful admission's channel in `o5s_export_pending`
    /// within its transaction (the Worker, which exports after replying).
    pub outbox: bool,
}

/// A pooled connection and, if enabled, its prepared statements by SQL text
/// and parameter types.
pub struct Conn {
    client: Client,
    prepared:
        Option<Mutex<std::collections::HashMap<(String, Vec<Type>), tokio_postgres::Statement>>>,
}

impl std::ops::Deref for Conn {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}

impl Conn {
    fn new(client: Client, prepared: bool) -> Self {
        Self {
            client,
            prepared: prepared.then(Mutex::default),
        }
    }

    /// Run `sql` with typed parameters: as a prepared statement when enabled
    /// and already prepared, else unnamed. A statement is sent when the
    /// returned future is first polled, so callers can pipeline it with
    /// others in order (writes before `COMMIT`); on a miss it therefore runs
    /// unnamed and is prepared alongside for later calls, never awaiting the
    /// prepare first. A failure forgets the prepared statements, in case the
    /// schema changed under them.
    async fn query(
        &self,
        sql: &str,
        params: &[(&(dyn ToSql + Sync), Type)],
    ) -> Result<Vec<Row>, tokio_postgres::Error> {
        let Some(cache) = &self.prepared else {
            return self.client.query_typed(sql, params).await;
        };
        let types: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
        let key = (sql.to_string(), types);
        let known = cache.lock().unwrap().get(&key).cloned();
        let Some(statement) = known else {
            let (result, prepared) = futures::join!(
                self.client.query_typed(sql, params),
                self.client.prepare_typed(sql, &key.1)
            );
            match (&result, prepared) {
                (Ok(_), Ok(s)) => {
                    cache.lock().unwrap().insert(key, s);
                }
                (Err(_), _) => cache.lock().unwrap().clear(),
                _ => {}
            }
            return result;
        };
        let values: Vec<&(dyn ToSql + Sync)> = params.iter().map(|(v, _)| *v).collect();
        let result = self.client.query(&statement, &values).await;
        if result.is_err() {
            cache.lock().unwrap().clear();
        }
        result
    }
}

/// A connection checked out of the engine; returned on drop unless closed.
struct Lease<'a, C: Connector, R: Runtime> {
    engine: &'a Engine<C, R>,
    client: Option<Conn>,
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
    fn client(&self) -> &Conn {
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
            prepared: false,
            outbox: false,
        })
    }

    async fn lease(&self) -> Result<Lease<'_, C, R>, DbError> {
        let permit = self.permits.acquire().await;
        let idle = self.idle.lock().unwrap().pop();
        let client = match idle {
            Some(c) if !c.is_closed() => c,
            _ => Conn::new(self.connector.connect().await?, self.prepared),
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
        outbox: Option<outbox::Outbox>,
        encode: impl Fn(&kernel::Reply) -> Vec<u8> + Send + Sync + 'static,
    ) -> Result<Outcome, DbError> {
        let outbox = outbox.filter(|_| self.outbox);
        if !self.group_commit {
            return self
                .execute_one(actor, cmd, idem, outbox.as_ref(), &encode)
                .await;
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
                outbox,
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
                .execute_one(&q.principal, &q.cmd, idem, q.outbox.as_ref(), &q.encode)
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
        client: &Conn,
        tid: i64,
        batch: &[Queued],
    ) -> Result<Vec<Result<Outcome, DbError>>, DbError> {
        let mut us = [0u64; 7];
        let mut writes = 0u64;
        let mut mark = self.runtime.now_us();
        let mut lap = |phase: usize, us: &mut [u64; 7]| {
            let now = self.runtime.now_us();
            us[phase] += now.saturating_sub(mark);
            mark = now;
        };
        client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL search_path TO {}; SELECT pg_advisory_xact_lock({tid})",
                ident(&self.schema)?
            ))
            .await?;
        lap(0, &mut us);
        let tx = Tx { client, tid };
        let mut out = Vec::with_capacity(batch.len());
        // The repository row the earlier commands wrote, kept here and stored
        // once at the end; their other writes, and idempotency records, go
        // out in the same flush as the next command's reads, ahead of them
        // (`Group.group_served`, `Group.defer_repo`).
        let mut repository: Option<kernel::Repository> = None;
        let mut pending: Vec<kernel::Write> = Vec::new();
        let mut records: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
        let mut exports: Vec<(outbox::Outbox, u64)> = Vec::new();
        for q in batch {
            let flush = std::mem::take(&mut pending);
            let recorded = std::mem::take(&mut records);
            let lookup = async {
                match &q.idem {
                    Some((key, _)) => client
                        .query(
                            "SELECT fingerprint, reply FROM h5i_idempotency WHERE tenant_id = $1 AND key = $2",
                            &[(&tid, Type::INT8), (key, Type::TEXT)],
                        )
                        .await
                        .map(Some),
                    None => Ok(None),
                }
            };
            let flush = kernel::sql_writes(&flush);
            let (stored, recorded, stored_reply, snap) = futures::join!(
                store(client, tid, &flush),
                record(client, tid, &recorded),
                lookup,
                service::load_with(&tx, &q.cmd, repository.as_ref())
            );
            writes += stored? as u64;
            recorded?;
            lap(2, &mut us);
            if let (Some((_, fingerprint)), Some(row)) =
                (&q.idem, stored_reply?.as_ref().and_then(|r| r.first()))
            {
                let fp: Vec<u8> = row.try_get(0)?;
                out.push(if &fp != fingerprint {
                    Err(DbError::IdempotencyConflict)
                } else {
                    Ok(Outcome::Replayed(row.try_get(1)?))
                });
                continue;
            }
            let snap = snap?;
            let decided = kernel::transition(&q.principal, &snap, &q.cmd);
            lap(3, &mut us);
            match decided {
                Err(refusal) => out.push(Ok(Outcome::Done(Err(refusal)))),
                Ok((ws, reply)) => {
                    for w in ws.iter() {
                        match w {
                            kernel::Write::SetRepository(r) => repository = Some(r.clone()),
                            other => pending.push(other.clone()),
                        }
                    }
                    if let Some((key, fingerprint)) = &q.idem {
                        records.push((key.clone(), fingerprint.clone(), (q.encode)(&reply)));
                    }
                    if let (Some(o), Some(g)) = (&q.outbox, generation(&ws)) {
                        exports.push((o.clone(), g));
                    }
                    out.push(Ok(Outcome::Done(Ok(reply))));
                }
            }
        }
        // The last flush: the remaining writes, the repository row, the
        // idempotency records and COMMIT, in that order.
        let last: Vec<kernel::Write> = repository
            .into_iter()
            .map(kernel::Write::SetRepository)
            .collect();
        let (pending, last) = (kernel::sql_writes(&pending), kernel::sql_writes(&last));
        let (stored, stored_repository, recorded, marked, committed) = futures::join!(
            store(client, tid, &pending),
            store(client, tid, &last),
            record(client, tid, &records),
            mark_exports(client, &exports),
            client.batch_execute("COMMIT")
        );
        writes += stored? as u64 + stored_repository? as u64;
        recorded?;
        marked?;
        committed.map_err(commit_error)?;
        lap(6, &mut us);
        self.stats.groups.fetch_add(1, Ordering::Relaxed);
        self.stats
            .grouped
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        self.stats.group_writes.fetch_add(writes, Ordering::Relaxed);
        for (total, add) in self.stats.group_us.iter().zip(us) {
            total.fetch_add(add, Ordering::Relaxed);
        }
        Ok(out)
    }

    async fn execute_one(
        &self,
        actor: &kernel::Principal,
        cmd: &kernel::Command,
        idem: Option<Idem<'_>>,
        outbox: Option<&outbox::Outbox>,
        encode: &(dyn Fn(&kernel::Reply) -> Vec<u8> + Sync),
    ) -> Result<Outcome, DbError> {
        let tid = tenant(actor.scope)?;
        for attempt in 0..self.max_attempts {
            self.stats.attempts.fetch_add(1, Ordering::Relaxed);
            let lease = self.lease().await?;
            let result = self
                .attempt(
                    lease.client(),
                    tid,
                    actor,
                    cmd,
                    idem.as_ref(),
                    outbox,
                    encode,
                )
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
        client: &Conn,
        tid: i64,
        actor: &kernel::Principal,
        cmd: &kernel::Command,
        idem: Option<&Idem<'_>>,
        outbox: Option<&outbox::Outbox>,
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
                    .query(
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
                            .query(
                                "INSERT INTO h5i_idempotency (tenant_id, key, fingerprint, reply) VALUES ($1, $2, $3, $4)",
                                &[(&tid, Type::INT8), (&idem.key, Type::TEXT), (&idem.fingerprint, Type::BYTEA), (&bytes, Type::BYTEA)],
                            )
                            .await
                            .map(|_| ()),
                        None => Ok(()),
                    }
                };
                let pending: Vec<_> = outbox
                    .and_then(|o| Some((o.clone(), generation(&ws)?)))
                    .into_iter()
                    .collect();
                let (stored, recorded, marked, committed) = futures::join!(
                    store(client, tid, &writes),
                    record,
                    mark_exports(client, &pending),
                    client.batch_execute("COMMIT")
                );
                stored?;
                recorded?;
                marked?;
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
    client: &'a Conn,
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
        let rows = self.client.query(&sql, &typed(&vals)).await?;
        rows.iter().map(|r| read_row(&kinds, r)).collect()
    }
}

/// Plan (`h5i_app_sql::plan`), compile (`h5i_app_pgsql::compile`) and run each
/// write in order. The statements are pipelined on the connection.
/// The generation (repository event counter) a write list reaches, if it
/// writes the repository row.
fn generation(ws: &[kernel::Write]) -> Option<u64> {
    ws.iter().rev().find_map(|w| match w {
        kernel::Write::SetRepository(r) => Some(r.events),
        _ => None,
    })
}

/// Record channels whose projections are now stale (`outbox`), pipelined.
async fn mark_exports(client: &Conn, exports: &[(outbox::Outbox, u64)]) -> Result<(), DbError> {
    let mut params = Vec::with_capacity(exports.len());
    for (o, g) in exports {
        params.push((o, outbox::record_params(o, *g)?));
    }
    let pending = params
        .iter()
        .map(|(o, (org, generation, user))| async move {
            client
                .query(
                    outbox::RECORD,
                    &[
                        (org, Type::INT8),
                        (&o.repo, Type::TEXT),
                        (&o.channel, Type::TEXT),
                        (generation, Type::INT8),
                        (user, Type::INT8),
                    ],
                )
                .await
        });
    for result in futures::future::join_all(pending).await {
        result?;
    }
    Ok(())
}

/// Store idempotency records `(key, fingerprint, reply)`, pipelined.
async fn record(
    client: &Conn,
    tid: i64,
    records: &[(String, Vec<u8>, Vec<u8>)],
) -> Result<(), DbError> {
    let pending = records.iter().map(|(key, fingerprint, reply)| async move {
        client
            .query(
                "INSERT INTO h5i_idempotency (tenant_id, key, fingerprint, reply) VALUES ($1, $2, $3, $4)",
                &[(&tid, Type::INT8), (key, Type::TEXT), (fingerprint, Type::BYTEA), (reply, Type::BYTEA)],
            )
            .await
    });
    for result in futures::future::join_all(pending).await {
        result?;
    }
    Ok(())
}

/// Send `ws` as pipelined statements; returns how many.
async fn store(client: &Conn, tid: i64, ws: &Vec<Write>) -> Result<usize, DbError> {
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
        client.query(sql, &params).await
    });
    for result in futures::future::join_all(pending).await {
        result?;
    }
    Ok(statements.len())
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
        fn now_us(&self) -> u64 {
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            START
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_micros() as u64
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
