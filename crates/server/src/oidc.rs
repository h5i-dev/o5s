//! OIDC / JWT bearer authentication.
//!
//! [`OidcAuthenticator`] validates `Authorization: Bearer <jwt>` against an
//! issuer's JWKS. It checks the signature (RS256/384/512, PS*, ES256/384), `iss`,
//! `aud`, `exp` and `nbf` (60 s leeway). The principal is the configured claim
//! (default `sub`), with an optional prefix such as `oidc:`, and must pass the
//! same name rules as static principals.
//!
//! Keys come from `jwks_url`, or from `<issuer>/.well-known/openid-configuration`
//! when it is unset. They are cached and refreshed hourly. An unknown `kid`
//! triggers an immediate refetch, at most once per `min_refetch_secs` (30 s), so key rotation works
//! without letting junk tokens hammer the issuer.
//!
//! [`ChainAuthenticator`] tries static tokens first, then OIDC for anything that
//! looks like a JWT.

use crate::auth::{AuthError, AuthFuture, Authenticator, Principal, TokenAuthenticator};
use crate::config::OidcSection;
use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

const REFRESH_EVERY: Duration = Duration::from_secs(3600);

struct Keys {
    by_kid: HashMap<String, (DecodingKey, Algorithm)>,
    fetched: Option<Instant>,
    last_attempt: Option<Instant>,
}

pub struct OidcAuthenticator {
    cfg: OidcSection,
    http: reqwest::Client,
    keys: RwLock<Keys>,
}

fn alg_of(s: &str) -> Option<Algorithm> {
    Some(match s {
        "RS256" => Algorithm::RS256,
        "RS384" => Algorithm::RS384,
        "RS512" => Algorithm::RS512,
        "PS256" => Algorithm::PS256,
        "PS384" => Algorithm::PS384,
        "PS512" => Algorithm::PS512,
        "ES256" => Algorithm::ES256,
        "ES384" => Algorithm::ES384,
        _ => return None, // HS* (shared secrets) and "none" are never accepted
    })
}

/// Parses a JWKS document into usable keys (unsupported keys are skipped).
pub fn parse_jwks(doc: &serde_json::Value) -> HashMap<String, (DecodingKey, Algorithm)> {
    let mut out = HashMap::new();
    let Some(keys) = doc.get("keys").and_then(|k| k.as_array()) else { return out };
    for k in keys {
        let s = |f: &str| k.get(f).and_then(|v| v.as_str());
        if s("use").is_some_and(|u| u != "sig") {
            continue;
        }
        let kid = s("kid").unwrap_or("").to_string();
        let parsed = match s("kty") {
            Some("RSA") => match (s("n"), s("e")) {
                (Some(n), Some(e)) => DecodingKey::from_rsa_components(n, e)
                    .ok()
                    .map(|d| (d, s("alg").and_then(alg_of).unwrap_or(Algorithm::RS256))),
                _ => None,
            },
            Some("EC") => match (s("x"), s("y"), s("crv")) {
                (Some(x), Some(y), Some(crv)) => {
                    let alg = match crv {
                        "P-256" => Algorithm::ES256,
                        "P-384" => Algorithm::ES384,
                        _ => continue,
                    };
                    DecodingKey::from_ec_components(x, y).ok().map(|d| (d, alg))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(p) = parsed {
            out.insert(kid, p);
        }
    }
    out
}

impl OidcAuthenticator {
    pub fn new(cfg: OidcSection) -> Result<OidcAuthenticator, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(OidcAuthenticator {
            cfg,
            http,
            keys: RwLock::new(Keys { by_kid: HashMap::new(), fetched: None, last_attempt: None }),
        })
    }

    async fn jwks_url(&self) -> Result<String, String> {
        if let Some(u) = &self.cfg.jwks_url {
            return Ok(u.clone());
        }
        let disc = format!("{}/.well-known/openid-configuration", self.cfg.issuer.trim_end_matches('/'));
        let v: serde_json::Value =
            self.http.get(&disc).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
        if v.get("issuer").and_then(|i| i.as_str()) != Some(self.cfg.issuer.as_str()) {
            return Err(format!("discovery document at {disc} has a different issuer"));
        }
        v.get("jwks_uri")
            .and_then(|u| u.as_str())
            .map(str::to_string)
            .ok_or_else(|| "discovery without jwks_uri".into())
    }

    /// Fetches the JWKS (rate-limited unless `force_due`).
    async fn refresh(&self) -> Result<(), String> {
        {
            let mut g = self.keys.write().await;
            if g.last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(self.cfg.min_refetch_secs)) {
                return Ok(());
            }
            g.last_attempt = Some(Instant::now());
        }
        let url = self.jwks_url().await?;
        let doc: serde_json::Value =
            self.http.get(&url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
        let keys = parse_jwks(&doc);
        if keys.is_empty() {
            return Err(format!("JWKS at {url} has no usable signing keys"));
        }
        info!(url, keys = keys.len(), "loaded OIDC signing keys");
        let mut g = self.keys.write().await;
        g.by_kid = keys;
        g.fetched = Some(Instant::now());
        Ok(())
    }

    async fn key_for(&self, kid: &str) -> Result<Option<(DecodingKey, Algorithm)>, AuthError> {
        let stale = self.keys.read().await.fetched.is_none_or(|t| t.elapsed() > REFRESH_EVERY);
        if stale && let Err(e) = self.refresh().await {
            warn!(error = %e, "OIDC key refresh failed");
        }
        let find = |g: &Keys| {
            g.by_kid.get(kid).cloned().or_else(|| {
                (g.by_kid.len() == 1 && kid.is_empty()).then(|| g.by_kid.values().next().cloned()).flatten()
            })
        };
        if let Some(k) = find(&*self.keys.read().await) {
            return Ok(Some(k));
        }
        // Unknown kid: maybe the issuer rotated keys.
        if let Err(e) = self.refresh().await {
            let g = self.keys.read().await;
            if g.by_kid.is_empty() {
                return Err(AuthError::Unavailable(format!("OIDC keys unavailable: {e}")));
            }
        }
        Ok(find(&*self.keys.read().await))
    }

    /// Validates a JWT and maps it to a principal.
    pub async fn validate(&self, token: &str) -> Result<Principal, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::Invalid)?;
        if alg_of(&format!("{:?}", header.alg)).is_none() {
            return Err(AuthError::Invalid);
        }
        let kid = header.kid.unwrap_or_default();
        let Some((key, alg)) = self.key_for(&kid).await? else { return Err(AuthError::Invalid) };
        if alg != header.alg {
            return Err(AuthError::Invalid); // algorithm must match the key's
        }
        let mut v = Validation::new(alg);
        v.set_issuer(&[self.cfg.issuer.as_str()]);
        v.set_audience(&[self.cfg.audience.as_str()]);
        v.leeway = 60;
        v.validate_nbf = true;
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        let data = decode::<serde_json::Value>(token, &key, &v).map_err(|_| AuthError::Invalid)?;
        let claim = data.claims.get(&self.cfg.principal_claim).and_then(|c| c.as_str()).ok_or(AuthError::Invalid)?;
        let name = format!("{}{claim}", self.cfg.principal_prefix);
        if !crate::config::valid_principal(&name) {
            return Err(AuthError::Invalid);
        }
        Ok(Principal { name, authenticated: true })
    }
}

fn looks_like_jwt(t: &str) -> bool {
    t.len() > 20 && t.matches('.').count() == 2 && t.starts_with("eyJ")
}

/// Static tokens first, then OIDC for JWT-shaped bearer tokens.
pub struct ChainAuthenticator {
    pub tokens: Arc<TokenAuthenticator>,
    pub oidc: Arc<OidcAuthenticator>,
}

impl Authenticator for ChainAuthenticator {
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap) -> AuthFuture<'a> {
        Box::pin(async move {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split_once(' '))
                .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
                .map(|(_, t)| t.trim().to_string());
            match bearer {
                Some(t) if looks_like_jwt(&t) => self.oidc.validate(&t).await,
                _ => self.tokens.authenticate(headers).await,
            }
        })
    }
}
