//! Git projections still to deliver. An admission on the Worker exports the
//! channel after replying, which can fail or never run. So the admission's
//! transaction also records the channel and the generation it reached here;
//! a successful export removes the record, and a scheduled sweep exports
//! whatever is left. An export never replaces a newer generation, so a
//! repeated or late one is harmless.
use crate::{Client, Connector, DbError, Engine, Runtime};
use tokio_postgres::types::Type;

/// A channel an admission changed, and who admitted to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbox {
    pub org: u64,
    pub repo: String,
    pub channel: String,
    pub user: u64,
}

/// A recorded projection still to deliver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub outbox: Outbox,
    pub generation: u64,
}

pub const DDL: &str = "CREATE TABLE IF NOT EXISTS o5s_export_pending (
  org BIGINT NOT NULL,
  repo TEXT NOT NULL,
  channel TEXT NOT NULL,
  generation BIGINT NOT NULL,
  user_id BIGINT NOT NULL,
  PRIMARY KEY (org, repo, channel)
)";

/// The statement an admission adds to its transaction (search path set).
pub(crate) const RECORD: &str = "INSERT INTO o5s_export_pending (org, repo, channel, generation, user_id) \
     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (org, repo, channel) DO UPDATE SET \
     generation = GREATEST(o5s_export_pending.generation, EXCLUDED.generation), user_id = EXCLUDED.user_id";

fn signed(n: u64) -> Result<i64, DbError> {
    i64::try_from(n).map_err(|_| DbError::Decode(format!("{n} out of range")))
}

/// The parameters of `RECORD`.
pub(crate) fn record_params(o: &Outbox, generation: u64) -> Result<(i64, i64, i64), DbError> {
    Ok((signed(o.org)?, signed(generation)?, signed(o.user)?))
}

impl<C: Connector, R: Runtime> Engine<C, R> {
    /// Up to `limit` projections still to deliver, oldest generation first.
    pub async fn pending_exports(&self, limit: u32) -> Result<Vec<Pending>, DbError> {
        let limit = i64::from(limit);
        let lease = self.lease().await?;
        let schema = crate::ident(&self.schema)?;
        let c: &Client = lease.client();
        let rows = c
            .query_typed(
                &format!(
                    "SELECT org, repo, channel, generation, user_id FROM {schema}.o5s_export_pending \
                     ORDER BY generation LIMIT $1"
                ),
                &[(&limit, Type::INT8)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| Pending {
                outbox: Outbox {
                    org: r.get::<_, i64>(0) as u64,
                    repo: r.get(1),
                    channel: r.get(2),
                    user: r.get::<_, i64>(4) as u64,
                },
                generation: r.get::<_, i64>(3) as u64,
            })
            .collect())
    }

    /// The channel's projection reached `generation`: forget the record
    /// unless a later admission has raised it since.
    pub async fn exported(&self, o: &Outbox, generation: u64) -> Result<(), DbError> {
        let (org, generation) = (signed(o.org)?, signed(generation)?);
        let lease = self.lease().await?;
        let schema = crate::ident(&self.schema)?;
        let c: &Client = lease.client();
        c.query_typed(
            &format!(
                "DELETE FROM {schema}.o5s_export_pending \
                 WHERE org = $1 AND repo = $2 AND channel = $3 AND generation <= $4"
            ),
            &[
                (&org, Type::INT8),
                (&o.repo, Type::TEXT),
                (&o.channel, Type::TEXT),
                (&generation, Type::INT8),
            ],
        )
        .await?;
        Ok(())
    }
}
