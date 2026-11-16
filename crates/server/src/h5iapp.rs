//! The access kernel on `h5i-app-http` (docs/H5I_PLAN.md, H2): an `H5iApp` over o5s's
//! own store instead of PostgreSQL.
//!
//! - The kernel is `o5s_net_kernel::App`, i.e. `h5i_app_core::Authz<Access>`: its
//!   `transition` is `guard_viewed` over the access kernel, proven in `verify/net`
//!   (`guarded_transition.authorized`, noninterference, `read_only_sound`, `panic_free`).
//! - [`NetExecutor`] is the `h5i_app_core::Executor`: it builds the command's snapshot from
//!   core's reachability index (which of the caller's readable cells reach each object) and
//!   runs the kernel. Object and ref reads go through it; commits stay with the store's
//!   name-layer kernel (core), so a `WriteRef` here is refused as unsupported.
//! - Principals: the server's authentication middleware runs first (static and issued tokens,
//!   OIDC; async, unlike h5i-app-http's `Authenticator`), and handlers wrap its result with
//!   `Actor::assume_authenticated`. [`NetAuth`] is the synchronous `Authenticator` `H5iApp`
//!   needs; it accepts static and issued (h5i-app-token) bearer tokens.
//! - h5i-app-http's `H5iApp` names a PostgreSQL `Store`; [`NetStore`] fills that slot and only
//!   renders replies and refusals (`Api`, `ReplyCodec`). Its `Store` methods are never called by
//!   [`NetExecutor`] and fail if they are.

use crate::auth::{Authorizer, TokenAuthenticator};
use axum::http::{HeaderMap, StatusCode};
use h5i_app_core::{Executed, Executor, Kernel, ReplyCodec, TenantId};
use h5i_app_http::{AuthError, Authenticator};
use h5i_app_json::Value;
use h5i_app_std::authz::Guarded;
use o5s_net_kernel::access::{self, Command, READ, Snapshot};
use std::sync::{Arc, RwLock};

/// The access kernel, guarded (`Authz<Access>`).
pub type NetKernel = o5s_net_kernel::App;

/// The kernel on h5i-app-http.
pub type NetApp = h5i_app_http::H5iApp<NetKernel, NetStore, NetExecutor>;

/// An infrastructure failure (not a kernel refusal).
#[derive(Debug)]
pub struct ExecError(pub String);

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExecError {}

/// Runs the access kernel on snapshots read from o5s's store.
pub struct NetExecutor {
    pub reach: Option<o5s_repo::reach::ReachIndex>,
    /// Shared with `AppState::cells` (policy reloads change them).
    pub cells: Arc<RwLock<Vec<String>>>,
}

impl NetExecutor {
    /// One snapshot per hash: the object with the cells that reach it, among those `actor`
    /// may read (the kernel's `allows`; the others cannot change `visible`'s answer), or no
    /// entry when none does. One batched index lookup for all hashes.
    pub async fn object_snapshots(
        &self,
        actor: &access::Principal,
        hashes: &[Vec<u8>],
    ) -> Result<Vec<Snapshot>, ExecError> {
        let empty = || hashes.iter().map(|_| Snapshot::default()).collect();
        let Some(reach) = self.reach.clone() else { return Ok(empty()) };
        let cells = self.cells.read().map(|c| c.clone()).unwrap_or_default();
        let candidates: Vec<u32> =
            (0..cells.len() as u32).filter(|&i| access::allows(actor, cells[i as usize].as_bytes(), READ)).collect();
        if candidates.is_empty() {
            return Ok(empty());
        }
        let hs: Vec<o5s_core::Hash> =
            hashes.iter().map(|h| o5s_core::Hash(<[u8; 32]>::try_from(h.as_slice()).unwrap_or([0; 32]))).collect();
        let reaching = tokio::task::spawn_blocking(move || reach.cells_reaching_many(&hs, &candidates))
            .await
            .map_err(|e| ExecError(format!("reach lookup: {e}")))?
            .map_err(|e| ExecError(format!("reach lookup: {e}")))?;
        Ok(hashes
            .iter()
            .zip(reaching)
            .map(|(h, cs)| Snapshot {
                refs: Vec::new(),
                objects: if cs.is_empty() || h.len() != 32 {
                    Vec::new()
                } else {
                    vec![(
                        h.clone(),
                        cs.iter().map(|&c| cells.get(c as usize).cloned().unwrap_or_default().into_bytes()).collect(),
                    )]
                },
            })
            .collect())
    }
}

impl Executor<NetKernel> for NetExecutor {
    type Error = ExecError;

    fn is_idempotency_conflict(_: &ExecError) -> bool {
        false
    }

    async fn execute(&self, actor: &access::Principal, cmd: &Command) -> Executed<NetKernel, ExecError> {
        let snap = self.snapshot_for(actor, cmd).await?;
        Ok(<NetKernel as Kernel>::transition(actor, &snap, cmd).map(|(_writes, reply)| reply))
    }

    /// Reads are idempotent; nothing is stored per key.
    async fn execute_idempotent<C: ReplyCodec<NetKernel>>(
        &self,
        actor: &access::Principal,
        _key: &str,
        cmd: &Command,
    ) -> Executed<NetKernel, ExecError> {
        self.execute(actor, cmd).await
    }

    async fn snapshot_for(&self, actor: &access::Principal, cmd: &Command) -> Result<Snapshot, ExecError> {
        match cmd {
            Command::ReadObject(h) => {
                Ok(self.object_snapshots(actor, std::slice::from_ref(h)).await?.pop().unwrap_or_default())
            }
            // Refs are read by the fronts through core's name layer.
            Command::ReadRef(_) | Command::WriteRef(..) => {
                Err(ExecError("this command is executed by the store's name-layer kernel".into()))
            }
        }
    }
}

/// `H5iApp`'s authenticator: static and issued bearer tokens, resolved to the principal's
/// grants. (Server routes authenticate with the middleware; see the module docs.)
pub struct NetAuth {
    pub tokens: Option<Arc<TokenAuthenticator>>,
    pub authz: Arc<Authorizer>,
}

impl Authenticator<NetKernel> for NetAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<access::Principal, AuthError> {
        let t = self.tokens.as_ref().ok_or_else(|| AuthError("no token authentication configured".into()))?;
        let p = t.check_sync(headers).map_err(|e| AuthError(e.to_string()))?;
        Ok(self.authz.grants(&p))
    }
}

/// Renders the kernel's replies and refusals for `H5iApp`. Both refusals of a read, "no such
/// object" and "not yours", are the same 404, so they cannot be told apart.
pub struct NetStore;

impl h5i_app_http::Api<NetKernel> for NetStore {
    fn decode_command(_: serde_json::Value) -> Result<Command, String> {
        Err("commands are built by the server's routes".into())
    }

    fn encode_reply(reply: &access::Reply) -> Value {
        Value::Obj(vec![(b"rows".to_vec(), Value::Num(reply.rows.len() as u64))])
    }

    fn encode_error(_: &Guarded<access::Error>) -> (StatusCode, Value) {
        (StatusCode::NOT_FOUND, h5i_app_http::error_body("not found"))
    }
}

impl ReplyCodec<NetKernel> for NetStore {
    fn fingerprint(_: &Command) -> Vec<u8> {
        Vec::new()
    }
    fn scope(_: &access::Principal) -> String {
        String::new()
    }
    fn encode(_: &access::Reply) -> Vec<u8> {
        Vec::new()
    }
    fn decode(_: &[u8]) -> Result<access::Reply, String> {
        Err("replies are not stored".into())
    }
}

fn unused() -> h5i_app_pg::DbError {
    h5i_app_pg::DbError::Pool("o5s does not use PostgreSQL: NetExecutor runs the kernel".into())
}

impl h5i_app_pg::Store<NetKernel> for NetStore {
    fn ddl() -> Vec<String> {
        Vec::new()
    }
    fn tables() -> Vec<&'static str> {
        Vec::new()
    }
    async fn load(_: &h5i_app_pg::Tx<'_>, _: TenantId) -> Result<Snapshot, h5i_app_pg::DbError> {
        Err(unused())
    }
    async fn write(_: &h5i_app_pg::Tx<'_>, _: TenantId, _: &Vec<access::Write>) -> Result<(), h5i_app_pg::DbError> {
        Err(unused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Access, PolicyEntry, TokenEntry};

    #[tokio::test]
    async fn executor_runs_the_guarded_kernel() {
        let ex = NetExecutor { reach: None, cells: Arc::new(RwLock::new(vec![String::new()])) };
        let p = access::Principal { grants: Vec::new() };
        // No reachability: the object is refused by the kernel (a refusal, not a failure).
        assert!(matches!(ex.execute(&p, &Command::ReadObject(vec![0; 32])).await, Ok(Err(_))));
        // Writes are the name layer's.
        assert!(ex.execute(&p, &Command::WriteRef(b"x".to_vec(), None)).await.is_err());
    }

    #[test]
    fn auth_resolves_tokens_to_grants() {
        let sha: String = crate::auth::sha256(b"s3cret").iter().map(|b| format!("{b:02x}")).collect();
        let tokens = Arc::new(TokenAuthenticator::new(&[TokenEntry { principal: "ci".into(), sha256: sha }], false));
        let authz = Arc::new(Authorizer::new(vec![PolicyEntry {
            principal: "ci".into(),
            namespace: "team-a".into(),
            access: Access::Write,
        }]));
        let a = NetAuth { tokens: Some(tokens), authz };
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer s3cret".parse().unwrap());
        let p = a.authenticate(&h).unwrap();
        assert_eq!(p.grants.len(), 1);
        assert_eq!(p.grants[0].prefix, b"team-a".to_vec());
        h.insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(a.authenticate(&h).is_err());
    }
}
