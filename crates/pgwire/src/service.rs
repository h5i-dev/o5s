//! The registry service shared by the native server and the Worker: plan an
//! API command, run its kernel steps, and read repository views through a
//! cache that probes the commit generation.
use crate::{Connector, DbError, Engine, Idem, Outcome, Runtime, Tx};
use h5i_app_sql::Val;
use o5s_platform::{Actor, Command, Refusal, Repository, describe, plan, repository_key};
use o5s_registry as kernel;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

/// Why a request did not complete.
#[derive(Debug)]
pub enum Failure {
    /// The kernel or validation refused; nothing was committed.
    Refused(Refusal),
    /// An idempotency key was reused for a different command.
    Conflict(String),
    /// Storage failed. Nothing was committed unless the message says the
    /// outcome is unknown; retry with the same idempotency key.
    Unavailable(String),
}

impl From<Refusal> for Failure {
    fn from(r: Refusal) -> Self {
        Failure::Refused(r)
    }
}

pub fn unavailable(e: DbError) -> Failure {
    match e {
        DbError::IdempotencyConflict => {
            Failure::Conflict("idempotency key reused for a different command".into())
        }
        DbError::CommitUnknown(e) => Failure::Unavailable(format!(
            "commit outcome unknown ({e}); retry with the same Idempotency-Key"
        )),
        DbError::RetriesExhausted => {
            Failure::Unavailable("database contention; retry later".into())
        }
        other => Failure::Unavailable(format!("storage unavailable: {other}")),
    }
}

fn not_found() -> Failure {
    Failure::Refused(Refusal("repository not found".into()))
}

/// More dependencies or batch entries than this load whole tables instead of
/// one filtered query per handle.
pub const POINT_LOADS: usize = 16;

fn val<T: h5i_app_sql::Column>(x: &T) -> Val {
    h5i_app_sql::Column::to_val(x)
}

async fn rows_where(
    tx: &Tx<'_>,
    table: u32,
    column: u32,
    value: Val,
    out: &mut Vec<Vec<Val>>,
) -> Result<(), DbError> {
    for row in tx.rows(table, Some((column, value))).await? {
        if !out.contains(&row) {
            out.push(row);
        }
    }
    Ok(())
}

async fn members_of_named(
    tx: &Tx<'_>,
    rows: &mut kernel::Rows,
    label: &[u8],
) -> Result<(), DbError> {
    let channels = kernel::Channel::from_rows(&rows.channels)
        .ok_or_else(|| DbError::Decode("stored channel rows do not decode".into()))?;
    if let Some(c) = channels.iter().find(|c| c.label == label) {
        rows_where(
            tx,
            kernel::Member::TABLE,
            kernel::registry_tables::Member::channel,
            val(&c.id),
            &mut rows.members,
        )
        .await?;
    }
    Ok(())
}

/// The rows a command can read, and no others. Every lookup the kernel makes
/// is by a key or column named in the command, so the transition on these
/// rows is the transition on the whole tenant (`narrow_load` in
/// `crates/registry/pending/Theorems.lean`): a repository command reads no
/// events, bodies or listings; approve and check read one patch; a proposal
/// reads the patches with its identity or dependency handles and the paths
/// it names; a merge reads the named channel, its members and the facts of
/// the batch.
pub async fn load(tx: &Tx<'_>, cmd: &kernel::Command) -> Result<kernel::Snapshot, DbError> {
    use kernel::Command as C;
    use kernel::registry_tables as col;
    let mut rows = kernel::Rows::default();
    let all = |t| tx.rows(t, None);
    match cmd {
        C::Register { name, repo } => {
            rows_where(
                tx,
                kernel::Listing::TABLE,
                col::Listing::label,
                val(name),
                &mut rows.listings,
            )
            .await?;
            rows_where(
                tx,
                kernel::Listing::TABLE,
                col::Listing::repo,
                val(repo),
                &mut rows.listings,
            )
            .await?;
        }
        C::List => rows.listings = all(kernel::Listing::TABLE).await?,
        C::Probe | C::Create { .. } => rows.repository = all(kernel::Repository::TABLE).await?,
        C::ReadBody { patch } => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows.bodies = tx
                .rows(kernel::Body::TABLE, Some((col::Body::patch, val(patch))))
                .await?;
        }
        C::ReadEvents => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows.events = all(kernel::Event::TABLE).await?;
        }
        C::Approve { patch, .. } | C::Check { patch, .. } => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows.patches = tx
                .rows(
                    kernel::PatchRow::TABLE,
                    Some((col::PatchRow::id, val(patch))),
                )
                .await?;
        }
        C::Propose {
            hash,
            dependencies,
            paths,
            ..
        } => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            if dependencies.len() > POINT_LOADS {
                rows.patches = all(kernel::PatchRow::TABLE).await?;
            } else {
                rows_where(
                    tx,
                    kernel::PatchRow::TABLE,
                    col::PatchRow::hash,
                    val(hash),
                    &mut rows.patches,
                )
                .await?;
                for d in dependencies {
                    rows_where(
                        tx,
                        kernel::PatchRow::TABLE,
                        col::PatchRow::id,
                        val(d),
                        &mut rows.patches,
                    )
                    .await?;
                }
            }
            if paths.len() > POINT_LOADS {
                rows.paths = all(kernel::PathKind::TABLE).await?;
            } else {
                for p in paths {
                    rows_where(
                        tx,
                        kernel::PathKind::TABLE,
                        col::PathKind::path,
                        val(&p.path),
                        &mut rows.paths,
                    )
                    .await?;
                }
            }
        }
        C::Fork {
            source, channel, ..
        } => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows_where(
                tx,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(channel),
                &mut rows.channels,
            )
            .await?;
            rows_where(
                tx,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(source),
                &mut rows.channels,
            )
            .await?;
            members_of_named(tx, &mut rows, source).await?;
        }
        C::Merge {
            channel, patches, ..
        } => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows_where(
                tx,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(channel),
                &mut rows.channels,
            )
            .await?;
            members_of_named(tx, &mut rows, channel).await?;
            if patches.len() > POINT_LOADS {
                rows.patches = all(kernel::PatchRow::TABLE).await?;
                rows.dependencies = all(kernel::Dependency::TABLE).await?;
                rows.approvals = all(kernel::Approval::TABLE).await?;
                rows.checks = all(kernel::CheckReport::TABLE).await?;
            } else {
                for p in patches {
                    let id = val(p);
                    rows_where(
                        tx,
                        kernel::PatchRow::TABLE,
                        col::PatchRow::id,
                        id.clone(),
                        &mut rows.patches,
                    )
                    .await?;
                    rows_where(
                        tx,
                        kernel::Dependency::TABLE,
                        col::Dependency::patch,
                        id.clone(),
                        &mut rows.dependencies,
                    )
                    .await?;
                    rows_where(
                        tx,
                        kernel::Approval::TABLE,
                        col::Approval::patch,
                        id.clone(),
                        &mut rows.approvals,
                    )
                    .await?;
                    rows_where(
                        tx,
                        kernel::CheckReport::TABLE,
                        col::CheckReport::patch,
                        id,
                        &mut rows.checks,
                    )
                    .await?;
                }
            }
        }
        C::Inspect => {
            rows.repository = all(kernel::Repository::TABLE).await?;
            rows.patches = all(kernel::PatchRow::TABLE).await?;
            rows.dependencies = all(kernel::Dependency::TABLE).await?;
            rows.approvals = all(kernel::Approval::TABLE).await?;
            rows.checks = all(kernel::CheckReport::TABLE).await?;
            rows.channels = all(kernel::Channel::TABLE).await?;
            rows.members = all(kernel::Member::TABLE).await?;
            rows.paths = all(kernel::PathKind::TABLE).await?;
            rows.events = all(kernel::Event::TABLE).await?;
        }
    }
    kernel::decode(&rows).ok_or_else(|| DbError::Decode("stored rows do not decode".into()))
}

struct Cached {
    /// Committed rows without bodies, from the newest load.
    rows: kernel::Snapshot,
    view: Arc<Repository>,
}

/// More new patches than this load the whole tenant instead of one body each.
const BODY_BATCH: usize = 32;

pub struct Service<C: Connector, R: Runtime> {
    pub engine: Engine<C, R>,
    cache: Mutex<HashMap<u64, Arc<Cached>>>,
}

fn principal(actor: &Actor, scope: u64) -> kernel::Principal {
    kernel::Principal {
        org: actor.tenant,
        user: actor.user,
        scope,
    }
}

impl<C: Connector, R: Runtime> Service<C, R> {
    pub fn new(engine: Engine<C, R>) -> Self {
        Self {
            engine,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Run an API command's kernel steps, each in its own transaction. A
    /// repository is created after its directory listing, so no existing
    /// repository is ever missing from the directory.
    pub async fn execute(
        &self,
        actor: &Actor,
        command: Command,
        key: Option<&str>,
    ) -> Result<serde_json::Value, Failure> {
        let view = if command.needs_view() {
            Some(self.preparation_view(actor, &command).await?)
        } else {
            None
        };
        let plan = plan(actor, view.as_deref(), command).map_err(Failure::Refused)?;
        let mut replies = Vec::new();
        for step in &plan.steps {
            let idem = key.map(|k| Idem {
                key: k,
                fingerprint: Sha256::digest(serde_json::to_vec(&step.command).unwrap_or_default())
                    .to_vec(),
            });
            let scoped;
            let idem = match idem {
                Some(i) => {
                    scoped = format!("u{}/{}", step.principal.user, i.key);
                    Some(Idem {
                        key: &scoped,
                        fingerprint: i.fingerprint,
                    })
                }
                None => None,
            };
            let outcome = self
                .engine
                .execute(&step.principal, &step.command, idem, |r| {
                    serde_json::to_vec(r).unwrap_or_default()
                })
                .await
                .map_err(unavailable)?;
            match outcome {
                Outcome::Done(Ok(reply)) => replies.push(reply),
                Outcome::Done(Err(e)) => return Err(Failure::Refused(Refusal(describe(e).into()))),
                Outcome::Replayed(bytes) => replies.push(
                    serde_json::from_slice(&bytes)
                        .map_err(|e| Failure::Unavailable(format!("stored reply: {e}")))?,
                ),
            }
        }
        Ok(plan.reply(&replies))
    }

    /// A view for preparing a proposal. Preparation reads only the immutable
    /// records of the proposal's dependencies, line bases and their causal
    /// past, which a cached view holds once it holds the named handles, so the
    /// cache is refreshed only when it lacks one. The kernel transaction still
    /// re-checks every handle.
    async fn preparation_view(
        &self,
        actor: &Actor,
        command: &Command,
    ) -> Result<Arc<Repository>, Failure> {
        let scope = repository_key(actor.tenant, command.repo());
        let mut needed: Vec<u64> = Vec::new();
        match command {
            Command::Propose { dependencies, .. } | Command::ProposeGraph { dependencies, .. } => {
                needed.extend(dependencies)
            }
            Command::ProposeLines {
                dependencies,
                edits,
                ..
            } => {
                needed.extend(dependencies);
                needed.extend(edits.iter().map(|e| e.base));
            }
            _ => {}
        }
        let cached = self.cache.lock().unwrap().get(&scope).cloned();
        if let Some(c) = cached
            && needed.iter().all(|id| c.view.patches.contains_key(id))
        {
            return Ok(c.view.clone());
        }
        self.repository(actor, command.repo()).await
    }

    pub async fn repositories(&self, actor: &Actor) -> Result<Vec<String>, Failure> {
        if actor.tenant >= kernel::REPOSITORY_SCOPE {
            return Err(Failure::Refused(Refusal("invalid tenant".into())));
        }
        let rows = self
            .engine
            .snapshot(actor.tenant, &kernel::Command::List)
            .await
            .map_err(unavailable)?;
        let mut names: Vec<String> = rows
            .listings
            .into_iter()
            .filter_map(|l| String::from_utf8(l.label).ok())
            .collect();
        names.sort();
        Ok(names)
    }

    /// The repository view at a snapshot no older than this call.
    pub async fn repository(&self, actor: &Actor, name: &str) -> Result<Arc<Repository>, Failure> {
        if actor.tenant >= kernel::REPOSITORY_SCOPE || !o5s_platform::valid_name(name) {
            return Err(not_found());
        }
        let scope = repository_key(actor.tenant, name);
        let p = principal(actor, scope);
        let label = name.as_bytes().to_vec();
        let probe = self
            .engine
            .snapshot(scope, &kernel::Command::Probe)
            .await
            .map_err(unavailable)?;
        if !kernel::visible(&p, &probe.repository, &label) {
            return Err(not_found());
        }
        let cached = self.cache.lock().unwrap().get(&scope).cloned();
        if let Some(c) = &cached
            && c.rows.repository == probe.repository
        {
            return Ok(c.view.clone());
        }
        let rows = self
            .engine
            .snapshot(scope, &kernel::Command::Inspect)
            .await
            .map_err(unavailable)?;
        if !kernel::visible(&p, &rows.repository, &label) {
            return Err(not_found());
        }
        let (old, mut view) = match &cached {
            Some(c) => (c.rows.clone(), (*c.view).clone()),
            None => (kernel::Snapshot::default(), Repository::default()),
        };
        let writes = o5s_platform::delta(&old, &rows);
        let fresh: Vec<u64> = writes
            .iter()
            .filter_map(|w| match w {
                kernel::Write::PutPatch(p) => Some(p.id),
                _ => None,
            })
            .collect();
        let mut bodies: BTreeMap<u64, kernel::Body> = BTreeMap::new();
        if fresh.len() > BODY_BATCH {
            let rows = self
                .engine
                .table(scope, kernel::Body::TABLE)
                .await
                .map_err(unavailable)?;
            let all = kernel::Body::from_rows(&rows)
                .ok_or_else(|| Failure::Unavailable("stored bodies do not decode".into()))?;
            bodies.extend(all.into_iter().map(|b| (b.patch, b)));
        } else {
            for id in &fresh {
                let one = self
                    .engine
                    .snapshot(scope, &kernel::Command::ReadBody { patch: *id })
                    .await
                    .map_err(unavailable)?;
                bodies.extend(one.bodies.into_iter().map(|b| (b.patch, b)));
            }
        }
        let mut ordered = Vec::with_capacity(writes.len() + fresh.len());
        for write in writes {
            let body = match &write {
                kernel::Write::PutPatch(p) => Some(bodies.remove(&p.id).ok_or_else(|| {
                    Failure::Unavailable(format!("patch {} has no stored body", p.id))
                })?),
                _ => None,
            };
            ordered.push(write);
            if let Some(body) = body {
                ordered.push(kernel::Write::PutBody(body));
            }
        }
        view.project(&ordered)
            .map_err(|e| Failure::Unavailable(format!("stored registry is inconsistent: {e}")))?;
        let view = Arc::new(view);
        self.cache.lock().unwrap().insert(
            scope,
            Arc::new(Cached {
                rows,
                view: view.clone(),
            }),
        );
        Ok(view)
    }

    /// A connection and a read-only transaction succeed.
    pub async fn ready(&self) -> Result<(), Failure> {
        self.engine
            .snapshot(0, &kernel::Command::List)
            .await
            .map(|_| ())
            .map_err(unavailable)
    }
}
