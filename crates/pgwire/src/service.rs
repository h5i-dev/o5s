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

/// The table of `Rows` a query fills.
#[derive(Clone, Copy)]
enum Field {
    Repository,
    Patches,
    Dependencies,
    Approvals,
    Checks,
    Channels,
    Members,
    Paths,
    Events,
    Bodies,
    Listings,
    Roles,
}

fn field(rows: &mut kernel::Rows, f: Field) -> &mut Vec<Vec<Val>> {
    match f {
        Field::Repository => &mut rows.repository,
        Field::Patches => &mut rows.patches,
        Field::Dependencies => &mut rows.dependencies,
        Field::Approvals => &mut rows.approvals,
        Field::Checks => &mut rows.checks,
        Field::Channels => &mut rows.channels,
        Field::Members => &mut rows.members,
        Field::Paths => &mut rows.paths,
        Field::Events => &mut rows.events,
        Field::Bodies => &mut rows.bodies,
        Field::Listings => &mut rows.listings,
        Field::Roles => &mut rows.roles,
    }
}

/// One `SELECT`: a table, an optional `column = value` filter, the field
/// its rows go to, and whether rows already loaded are skipped
/// (`rows_where`) or the result replaces the field (a whole table).
struct Query {
    field: Field,
    table: u32,
    filter: Option<(u32, Val)>,
    dedup: bool,
}

fn whole(field: Field, table: u32) -> Query {
    Query {
        field,
        table,
        filter: None,
        dedup: false,
    }
}

fn point(field: Field, table: u32, column: u32, value: Val) -> Query {
    Query {
        field,
        table,
        filter: Some((column, value)),
        dedup: true,
    }
}

/// Send every query at once (one roundtrip; PostgreSQL runs them in order on
/// the connection) and merge the results in query order, exactly as running
/// them one after another would.
async fn run(tx: &Tx<'_>, queries: Vec<Query>, rows: &mut kernel::Rows) -> Result<(), DbError> {
    let results =
        futures::future::join_all(queries.iter().map(|q| tx.rows(q.table, q.filter.clone()))).await;
    for (q, result) in queries.iter().zip(results) {
        let out = field(rows, q.field);
        if !q.dedup {
            *out = result?;
            continue;
        }
        for row in result? {
            if !out.contains(&row) {
                out.push(row);
            }
        }
    }
    Ok(())
}

/// The members query of the channel named `label`, once channels are loaded.
fn members_of_named(rows: &kernel::Rows, label: &[u8]) -> Result<Vec<Query>, DbError> {
    let channels = kernel::Channel::from_rows(&rows.channels)
        .ok_or_else(|| DbError::Decode("stored channel rows do not decode".into()))?;
    Ok(channels
        .iter()
        .find(|c| c.label == label)
        .map(|c| {
            point(
                Field::Members,
                kernel::Member::TABLE,
                kernel::registry_tables::Member::channel,
                val(&c.id),
            )
        })
        .into_iter()
        .collect())
}

/// The rows a command can read, and no others. Every lookup the kernel makes
/// is by a key or column named in the command, so the transition on these
/// rows is the transition on the whole tenant (`Narrow.narrow_load` in
/// `crates/registry/proofs/Narrow.lean`): a repository command reads no
/// events, bodies or listings; approve and check read one patch; a proposal
/// reads the patches with its identity or dependency handles and the paths
/// it names; a merge reads the named channel, its members and the facts of
/// the batch. All queries but the members of a named channel go out in one
/// roundtrip.
pub async fn load(tx: &Tx<'_>, cmd: &kernel::Command) -> Result<kernel::Snapshot, DbError> {
    use kernel::Command as C;
    use kernel::registry_tables as col;
    let mut q = Vec::new();
    let repository = || whole(Field::Repository, kernel::Repository::TABLE);
    // Every repository command reads the whole (small) role table: an empty
    // one means the repository is open to its organization.
    if !matches!(cmd, C::Register { .. } | C::List | C::Create { .. }) {
        q.push(whole(Field::Roles, kernel::Role::TABLE));
    }
    match cmd {
        C::Register { name, repo } => {
            q.push(point(
                Field::Listings,
                kernel::Listing::TABLE,
                col::Listing::label,
                val(name),
            ));
            q.push(point(
                Field::Listings,
                kernel::Listing::TABLE,
                col::Listing::repo,
                val(repo),
            ));
        }
        C::List => q.push(whole(Field::Listings, kernel::Listing::TABLE)),
        C::Probe | C::Create { .. } => q.push(repository()),
        C::ReadBody { patch } => {
            q.push(repository());
            q.push(Query {
                dedup: false,
                ..point(
                    Field::Bodies,
                    kernel::Body::TABLE,
                    col::Body::patch,
                    val(patch),
                )
            });
        }
        C::ReadEvents => {
            q.push(repository());
            q.push(whole(Field::Events, kernel::Event::TABLE));
        }
        C::Approve { patch, .. } | C::Check { patch, .. } => {
            q.push(repository());
            q.push(Query {
                dedup: false,
                ..point(
                    Field::Patches,
                    kernel::PatchRow::TABLE,
                    col::PatchRow::id,
                    val(patch),
                )
            });
        }
        C::Propose {
            hash,
            dependencies,
            paths,
            ..
        } => {
            q.push(repository());
            if dependencies.len() > POINT_LOADS {
                q.push(whole(Field::Patches, kernel::PatchRow::TABLE));
            } else {
                q.push(point(
                    Field::Patches,
                    kernel::PatchRow::TABLE,
                    col::PatchRow::hash,
                    val(hash),
                ));
                for d in dependencies {
                    q.push(point(
                        Field::Patches,
                        kernel::PatchRow::TABLE,
                        col::PatchRow::id,
                        val(d),
                    ));
                }
            }
            if paths.len() > POINT_LOADS {
                q.push(whole(Field::Paths, kernel::PathKind::TABLE));
            } else {
                for p in paths {
                    q.push(point(
                        Field::Paths,
                        kernel::PathKind::TABLE,
                        col::PathKind::path,
                        val(&p.path),
                    ));
                }
            }
        }
        C::Fork {
            source, channel, ..
        } => {
            q.push(repository());
            q.push(point(
                Field::Channels,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(channel),
            ));
            q.push(point(
                Field::Channels,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(source),
            ));
        }
        C::Merge {
            channel, patches, ..
        } => {
            q.push(repository());
            q.push(point(
                Field::Channels,
                kernel::Channel::TABLE,
                col::Channel::label,
                val(channel),
            ));
            if patches.len() > POINT_LOADS {
                q.push(whole(Field::Patches, kernel::PatchRow::TABLE));
                q.push(whole(Field::Dependencies, kernel::Dependency::TABLE));
                q.push(whole(Field::Approvals, kernel::Approval::TABLE));
                q.push(whole(Field::Checks, kernel::CheckReport::TABLE));
            } else {
                for p in patches {
                    let id = val(p);
                    q.push(point(
                        Field::Patches,
                        kernel::PatchRow::TABLE,
                        col::PatchRow::id,
                        id.clone(),
                    ));
                    q.push(point(
                        Field::Dependencies,
                        kernel::Dependency::TABLE,
                        col::Dependency::patch,
                        id.clone(),
                    ));
                    q.push(point(
                        Field::Approvals,
                        kernel::Approval::TABLE,
                        col::Approval::patch,
                        id.clone(),
                    ));
                    q.push(point(
                        Field::Checks,
                        kernel::CheckReport::TABLE,
                        col::CheckReport::patch,
                        id,
                    ));
                }
            }
        }
        C::Grant { .. } => q.push(repository()),
        C::Inspect => {
            q.push(repository());
            q.push(whole(Field::Patches, kernel::PatchRow::TABLE));
            q.push(whole(Field::Dependencies, kernel::Dependency::TABLE));
            q.push(whole(Field::Approvals, kernel::Approval::TABLE));
            q.push(whole(Field::Checks, kernel::CheckReport::TABLE));
            q.push(whole(Field::Channels, kernel::Channel::TABLE));
            q.push(whole(Field::Members, kernel::Member::TABLE));
            q.push(whole(Field::Paths, kernel::PathKind::TABLE));
            q.push(whole(Field::Events, kernel::Event::TABLE));
        }
    }
    let mut rows = kernel::Rows::default();
    run(tx, q, &mut rows).await?;
    // Members of the named channel need its id first.
    let label = match cmd {
        C::Fork { source, .. } => Some(source),
        C::Merge { channel, .. } => Some(channel),
        _ => None,
    };
    if let Some(label) = label {
        let members = members_of_named(&rows, label)?;
        run(tx, members, &mut rows).await?;
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
        if !kernel::readable(&p, &probe, &label) {
            return Err(not_found());
        }
        let cached = self.cache.lock().unwrap().get(&scope).cloned();
        if let Some(c) = &cached
            && c.rows.repository == probe.repository
            && c.rows.roles == probe.roles
        {
            return Ok(c.view.clone());
        }
        let rows = self
            .engine
            .snapshot(scope, &kernel::Command::Inspect)
            .await
            .map_err(unavailable)?;
        if !kernel::readable(&p, &rows, &label) {
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
