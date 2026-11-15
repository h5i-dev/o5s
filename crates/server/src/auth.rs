//! Authentication (who is calling) and authorization (what they may do).
//!
//! * [`Authenticator`] is pluggable. [`TokenAuthenticator`] checks static bearer
//!   tokens stored as SHA-256 hashes. An OIDC/JWT authenticator can implement
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
    by_hash: HashMap<[u8; 32], String>,
    allow_anonymous: bool,
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

impl TokenAuthenticator {
    pub fn new(tokens: &[TokenEntry], allow_anonymous: bool) -> Self {
        let by_hash = tokens.iter().filter_map(|t| Some((parse_hex32(&t.sha256)?, t.principal.clone()))).collect();
        TokenAuthenticator { by_hash, allow_anonymous }
    }

    fn check(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        let Some(v) = headers.get(axum::http::header::AUTHORIZATION) else {
            return if self.allow_anonymous { Ok(Principal::anonymous()) } else { Err(AuthError::Missing) };
        };
        let v = v.to_str().map_err(|_| AuthError::Malformed)?;
        let (scheme, tok) = v.split_once(' ').ok_or(AuthError::Malformed)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(AuthError::Malformed);
        }
        let tok = tok.trim();
        if tok.is_empty() {
            return Err(AuthError::Malformed);
        }
        // The lookup key is a hash of the secret, so timing reveals nothing usable.
        match self.by_hash.get(&sha256(tok.as_bytes())) {
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

/// Evaluates [`PolicyEntry`] grants.
#[derive(Clone, Debug, Default)]
pub struct Authorizer {
    entries: Vec<PolicyEntry>,
}

impl Authorizer {
    pub fn new(entries: Vec<PolicyEntry>) -> Self {
        Authorizer { entries }
    }

    fn applies(e: &PolicyEntry, p: &Principal) -> bool {
        if p.authenticated { e.principal == p.name || e.principal == "*" } else { e.principal == ANONYMOUS }
    }

    /// May `p` perform `access` on name (or namespace prefix) `path`?
    pub fn allows(&self, p: &Principal, path: &str, access: Access) -> bool {
        self.entries
            .iter()
            .any(|e| Self::applies(e, p) && e.access >= access && pico2peta_core::is_under(path, &e.namespace))
    }

    /// Does `p` hold `access` on at least one namespace?
    pub fn allows_somewhere(&self, p: &Principal, access: Access) -> bool {
        self.entries.iter().any(|e| Self::applies(e, p) && e.access >= access)
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
        assert_eq!(a.check(&hdr("Basic xyz")), Err(AuthError::Malformed));
        assert_eq!(a.check(&hdr("Bearer")), Err(AuthError::Malformed));
        assert_eq!(a.check(&HeaderMap::new()), Err(AuthError::Missing));
        let anon = TokenAuthenticator::new(&[], true);
        assert_eq!(anon.check(&HeaderMap::new()).unwrap(), Principal::anonymous());
        // A bad token is never downgraded to anonymous.
        assert_eq!(anon.check(&hdr("Bearer x")), Err(AuthError::Invalid));
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
    }
}
