//! Tokens of the form `v1.<tenant>.<user>.<expiry-unix>.<hex hmac-sha256>`,
//! sent as `Authorization: Bearer <token>`: `h5i_app_http::HmacAuth`'s format,
//! so either server accepts the other's tokens. Parsing is verified in
//! `h5i-app-token/proofs`.

use axum::http::HeaderMap;

#[derive(Debug)]
pub struct AuthError(pub String);

pub struct HmacAuth {
    secret: Vec<u8>,
}

impl HmacAuth {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        HmacAuth { secret: secret.into() }
    }

    fn tag(&self, payload: &[u8]) -> Vec<u8> {
        libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha256, &self.secret, payload, None)
    }

    /// A token for `user` of `tenant`, valid until `exp` (unix seconds).
    pub fn issue(&self, tenant: u64, user: u64, exp: u64) -> String {
        let payload = h5i_app_token::encode_payload(tenant, user, exp);
        let token = h5i_app_token::join(&payload, &self.tag(&payload));
        String::from_utf8(token).expect("tokens are ascii")
    }

    /// The tenant and user of a token valid at `now` (unix seconds).
    pub fn verify(&self, token: &str, now: u64) -> Result<(u64, u64), AuthError> {
        let err = |m: &str| AuthError(m.to_string());
        let t = h5i_app_token::parse(token.as_bytes()).ok_or_else(|| err("malformed token"))?;
        if !ct_eq(&self.tag(&t.payload), &t.sig) {
            return Err(err("bad signature"));
        }
        if t.exp < now {
            return Err(err("token expired"));
        }
        Ok((t.tenant, t.user))
    }

    /// The tenant and user of the request's bearer token.
    pub fn authenticate(&self, headers: &HeaderMap, now: u64) -> Result<(u64, u64), AuthError> {
        let h = headers.get("authorization").ok_or_else(|| AuthError("missing authorization".into()))?;
        let h = h.to_str().map_err(|_| AuthError("bad authorization header".into()))?;
        let token = h.strip_prefix("Bearer ").ok_or_else(|| AuthError("expected Bearer token".into()))?;
        self.verify(token, now)
    }
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
        assert_eq!(HmacAuth::new("other").verify(&t, 0).unwrap_err().0, "bad signature");
    }
}
