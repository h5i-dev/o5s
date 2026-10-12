//! Token revocation. Tokens carry only an organization, a user and an expiry
//! (`v1.<tenant>.<user>.<expiry>.<signature>`), so a revocation names either
//! one token, by the SHA-256 of its text, or every token of a user that
//! expires no later than a given time. The signing secret's rotation
//! (`H5I_APP_PREVIOUS_SECRETS`) remains the way to revoke everything.
use crate::{Client, Connector, DbError, Engine, Runtime, ident};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tokio_postgres::types::Type;

/// The SHA-256 of a token's text, in hexadecimal.
pub fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Revocations held in memory (the local backend persists them as JSON).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocations {
    /// `(tenant, user)` to the latest expiry revoked.
    #[serde(with = "pairs")]
    pub users: BTreeMap<(u64, u64), u64>,
    /// Token digest to its expiry; entries are useless once it has passed.
    pub tokens: BTreeMap<String, u64>,
}

mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<S: Serializer>(
        m: &BTreeMap<(u64, u64), u64>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        m.iter()
            .map(|((t, u), n)| (*t, *u, *n))
            .collect::<Vec<_>>()
            .serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<(u64, u64), u64>, D::Error> {
        Ok(Vec::<(u64, u64, u64)>::deserialize(d)?
            .into_iter()
            .map(|(t, u, n)| ((t, u), n))
            .collect())
    }
}

impl Revocations {
    /// Is this token (of `tenant`/`user`, expiring at `exp`) revoked?
    pub fn revoked(&self, tenant: u64, user: u64, exp: u64, digest: &str) -> bool {
        self.users.get(&(tenant, user)).is_some_and(|n| exp <= *n)
            || self.tokens.contains_key(digest)
    }

    pub fn add(&mut self, r: &Revocation) {
        match r {
            Revocation::User {
                tenant,
                user,
                not_after,
            } => {
                let n = self.users.entry((*tenant, *user)).or_insert(0);
                *n = (*n).max(*not_after);
            }
            Revocation::Token { digest, expires } => {
                self.tokens.insert(digest.clone(), *expires);
            }
        }
    }

    /// Forget token entries that have expired anyway.
    pub fn prune(&mut self, now: u64) {
        self.tokens.retain(|_, exp| *exp >= now);
    }

    /// The earliest expiry a new token of this user may have.
    pub fn first_valid_expiry(&self, tenant: u64, user: u64) -> u64 {
        self.users.get(&(tenant, user)).map_or(0, |n| n + 1)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Revocation {
    /// Every token of `user` that expires at or before `not_after`.
    User {
        tenant: u64,
        user: u64,
        not_after: u64,
    },
    /// One token, until it expires anyway.
    Token { digest: String, expires: u64 },
}

/// Tables of the PostgreSQL store, created with the registry's.
pub const DDL: [&str; 2] = [
    "CREATE TABLE IF NOT EXISTS o5s_revoked_users (
  tenant_id BIGINT NOT NULL,
  user_id BIGINT NOT NULL,
  not_after BIGINT NOT NULL,
  PRIMARY KEY (tenant_id, user_id)
)",
    "CREATE TABLE IF NOT EXISTS o5s_revoked_tokens (
  digest TEXT PRIMARY KEY,
  expires BIGINT NOT NULL
)",
];

fn signed(n: u64) -> Result<i64, DbError> {
    i64::try_from(n).map_err(|_| DbError::Decode(format!("{n} out of range")))
}

impl<C: Connector, R: Runtime> Engine<C, R> {
    async fn on_client<T>(
        &self,
        f: impl AsyncFnOnce(&Client, &str) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let lease = self.lease().await?;
        let schema = ident(&self.schema)?;
        f(lease.client(), &schema).await
    }

    /// Is this token revoked? One indexed lookup per table.
    pub async fn revoked(
        &self,
        tenant: u64,
        user: u64,
        exp: u64,
        token: &str,
    ) -> Result<bool, DbError> {
        let (t, u, e, d) = (
            signed(tenant)?,
            signed(user)?,
            signed(exp.min(i64::MAX as u64))?,
            digest(token),
        );
        self.on_client(async |c: &Client, schema: &str| {
            let rows = c
                .query_typed(
                    &format!(
                        "SELECT EXISTS (SELECT 1 FROM {schema}.o5s_revoked_users WHERE tenant_id = $1 AND user_id = $2 AND not_after >= $3) \
                         OR EXISTS (SELECT 1 FROM {schema}.o5s_revoked_tokens WHERE digest = $4)"
                    ),
                    &[(&t, Type::INT8), (&u, Type::INT8), (&e, Type::INT8), (&d, Type::TEXT)],
                )
                .await?;
            Ok(rows.first().map(|r| r.get::<_, bool>(0)).unwrap_or(false))
        })
        .await
    }

    pub async fn revoke(&self, r: &Revocation) -> Result<(), DbError> {
        self.on_client(async |c: &Client, schema: &str| {
            match r {
                Revocation::User {
                    tenant,
                    user,
                    not_after,
                } => {
                    c.query_typed(
                        &format!(
                            "INSERT INTO {schema}.o5s_revoked_users (tenant_id, user_id, not_after) VALUES ($1, $2, $3) \
                             ON CONFLICT (tenant_id, user_id) DO UPDATE SET not_after = GREATEST({schema}.o5s_revoked_users.not_after, EXCLUDED.not_after)"
                        ),
                        &[
                            (&signed(*tenant)?, Type::INT8),
                            (&signed(*user)?, Type::INT8),
                            (&signed((*not_after).min(i64::MAX as u64))?, Type::INT8),
                        ],
                    )
                    .await?;
                }
                Revocation::Token { digest, expires } => {
                    c.query_typed(
                        &format!(
                            "INSERT INTO {schema}.o5s_revoked_tokens (digest, expires) VALUES ($1, $2) ON CONFLICT (digest) DO NOTHING"
                        ),
                        &[
                            (digest, Type::TEXT),
                            (&signed((*expires).min(i64::MAX as u64))?, Type::INT8),
                        ],
                    )
                    .await?;
                }
            }
            Ok(())
        })
        .await
    }

    /// The latest revoked expiry of a user, if any.
    pub async fn not_after(&self, tenant: u64, user: u64) -> Result<Option<u64>, DbError> {
        let (t, u) = (signed(tenant)?, signed(user)?);
        self.on_client(async |c: &Client, schema: &str| {
            let rows = c
                .query_typed(
                    &format!(
                        "SELECT not_after FROM {schema}.o5s_revoked_users WHERE tenant_id = $1 AND user_id = $2"
                    ),
                    &[(&t, Type::INT8), (&u, Type::INT8)],
                )
                .await?;
            Ok(rows.first().map(|r| r.get::<_, i64>(0) as u64))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_and_tokens_revoke_independently() {
        let mut r = Revocations::default();
        r.add(&Revocation::User {
            tenant: 1,
            user: 2,
            not_after: 100,
        });
        assert!(r.revoked(1, 2, 100, "x"));
        assert!(!r.revoked(1, 2, 101, "x"));
        assert!(!r.revoked(1, 3, 50, "x"));
        assert_eq!(r.first_valid_expiry(1, 2), 101);
        // A later revocation never shortens an earlier one.
        r.add(&Revocation::User {
            tenant: 1,
            user: 2,
            not_after: 50,
        });
        assert!(r.revoked(1, 2, 100, "x"));
        let d = digest("v1.1.3.500.ab");
        r.add(&Revocation::Token {
            digest: d.clone(),
            expires: 500,
        });
        assert!(r.revoked(1, 3, 500, &d));
        r.prune(501);
        assert!(!r.revoked(1, 3, 500, &d));
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Revocations>(&json).unwrap(), r);
    }
}
