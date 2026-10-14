//! Tokens of the form `v1.<tenant>.<user>.<expiry-unix>.<hex hmac-sha256>`,
//! sent as `Authorization: Bearer <token>`: `h5i_app_http::HmacAuth`'s format,
//! so either server accepts the other's tokens. Parsing is verified in
//! `h5i-app-token/proofs`.
//!
//! A read-only token has the same form, signed with a key derived from the
//! secret instead of the secret: it authenticates its user for `GET`
//! requests only (`permits`), and neither kind can be turned into the other.

use axum::http::{HeaderMap, Method};

/// Derives the key read-only tokens are signed with.
const READ_ONLY: &[u8] = b"o5s read-only token key";

#[derive(Debug)]
pub struct AuthError(pub String);

pub struct HmacAuth {
    secret: Vec<u8>,
    /// Earlier secrets still accepted during rotation; never used to issue.
    previous: Vec<Vec<u8>>,
}

impl HmacAuth {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        HmacAuth {
            secret: secret.into(),
            previous: Vec::new(),
        }
    }

    /// Also accept tokens signed with these earlier secrets.
    pub fn with_previous(mut self, previous: Vec<Vec<u8>>) -> Self {
        self.previous = previous.into_iter().filter(|s| !s.is_empty()).collect();
        self
    }

    fn tag(&self, payload: &[u8]) -> Vec<u8> {
        tag_with(&self.secret, payload)
    }

    /// A token for `user` of `tenant`, valid until `exp` (unix seconds).
    pub fn issue(&self, tenant: u64, user: u64, exp: u64) -> String {
        let payload = h5i_app_token::encode_payload(tenant, user, exp);
        let token = h5i_app_token::join(&payload, &self.tag(&payload));
        String::from_utf8(token).expect("tokens are ascii")
    }

    /// A token for `user` of `tenant`, valid until `exp`, for `GET` only.
    pub fn issue_read_only(&self, tenant: u64, user: u64, exp: u64) -> String {
        let payload = h5i_app_token::encode_payload(tenant, user, exp);
        let token = h5i_app_token::join(&payload, &tag_with(&read_key(&self.secret), &payload));
        String::from_utf8(token).expect("tokens are ascii")
    }

    /// Whether `token` is signed as read-only (valid or not otherwise).
    pub fn read_only(&self, token: &str) -> bool {
        h5i_app_token::parse(token.as_bytes()).is_some_and(|t| {
            std::iter::once(&self.secret)
                .chain(&self.previous)
                .any(|s| ct_eq(&tag_with(&read_key(s), &t.payload), &t.sig))
        })
    }

    /// Whether a request may proceed: a read-only token may only `GET`.
    pub fn permits(&self, method: &Method, headers: &HeaderMap) -> bool {
        if method == Method::GET || method == Method::HEAD {
            return true;
        }
        let token = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "));
        !token.is_some_and(|t| self.read_only(t))
    }

    /// The tenant and user of a token valid at `now` (unix seconds).
    pub fn verify(&self, token: &str, now: u64) -> Result<(u64, u64), AuthError> {
        self.claims(token, now).map(|(t, u, _)| (t, u))
    }

    /// The tenant, user and expiry of a token valid at `now`.
    pub fn claims(&self, token: &str, now: u64) -> Result<(u64, u64, u64), AuthError> {
        let err = |m: &str| AuthError(m.to_string());
        let t = h5i_app_token::parse(token.as_bytes()).ok_or_else(|| err("malformed token"))?;
        let signed = std::iter::once(&self.secret)
            .chain(&self.previous)
            .any(|s| {
                ct_eq(&tag_with(s, &t.payload), &t.sig)
                    || ct_eq(&tag_with(&read_key(s), &t.payload), &t.sig)
            });
        if !signed {
            return Err(err("bad signature"));
        }
        if t.exp < now {
            return Err(err("token expired"));
        }
        Ok((t.tenant, t.user, t.exp))
    }

    /// Length and a short HMAC fingerprint of the secret, to compare with
    /// `admin fingerprint` when tokens fail to verify. Reveals neither.
    pub fn fingerprint(&self) -> String {
        let tag = self.tag(b"o5s-secret-check");
        let hex: String = tag.iter().take(6).map(|b| format!("{b:02x}")).collect();
        format!("{} bytes, {hex}", self.secret.len())
    }

    /// The tenant and user of the request's bearer token.
    pub fn authenticate(&self, headers: &HeaderMap, now: u64) -> Result<(u64, u64), AuthError> {
        let h = headers
            .get("authorization")
            .ok_or_else(|| AuthError("missing authorization".into()))?;
        let h = h
            .to_str()
            .map_err(|_| AuthError("bad authorization header".into()))?;
        let token = h
            .strip_prefix("Bearer ")
            .ok_or_else(|| AuthError("expected Bearer token".into()))?;
        self.verify(token, now)
    }
}

fn tag_with(secret: &[u8], payload: &[u8]) -> Vec<u8> {
    libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, secret, payload, None)
}

fn read_key(secret: &[u8]) -> Vec<u8> {
    tag_with(secret, READ_ONLY)
}

/// Constant-time equality, so the comparison does not leak the tag.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_what_it_issues() {
        let auth = HmacAuth::new("s");
        let t = auth.issue(1, 2, 100);
        assert_eq!(auth.verify(&t, 100).unwrap(), (1, 2));
        assert_eq!(auth.verify(&t, 101).unwrap_err().0, "token expired");
        assert_eq!(
            HmacAuth::new("other").verify(&t, 0).unwrap_err().0,
            "bad signature"
        );
    }

    #[test]
    fn read_only_tokens_authenticate_reads_only() {
        let auth = HmacAuth::new("s");
        let r = auth.issue_read_only(1, 2, 100);
        assert_eq!(auth.verify(&r, 100).unwrap(), (1, 2));
        assert!(auth.read_only(&r));
        assert!(!auth.read_only(&auth.issue(1, 2, 100)));
        assert!(!HmacAuth::new("other").read_only(&r));
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {r}").parse().unwrap());
        assert!(auth.permits(&Method::GET, &headers));
        assert!(!auth.permits(&Method::POST, &headers));
        let full = auth.issue(1, 2, 100);
        headers.insert("authorization", format!("Bearer {full}").parse().unwrap());
        assert!(auth.permits(&Method::POST, &headers));
        // Rotation keeps read-only tokens read-only.
        let rotated = HmacAuth::new("new").with_previous(vec![b"s".to_vec()]);
        assert!(rotated.read_only(&r) && rotated.verify(&r, 0).is_ok());
    }

    #[test]
    fn rotation_accepts_previous_secrets_without_issuing_with_them() {
        let old = HmacAuth::new("old").issue(1, 2, 100);
        let auth = HmacAuth::new("new").with_previous(vec![b"old".to_vec()]);
        assert_eq!(auth.verify(&old, 0).unwrap(), (1, 2));
        assert_eq!(
            HmacAuth::new("new").verify(&old, 0).unwrap_err().0,
            "bad signature"
        );
        assert!(auth.verify(&auth.issue(1, 2, 100), 0).is_ok());
    }
}
