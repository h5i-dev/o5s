//! A PostgreSQL engine for h5i-app kernels on Workers, after `h5i_app_pg::Engine`.
//!
//! Each request runs in one SERIALIZABLE transaction: load the tenant, call
//! `transition`, write, commit. Serialization failures restart from `BEGIN`.
//! With an idempotency key a command runs at most once; without one, a
//! connection lost during `COMMIT` is reported as [`DbError::CommitUnknown`].
//!
//! The connection goes through Hyperdrive on a Workers `Socket`. Hyperdrive
//! pools connections per transaction and has no advisory locks, so unlike
//! `h5i_app_pg` there is no tenant lock (SERIALIZABLE alone gives correctness)
//! and the schema is chosen per transaction with `SET LOCAL search_path`.
//!
//! All SQL on tenant tables comes from `h5i_app_pgsql` (extracted to Lean);
//! this file builds none.

use h5i_app_core::{Kernel, TenantId, Timestamp};
use h5i_app_pgsql::{Kind, Query, Table};
use h5i_app_sql::{Val, Write};
use std::marker::PhantomData;
use std::time::Duration;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};
use worker::postgres_tls::PassthroughTls;
use worker::{Hyperdrive, SecureTransport, Socket};

/// Maps snapshots and write sets to tables, as `h5i_app_pg::Store` with `schema!`.
pub trait Store<K: Kernel> {
    /// Every table, numbered like the kernel's `TABLE`.
    fn tables() -> Vec<Table>;
    /// The snapshot that the tenant's rows of each table, in order, stand for.
    fn load(rows: Vec<Vec<Vec<Val>>>) -> Option<K::Snapshot>;
    /// The table writes for a write set; a later load must decode to `K::apply(before, ws)`.
    fn writes(ws: &K::WriteSet) -> Vec<Write>;
}

/// Needed to replay stored replies for idempotency keys.
pub trait ReplyCodec<K: Kernel> {
    /// Rejects a key reused for a different command. Leave out the time, since retries arrive later.
    fn fingerprint(cmd: &K::Command) -> Vec<u8>;
    /// Owner of a key, so one user cannot replay another's reply. Must not contain `/`.
    fn scope(actor: &K::Principal) -> String;
    fn encode(reply: &K::Reply) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<K::Reply, String>;
}

/// JSON in and out, as `h5i_app_http::Api`.
pub trait Api<K: Kernel> {
    fn decode_command(body: &[u8]) -> Result<K::Command, String>;
    fn encode_reply(reply: &K::Reply) -> h5i_app_json::Value;
    fn encode_error(err: &K::Error) -> (axum::http::StatusCode, h5i_app_json::Value);
}

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
            DbError::Postgres(e) => write!(f, "postgres: {e}"),
            DbError::Connect(e) => write!(f, "connect: {e}"),
            DbError::Decode(e) => write!(f, "decode: {e}"),
            DbError::IdempotencyConflict => write!(f, "idempotency key reused for a different command"),
            DbError::RetriesExhausted => write!(f, "too many serialization failures"),
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
            _ => false,
        }
    }
}

/// No server error code on COMMIT: the connection dropped and COMMIT may have landed.
fn commit_error(e: tokio_postgres::Error) -> DbError {
    if e.code().is_some() { DbError::Postgres(e) } else { DbError::CommitUnknown(e) }
}

/// A new connection through Hyperdrive, which pools the real ones; open one per request.
pub async fn connect(hd: &Hyperdrive) -> Result<Client, DbError> {
    let socket = Socket::builder()
        .secure_transport(SecureTransport::StartTls)
        .connect(hd.host(), hd.port())
        .map_err(|e| DbError::Connect(e.to_string()))?;
    let config = hd.connection_string().parse::<tokio_postgres::Config>()?;
    let (client, connection) = config.connect_raw(socket, PassthroughTls).await?;
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = connection.await {
            worker::console_error!("postgres connection: {e}");
        }
    });
    Ok(client)
}

/// The engine's own table, as in `h5i_app_pg`.
pub const FRAMEWORK_DDL: &str = "CREATE TABLE IF NOT EXISTS h5i_idempotency (
  tenant_id BIGINT NOT NULL,
  key TEXT NOT NULL,
  fingerprint BYTEA NOT NULL,
  reply BYTEA NOT NULL,
  PRIMARY KEY (tenant_id, key)
)";

/// The statements that create `schema` and its tables. Run them on the
/// database directly: Hyperdrive serves only the engine's transactions.
pub fn ddl<K: Kernel, S: Store<K>>(schema: &str) -> Result<Vec<String>, DbError> {
    let tables = S::tables();
    if !h5i_app_pgsql::valid(&tables) {
        return Err(DbError::Decode("invalid table schema".into()));
    }
    let mut out = vec![format!("CREATE SCHEMA IF NOT EXISTS {}", ident(schema)?), format!("SET search_path TO {}", ident(schema)?)];
    out.push(FRAMEWORK_DDL.to_string());
    for t in &tables {
        out.push(text(&h5i_app_pgsql::create(t))?);
    }
    Ok(out)
}

/// `schema` quoted; plain lowercase names only, like `h5i_app_pg::with_schema`.
fn ident(schema: &str) -> Result<String, DbError> {
    let ok = !schema.is_empty()
        && schema.len() <= 63
        && schema.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !schema.as_bytes()[0].is_ascii_digit();
    if ok { Ok(format!("\"{schema}\"")) } else { Err(DbError::Decode(format!("bad schema name {schema:?}"))) }
}

struct Idem<'a, K: Kernel> {
    key: &'a str,
    fingerprint: Vec<u8>,
    encode: fn(&K::Reply) -> Vec<u8>,
    decode: fn(&[u8]) -> Result<K::Reply, String>,
}

enum Attempt<K: Kernel> {
    Done(Result<K::Reply, K::Error>),
    Replayed(K::Reply),
}

pub struct Engine<K: Kernel, S: Store<K>> {
    client: Client,
    hyperdrive: Hyperdrive,
    /// `SET LOCAL search_path` text for this app's schema.
    set_schema: String,
    max_attempts: u32,
    _marker: PhantomData<fn() -> (K, S)>,
}

impl<K: Kernel, S: Store<K>> Engine<K, S> {
    /// Connects through `hyperdrive`; the app's tables live in PostgreSQL schema `schema`.
    pub async fn connect(hyperdrive: Hyperdrive, schema: &str) -> Result<Self, DbError> {
        let set_schema = format!("SET LOCAL search_path TO {}", ident(schema)?);
        let client = connect(&hyperdrive).await?;
        Ok(Engine { client, hyperdrive, set_schema, max_attempts: 20, _marker: PhantomData })
    }

    /// Outer error: infrastructure. Inner error: the kernel refused.
    pub async fn execute(&mut self, actor: &K::Principal, cmd: &K::Command) -> Result<Result<K::Reply, K::Error>, DbError> {
        self.run(actor, cmd, None).await
    }

    /// At most once per `(tenant, scope, key)`; a repeat gets the stored reply.
    /// Refusals are not stored.
    pub async fn execute_idempotent(
        &mut self,
        actor: &K::Principal,
        key: &str,
        cmd: &K::Command,
    ) -> Result<Result<K::Reply, K::Error>, DbError>
    where
        S: ReplyCodec<K>,
    {
        let scope = S::scope(actor);
        if scope.contains('/') {
            return Err(DbError::Decode(format!("idempotency scope {scope:?} contains '/'")));
        }
        let key = format!("{scope}/{key}");
        let idem = Idem { key: &key, fingerprint: S::fingerprint(cmd), encode: S::encode, decode: S::decode };
        self.run(actor, cmd, Some(idem)).await
    }

    async fn run(&mut self, actor: &K::Principal, cmd: &K::Command, idem: Option<Idem<'_, K>>) -> Result<Result<K::Reply, K::Error>, DbError> {
        let tenant = K::tenant(actor);
        for attempt in 0..self.max_attempts {
            if self.client.is_closed() {
                self.client = connect(&self.hyperdrive).await?;
            }
            match self.attempt(tenant, actor, cmd, idem.as_ref()).await {
                Ok(Attempt::Done(r)) => return Ok(r),
                Ok(Attempt::Replayed(r)) => return Ok(Ok(r)),
                // With a key, a retry after an unknown commit replays or reruns safely.
                Err(e)
                    if e.is_retryable()
                        || (idem.is_some()
                            && (e.code() == Some(&SqlState::UNIQUE_VIOLATION) || matches!(e, DbError::CommitUnknown(_)))) =>
                {
                    worker::console_debug!("retrying transaction (attempt {attempt}): {e}");
                    worker::Delay::from(backoff(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(DbError::RetriesExhausted)
    }

    async fn attempt(
        &mut self,
        tenant: TenantId,
        actor: &K::Principal,
        cmd: &K::Command,
        idem: Option<&Idem<'_, K>>,
    ) -> Result<Attempt<K>, DbError> {
        let tid = tenant_param(tenant)?;
        let tables = S::tables();
        let tx = self.client.build_transaction().isolation_level(IsolationLevel::Serializable).start().await?;
        tx.batch_execute(&self.set_schema).await?;
        if let Some(idem) = idem {
            let stored = tx
                .query_opt("SELECT fingerprint, reply FROM h5i_idempotency WHERE tenant_id = $1 AND key = $2", &[&tid, &idem.key])
                .await?;
            if let Some(row) = stored {
                let fp: Vec<u8> = row.try_get(0)?;
                if fp != idem.fingerprint {
                    return Err(DbError::IdempotencyConflict);
                }
                let bytes: Vec<u8> = row.try_get(1)?;
                let reply = (idem.decode)(&bytes).map_err(DbError::Decode)?;
                tx.commit().await?;
                return Ok(Attempt::Replayed(reply));
            }
        }
        let snap = load::<K, S>(&tx, &tables, tid).await?;
        let mut actor = actor.clone();
        K::stamp(&mut actor, now());
        let decision = K::transition(&actor, &snap, cmd);
        // Replies are only comparable when a codec exists (keyed requests).
        let encoded = match (&decision, idem) {
            (Ok((_, reply)), Some(idem)) => Some((idem.encode)(reply)),
            _ => None,
        };
        match decision {
            Err(refusal) => {
                tx.rollback().await?;
                Ok(Attempt::Done(Err(refusal)))
            }
            Ok((ws, reply)) => {
                store(&tx, &tables, tid, &S::writes(&ws)).await?;
                if let (Some(idem), Some(bytes)) = (idem, &encoded) {
                    tx.execute(
                        "INSERT INTO h5i_idempotency (tenant_id, key, fingerprint, reply) VALUES ($1, $2, $3, $4)",
                        &[&tid, &idem.key, &idem.fingerprint, bytes],
                    )
                    .await?;
                }
                tx.commit().await.map_err(commit_error)?;
                Ok(Attempt::Done(Ok(reply)))
            }
        }
    }
}

/// Every table of the tenant, decoded.
async fn load<K: Kernel, S: Store<K>>(tx: &Transaction<'_>, tables: &Vec<Table>, tid: i64) -> Result<K::Snapshot, DbError> {
    let mut rows = Vec::with_capacity(tables.len());
    for i in 0..tables.len() {
        rows.push(load_table(tx, tables, tid, i as u32).await?);
    }
    S::load(rows).ok_or_else(|| DbError::Decode("stored rows do not decode".into()))
}

/// The tenant's rows of table `table`, without `tenant_id`, in key order.
async fn load_table(tx: &Transaction<'_>, tables: &Vec<Table>, tid: i64, table: u32) -> Result<Vec<Vec<Val>>, DbError> {
    let Some(q) = h5i_app_pgsql::select(tables, tid, table, None) else {
        return Err(DbError::Decode(format!("no table {table}, or an invalid schema")));
    };
    let (sql, vals) = bind(&q)?;
    let kinds: Vec<Kind> = tables[table as usize].columns.iter().map(|c| c.kind).collect();
    let rows = tx.query(&sql, &refs(&vals)).await?;
    rows.iter().map(|r| read_row(&kinds, r)).collect()
}

/// Plan (`h5i_app_sql::plan`), compile (`h5i_app_pgsql::compile`) and run each write in
/// order. A statement that does not fit the schema fails the transaction.
async fn store(tx: &Transaction<'_>, tables: &Vec<Table>, tid: i64, ws: &Vec<Write>) -> Result<(), DbError> {
    for stmt in h5i_app_sql::plan(ws) {
        let Some(q) = h5i_app_pgsql::compile(tables, tid, &stmt) else {
            return Err(DbError::Decode(format!("statement does not fit the schema: {stmt:?}")));
        };
        let (sql, vals) = bind(&q)?;
        tx.execute(&sql, &refs(&vals)).await?;
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
    fn to_sql(&self, ty: &Type, out: &mut bytes::BytesMut) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            Value::Int(v) => v.to_sql(ty, out),
            Value::Bool(v) => v.to_sql(ty, out),
            Value::Text(v) => v.to_sql(ty, out),
            Value::Bytes(v) => v.to_sql(ty, out),
            Value::Null => Ok(IsNull::Yes),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::INT8 | Type::BOOL | Type::TEXT | Type::BYTEA)
    }

    to_sql_checked!();
}

fn text(sql: &h5i_app_pgsql::Sql) -> Result<String, DbError> {
    String::from_utf8(h5i_app_pgsql::render(sql)).map_err(|e| DbError::Decode(e.to_string()))
}

/// `q`'s rendered text and its parameters as driver values.
fn bind(q: &Query) -> Result<(String, Vec<Value>), DbError> {
    Ok((text(&q.sql)?, q.params.iter().map(from_val).collect::<Result<Vec<_>, _>>()?))
}

fn refs(vals: &[Value]) -> Vec<&(dyn ToSql + Sync)> {
    vals.iter().map(|v| v as &(dyn ToSql + Sync)).collect()
}

fn read_row(kinds: &[Kind], row: &Row) -> Result<Vec<Val>, DbError> {
    let mut out = Vec::with_capacity(kinds.len());
    for (i, k) in kinds.iter().enumerate() {
        let v = match k {
            Kind::Int => row.try_get::<_, Option<i64>>(i)?.map(Val::Int),
            Kind::Bool => row.try_get::<_, Option<bool>>(i)?.map(Val::Bool),
            Kind::Text => row.try_get::<_, Option<String>>(i)?.map(|s| Val::Text(s.into_bytes())),
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
        Val::Text(b) => Value::Text(String::from_utf8(b.clone()).map_err(|e| DbError::Decode(e.to_string()))?),
        Val::Bytes(b) => Value::Bytes(b.clone()),
        Val::Null => Value::Null,
    })
}

fn tenant_param(t: TenantId) -> Result<i64, DbError> {
    i64::try_from(t.0).map_err(|_| DbError::Decode(format!("tenant id {} out of range", t.0)))
}

/// The JavaScript clock: `SystemTime::now` panics on `wasm32-unknown-unknown`.
pub fn now() -> Timestamp {
    Timestamp(worker::Date::now().as_millis().saturating_mul(1000))
}

fn backoff(attempt: u32) -> Duration {
    let cap_ms = 1u64 << attempt.min(8);
    let jitter = (worker::js_sys::Math::random() * (cap_ms * 1000) as f64) as u64;
    Duration::from_micros(jitter + 100)
}
