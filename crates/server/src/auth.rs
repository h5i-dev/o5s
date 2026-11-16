//! Authentication (who is calling) and authorization (what they may do).
//!
//! * [`Authenticator`] is pluggable. [`TokenAuthenticator`] checks static
//!   tokens stored as SHA-256 hashes, sent as `Bearer <token>` or as the
//!   password of HTTP Basic auth (for `docker login` and other OCI clients). An OIDC/JWT authenticator can implement
//!   the same trait later.
//! * [`Authorizer`] evaluates per-namespace policies. Access levels are ordered
//!   `read < write < admin`, and a grant on namespace `p` covers every name
//!   under `p/` (segment-aligned; `""` covers everything).
//!
//! Object semantics: objects are content-addressed and global (deduplicated
//! across namespaces). Reading an object needs `read` on *some* namespace, and
//! storing one needs `write` on some namespace. A 256-bit hash is effectively
//! a capability, but anonymous callers still need an explicit grant.

use crate::config::{Access, PolicyEntry, TokenEntry};
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The reserved principal for unauthenticated callers.
pub const ANONYMOUS: &str = "anonymous";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    pub authenticated: bool,
}

impl Principal {
    pub fn anonymous() -> Self {
        Principal { name: ANONYMOUS.into(), authenticated: false }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("missing credentials")]
    Missing,
    #[error("invalid credentials")]
    Invalid,
    #[error("malformed Authorization header")]
    Malformed,
    #[error("authentication backend unavailable: {0}")]
    Unavailable(String),
}

pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<Principal, AuthError>> + Send + 'a>>;

/// Identifies the caller of a request.
pub trait Authenticator: Send + Sync + 'static {
    /// Returns the principal, or an error if credentials are present but bad,
    /// or missing while anonymous access is off.
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap) -> AuthFuture<'a>;
}

/// Static bearer tokens. Only SHA-256 hashes are kept in memory or config.
pub struct TokenAuthenticator {
    inner: std::sync::RwLock<Arc<TokenTable>>,
    /// The HMAC key of issued (h5i-app) tokens, if configured.
    key: Option<Vec<u8>>,
}

/// The h5i-app token user id of a principal: stable and derived from its name.
pub fn user_id(principal: &str) -> u64 {
    let h = sha256(principal.as_bytes());
    u64::from_be_bytes(h[..8].try_into().unwrap_or([0; 8])) >> 1
}

fn hmac(key: &[u8], payload: &[u8]) -> Vec<u8> {
    libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, key, payload, None)
}

/// Constant-time equality (the tag must not leak through timing).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Issues an h5i-app token for `principal`, valid for `ttl_secs`.
pub fn issue_token(key: &[u8], principal: &str, ttl_secs: u64) -> String {
    let payload = h5i_app_token::encode_payload(0, user_id(principal), now_secs().saturating_add(ttl_secs));
    let token = h5i_app_token::join(&payload, &hmac(key, &payload));
    String::from_utf8(token).unwrap_or_default()
}

struct TokenTable {
    by_hash: HashMap<[u8; 32], String>,
    allow_anonymous: bool,
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    crate::hexs::decode32(s)
}

impl TokenAuthenticator {
    pub fn new(tokens: &[TokenEntry], allow_anonymous: bool) -> Self {
        TokenAuthenticator { inner: std::sync::RwLock::new(Arc::new(Self::table(tokens, allow_anonymous))), key: None }
    }

    /// With an HMAC key: issued h5i-app tokens are accepted (see [`Self::check`]).
    pub fn with_key(mut self, key: Option<Vec<u8>>) -> Self {
        self.key = key;
        self
    }

    fn table(tokens: &[TokenEntry], allow_anonymous: bool) -> TokenTable {
        let by_hash = tokens.iter().filter_map(|t| Some((parse_hex32(&t.sha256)?, t.principal.clone()))).collect();
        TokenTable { by_hash, allow_anonymous }
    }

    /// Atomically replaces the token set (SIGHUP reload). Revoked tokens stop
    /// working for the next request.
    pub fn reload(&self, tokens: &[TokenEntry], allow_anonymous: bool) {
        let t = Arc::new(Self::table(tokens, allow_anonymous));
        if let Ok(mut g) = self.inner.write() {
            *g = t;
        }
    }

    /// Static and issued tokens, synchronously (`h5iapp::NetAuth`).
    pub fn check_sync(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        self.check(headers)
    }

    fn check(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        let table = self.inner.read().map_err(|_| AuthError::Unavailable("token table poisoned".into()))?.clone();
        let Some(v) = headers.get(axum::http::header::AUTHORIZATION) else {
            return if table.allow_anonymous { Ok(Principal::anonymous()) } else { Err(AuthError::Missing) };
        };
        let v = v.to_str().map_err(|_| AuthError::Malformed)?;
        let (scheme, rest) = v.split_once(' ').ok_or(AuthError::Malformed)?;
        let decoded;
        let tok = if scheme.eq_ignore_ascii_case("bearer") {
            rest.trim()
        } else if scheme.eq_ignore_ascii_case("basic") {
            // `docker login` / oras: any username, the token as password.
            use base64::Engine as _;
            let raw =
                base64::engine::general_purpose::STANDARD.decode(rest.trim()).map_err(|_| AuthError::Malformed)?;
            decoded = String::from_utf8(raw).map_err(|_| AuthError::Malformed)?;
            decoded.split_once(':').ok_or(AuthError::Malformed)?.1
        } else {
            return Err(AuthError::Malformed);
        };
        if tok.is_empty() {
            return Err(AuthError::Malformed);
        }
        // An issued token (`v1.`): h5i-app's proven parser, then the HMAC tag (constant time),
        // the expiry, and the token table (revocation removes the entry). Its user must be the
        // listed principal's.
        if let Some(key) = &self.key
            && tok.starts_with("v1.")
        {
            let t = h5i_app_token::parse(tok.as_bytes()).ok_or(AuthError::Invalid)?;
            if !ct_eq(&hmac(key, &t.payload), &t.sig) || t.exp < now_secs() || t.tenant != 0 {
                return Err(AuthError::Invalid);
            }
            return match table.by_hash.get(&sha256(tok.as_bytes())) {
                Some(p) if user_id(p) == t.user => Ok(Principal { name: p.clone(), authenticated: true }),
                _ => Err(AuthError::Invalid),
            };
        }
        // A static token: the lookup key is a hash of the secret, so timing reveals nothing usable.
        match table.by_hash.get(&sha256(tok.as_bytes())) {
            Some(p) => Ok(Principal { name: p.clone(), authenticated: true }),
            None => Err(AuthError::Invalid),
        }
    }
}

impl Authenticator for TokenAuthenticator {
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap) -> AuthFuture<'a> {
        let r = self.check(headers);
        Box::pin(async move { r })
    }
}

/// The access kernel's level for `a` (1 read, 2 write, 3 admin).
pub fn level(a: Access) -> u8 {
    match a {
        Access::Read => pico2peta_net_kernel::access::READ,
        Access::Write => pico2peta_net_kernel::access::WRITE,
        Access::Admin => pico2peta_net_kernel::access::ADMIN,
    }
}

/// Evaluates [`PolicyEntry`] grants. The policy set can be swapped at runtime.
#[derive(Debug, Default)]
pub struct Authorizer {
    entries: std::sync::RwLock<Arc<Vec<PolicyEntry>>>,
}

impl Authorizer {
    pub fn new(entries: Vec<PolicyEntry>) -> Self {
        Authorizer { entries: std::sync::RwLock::new(Arc::new(entries)) }
    }

    /// Atomically replaces the policy set (SIGHUP reload).
    pub fn reload(&self, entries: Vec<PolicyEntry>) {
        if let Ok(mut g) = self.entries.write() {
            *g = Arc::new(entries);
        }
    }

    fn snapshot(&self) -> Arc<Vec<PolicyEntry>> {
        // A poisoned lock means a panic mid-swap; deny everything rather than guess.
        self.entries.read().map(|g| g.clone()).unwrap_or_default()
    }

    fn applies(e: &PolicyEntry, p: &Principal) -> bool {
        if p.authenticated { e.principal == p.name || e.principal == "*" } else { e.principal == ANONYMOUS }
    }

    /// The grants that apply to `p`, as the access kernel takes them (`net-kernel/src/access.rs`).
    /// Which policy entries apply to a principal (its name, `*`, or anonymous) is this shell's
    /// part; what they allow is the kernel's.
    pub fn grants(&self, p: &Principal) -> pico2peta_net_kernel::access::Principal {
        let grants = self
            .snapshot()
            .iter()
            .filter(|e| Self::applies(e, p))
            .map(|e| pico2peta_net_kernel::access::Grant {
                prefix: e.namespace.as_bytes().to_vec(),
                access: level(e.access),
            })
            .collect();
        pico2peta_net_kernel::access::Principal { grants }
    }

    /// May `p` perform `access` on name (or namespace prefix) `path`? Decided by the extracted
    /// access kernel (`access::allows`).
    pub fn allows(&self, p: &Principal, path: &str, access: Access) -> bool {
        pico2peta_net_kernel::access::allows(&self.grants(p), path.as_bytes(), level(access))
    }

    /// Does `p` hold `access` on at least one namespace?
    pub fn allows_somewhere(&self, p: &Principal, access: Access) -> bool {
        self.snapshot().iter().any(|e| Self::applies(e, p) && e.access >= access)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hdr(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_str(v).unwrap());
        h
    }

    fn tok(principal: &str, secret: &str) -> TokenEntry {
        let h = sha256(secret.as_bytes());
        TokenEntry { principal: principal.into(), sha256: h.iter().map(|b| format!("{b:02x}")).collect() }
    }

    #[test]
    fn tokens() {
        let a = TokenAuthenticator::new(&[tok("ci", "s3cret")], false);
        assert_eq!(a.check(&hdr("Bearer s3cret")).unwrap().name, "ci");
        assert_eq!(a.check(&hdr("bearer  s3cret ")).unwrap().name, "ci");
        assert_eq!(a.check(&hdr("Bearer nope")), Err(AuthError::Invalid));
        assert_eq!(a.check(&hdr("Basic xyz!")), Err(AuthError::Malformed));
        // base64("anyone:s3cret")
        assert_eq!(a.check(&hdr("Basic YW55b25lOnMzY3JldA==")).unwrap().name, "ci");
        // base64("anyone:wrong")
        assert_eq!(a.check(&hdr("Basic YW55b25lOndyb25n")), Err(AuthError::Invalid));
        assert_eq!(a.check(&hdr("Digest abc")), Err(AuthError::Malformed));
        assert_eq!(a.check(&hdr("Bearer")), Err(AuthError::Malformed));
        assert_eq!(a.check(&HeaderMap::new()), Err(AuthError::Missing));
        let anon = TokenAuthenticator::new(&[], true);
        assert_eq!(anon.check(&HeaderMap::new()).unwrap(), Principal::anonymous());
        // A bad token is never downgraded to anonymous.
        assert_eq!(anon.check(&hdr("Bearer x")), Err(AuthError::Invalid));
        // Reload revokes old tokens immediately.
        a.reload(&[tok("ci2", "new")], false);
        assert_eq!(a.check(&hdr("Bearer s3cret")), Err(AuthError::Invalid));
        assert_eq!(a.check(&hdr("Bearer new")).unwrap().name, "ci2");
    }

    #[test]
    fn policies() {
        let pe = |p: &str, ns: &str, a| PolicyEntry { principal: p.into(), namespace: ns.into(), access: a };
        let z = Authorizer::new(vec![
            pe("ci", "pkgs", Access::Write),
            pe("*", "public", Access::Read),
            pe("root", "", Access::Admin),
            pe(ANONYMOUS, "public/open", Access::Read),
        ]);
        let ci = Principal { name: "ci".into(), authenticated: true };
        let bob = Principal { name: "bob".into(), authenticated: true };
        let root = Principal { name: "root".into(), authenticated: true };
        let anon = Principal::anonymous();
        assert!(z.allows(&ci, "pkgs/foo", Access::Write));
        assert!(z.allows(&ci, "pkgs/foo", Access::Read));
        assert!(!z.allows(&ci, "pkgs/foo", Access::Admin));
        assert!(!z.allows(&ci, "pkgsx/foo", Access::Read), "prefix match must be segment-aligned");
        assert!(z.allows(&bob, "public/x", Access::Read));
        assert!(!z.allows(&bob, "public/x", Access::Write));
        assert!(z.allows(&root, "anything/at/all", Access::Admin));
        assert!(!z.allows(&anon, "public/x", Access::Read), "'*' excludes anonymous");
        assert!(z.allows(&anon, "public/open/y", Access::Read));
        assert!(z.allows_somewhere(&ci, Access::Write));
        assert!(!z.allows_somewhere(&bob, Access::Write));
        // A principal literally named like a wildcard cannot exist (config validation),
        // and an unauthenticated principal never matches a named grant.
        let fake = Principal { name: "root".into(), authenticated: false };
        assert!(!z.allows(&fake, "x", Access::Read));
        z.reload(vec![]);
        assert!(!z.allows(&root, "anything", Access::Read), "reload revokes grants");
    }
}
