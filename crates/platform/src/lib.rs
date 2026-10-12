//! Patch collaboration on top of the verified registry kernel.
//!
//! Every registry decision (repository directory, immutable patch identity,
//! independent review and check facts, channel forks and dependency-closed
//! admission) is made by the extracted `o5s_registry::transition`. This crate
//! prepares kernel commands natively (SHA-256 identities and line/graph edits
//! validated against immutable dependencies), projects committed kernel rows
//! into repository views, and materializes files and conflicts. Hashing,
//! preparation and materialization belong to the tested trust boundary.
use o5s_registry as kernel;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub use o5s_registry::REPOSITORY_SCOPE;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    /// None is a tombstone, not an empty file.
    pub content: Option<String>,
    /// Git mode 100755 instead of 100644.
    #[serde(default, skip_serializing_if = "is_false")]
    pub executable: bool,
    /// `content` is base64 of arbitrary bytes rather than UTF-8 text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub base64: bool,
    /// A symbolic link (`content` is its target) or a submodule (`content` is
    /// the commit id); omitted for ordinary files, so their bodies are unchanged.
    #[serde(default, skip_serializing_if = "Kind::is_file")]
    pub kind: Kind,
}
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}
/// What a path holds besides an ordinary file: Git modes 120000 and 160000.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    File,
    Symlink,
    Submodule,
}
impl Kind {
    pub fn is_file(&self) -> bool {
        *self == Kind::File
    }
}
/// A Git tree entry's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Regular,
    Executable,
    Symlink,
    /// The bytes are the 20-byte commit id.
    Gitlink,
}
/// Check a symlink target or submodule commit id.
pub fn valid_special(kind: Kind, content: &str, executable: bool, base64: bool) -> bool {
    match kind {
        Kind::File => true,
        Kind::Symlink => {
            !executable
                && !base64
                && !content.is_empty()
                && content.len() <= 4096
                && !content.contains('\0')
        }
        Kind::Submodule => {
            !executable
                && !base64
                && content.len() == 40
                && content
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
    }
}
/// A file's bytes and mode, as one register value: concurrent edits that
/// differ in any of them conflict.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Content {
    pub text: String,
    pub executable: bool,
    pub base64: bool,
    #[serde(default, skip_serializing_if = "Kind::is_file")]
    pub kind: Kind,
}
impl Edit {
    /// The register value this edit writes; `None` removes the file.
    pub fn value(&self) -> Option<Content> {
        self.content.as_ref().map(|text| Content {
            text: text.clone(),
            executable: self.executable,
            base64: self.base64,
            kind: self.kind,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Patch {
    pub id: u64,
    pub hash: String,
    pub author: u64,
    pub intent: String,
    pub dependencies: Vec<u64>,
    pub edits: Vec<Edit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_edits: Vec<lines::LineEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graph_edits: Vec<documents::GraphEdit>,
    pub approvals: BTreeSet<u64>,
    pub checks_passed: bool,
}
/// A repository as projected from committed kernel rows.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub patches: BTreeMap<u64, Patch>,
    /// Channel membership in admission order.
    pub channels: BTreeMap<String, Vec<u64>>,
    pub events: Vec<Event>,
    /// The committed kernel event counter: changes with every commit.
    #[serde(default)]
    pub generation: u64,
    /// Access levels of a restricted repository; empty when it is open to
    /// its organization.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<u64, u64>,
    #[serde(skip)]
    channel_names: BTreeMap<u64, String>,
    #[serde(skip)]
    by_hash: BTreeMap<String, u64>,
    #[serde(skip)]
    docs: documents::Memo,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub actor: u64,
    pub action: String,
    pub patch: Option<u64>,
    pub channel: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub tenant: u64,
    pub user: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Create {
        repo: String,
    },
    Fork {
        repo: String,
        source: String,
        channel: String,
    },
    Propose {
        repo: String,
        intent: String,
        dependencies: Vec<u64>,
        edits: Vec<Edit>,
    },
    ProposeLines {
        repo: String,
        intent: String,
        dependencies: Vec<u64>,
        edits: Vec<lines::LineEdit>,
    },
    ProposeGraph {
        repo: String,
        intent: String,
        dependencies: Vec<u64>,
        edits: Vec<documents::GraphEdit>,
        /// Whole-file edits of legacy or binary files in the same proposal.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<Edit>,
    },
    Approve {
        repo: String,
        patch: u64,
    },
    Check {
        repo: String,
        patch: u64,
        passed: bool,
    },
    MergeBatch {
        repo: String,
        channel: String,
        patches: Vec<u64>,
    },
    Merge {
        repo: String,
        channel: String,
        patch: u64,
    },
    /// Set a user's access to the repository: `none`, `read`, `write`,
    /// `review` or `admin`. The first grant restricts an open repository.
    Grant {
        repo: String,
        user: u64,
        role: String,
    },
}
/// A role's level, as the kernel's `READ`..`ADMIN`.
pub fn role_level(role: &str) -> Option<u64> {
    Some(match role {
        "none" => 0,
        "read" => kernel::READ,
        "write" => kernel::WRITE,
        "review" => kernel::REVIEW,
        "admin" => kernel::ADMIN,
        _ => return None,
    })
}
/// The name of a level.
pub fn role_name(level: u64) -> &'static str {
    match level {
        0 => "none",
        kernel::READ => "read",
        kernel::WRITE => "write",
        kernel::REVIEW => "review",
        _ => "admin",
    }
}
impl Command {
    pub fn repo(&self) -> &str {
        match self {
            Self::Create { repo }
            | Self::Fork { repo, .. }
            | Self::Propose { repo, .. }
            | Self::ProposeLines { repo, .. }
            | Self::ProposeGraph { repo, .. }
            | Self::Approve { repo, .. }
            | Self::Check { repo, .. }
            | Self::Merge { repo, .. }
            | Self::MergeBatch { repo, .. }
            | Self::Grant { repo, .. } => repo,
        }
    }
    /// Proposals are prepared against the repository's immutable patches.
    pub fn needs_view(&self) -> bool {
        matches!(
            self,
            Self::Propose { .. } | Self::ProposeLines { .. } | Self::ProposeGraph { .. }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Refusal(pub String);
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Refusal {}
fn refuse(msg: &str) -> Refusal {
    Refusal(msg.into())
}
pub fn valid_name(name: &str) -> bool {
    kernel::valid_name(&name.as_bytes().to_vec())
}
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git")
        })
        && !path
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
}

/// The message each kernel refusal is reported with.
pub fn describe(error: kernel::Error) -> &'static str {
    use kernel::Error as E;
    match error {
        E::WrongScope => "tenant scope mismatch",
        E::InvalidName => "invalid repository name",
        E::RepositoryExists => "repository already exists",
        E::RepositoryNotFound => "repository not found",
        E::KeyCollision => "repository key collision",
        E::InvalidChannelName => "invalid channel name",
        E::ChannelExists => "channel already exists",
        E::SourceChannelNotFound => "source channel not found",
        E::ChannelNotFound => "channel not found",
        E::InvalidHash => "invalid patch hash",
        E::UnknownDependency => "unknown dependency",
        E::GraphDocument => "graph document requires graph operations",
        E::LegacyDocument => "legacy document requires explicit graph migration",
        E::PatchNotFound => "patch not found",
        E::IndependentReviewerRequired => "independent reviewer required",
        E::IndependentCheckReporterRequired => "independent check reporter required",
        E::InvalidBatch => "1..10000 patches required",
        E::Unapproved => "patch unapproved",
        E::ChecksFailed => "checks failed",
        E::MissingDependency => "dependency not integrated",
        E::Exhausted => "identifier space exhausted",
        E::ReadOnly => "read-only load",
        E::Forbidden => "your role does not allow this",
        E::InvalidLevel => "invalid role",
        E::OwnLevel => "users cannot change their own role",
    }
}

/// The tenant scope holding repository `name` of organization `tenant`:
/// 62 bits of SHA-256 above `REPOSITORY_SCOPE`. The kernel refuses access when
/// the stored organization and name differ, so a collision cannot leak data.
pub fn repository_key(tenant: u64, name: &str) -> u64 {
    let digest = Sha256::new()
        .chain_update(b"o5s-repository-v1\0")
        .chain_update(tenant.to_be_bytes())
        .chain_update(name.as_bytes())
        .finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    REPOSITORY_SCOPE | (u64::from_be_bytes(bytes) & (REPOSITORY_SCOPE - 1))
}

fn principal(actor: &Actor, scope: u64) -> kernel::Principal {
    kernel::Principal {
        org: actor.tenant,
        user: actor.user,
        scope,
    }
}

/// One kernel transition: who runs it, in which tenant scope, and what.
#[derive(Clone, Debug)]
pub struct Step {
    pub principal: kernel::Principal,
    pub command: kernel::Command,
}

/// The kernel steps of one API command and how to describe their replies.
#[derive(Clone, Debug)]
pub struct Plan {
    pub steps: Vec<Step>,
    shape: Shape,
}
#[derive(Clone, Debug)]
enum Shape {
    Repo(String),
    Channel(String),
    Proposal(String),
    Approval(u64),
    Check(u64, bool),
    Batch(Vec<u64>, String),
    Merge(u64, String),
    Grant(u64, String),
}
impl Plan {
    /// The API reply for the kernel replies of `steps`, in order.
    pub fn reply(&self, replies: &[kernel::Reply]) -> serde_json::Value {
        match (&self.shape, replies.last()) {
            (Shape::Repo(name), _) => serde_json::json!({"repo":name}),
            (Shape::Channel(name), _) => serde_json::json!({"channel":name}),
            (
                Shape::Proposal(hash),
                Some(kernel::Reply::Proposed {
                    patch,
                    duplicate: true,
                }),
            ) => {
                serde_json::json!({"patch":patch,"hash":hash,"duplicate":true})
            }
            (Shape::Proposal(hash), Some(kernel::Reply::Proposed { patch, .. })) => {
                serde_json::json!({"patch":patch,"hash":hash})
            }
            (Shape::Approval(patch), _) => serde_json::json!({"patch":patch,"approved":true}),
            (Shape::Check(patch, passed), _) => {
                serde_json::json!({"patch":patch,"checks_passed":passed})
            }
            (Shape::Batch(patches, channel), _) => {
                serde_json::json!({"patches":patches,"channel":channel})
            }
            (Shape::Merge(patch, channel), _) => {
                serde_json::json!({"patch":patch,"channel":channel})
            }
            (Shape::Grant(user, role), _) => serde_json::json!({"user":user,"role":role}),
            (Shape::Proposal(_), _) => serde_json::json!({}),
        }
    }
    /// The repository scope this plan changes.
    pub fn scope(&self) -> u64 {
        self.steps.last().map_or(0, |s| s.principal.scope)
    }
}

/// Native preparation: turn an API command into kernel steps. Proposals need
/// the repository's immutable patches (`view`), which never change once
/// visible; the kernel re-checks dependency existence in its transaction.
pub fn plan(actor: &Actor, view: Option<&Repository>, command: Command) -> Result<Plan, Refusal> {
    if actor.tenant >= REPOSITORY_SCOPE {
        return Err(refuse("invalid tenant"));
    }
    if !valid_name(command.repo()) {
        return Err(refuse("invalid repository name"));
    }
    let scope = repository_key(actor.tenant, command.repo());
    let at = |command| Step {
        principal: principal(actor, scope),
        command,
    };
    let bytes = |s: &str| s.as_bytes().to_vec();
    let (steps, shape) = match command {
        Command::Create { repo } => (
            vec![
                Step {
                    principal: principal(actor, actor.tenant),
                    command: kernel::Command::Register {
                        name: bytes(&repo),
                        repo: scope,
                    },
                },
                at(kernel::Command::Create { name: bytes(&repo) }),
            ],
            Shape::Repo(repo),
        ),
        Command::Fork {
            repo,
            source,
            channel,
        } => (
            vec![at(kernel::Command::Fork {
                repo: bytes(&repo),
                source: bytes(&source),
                channel: bytes(&channel),
            })],
            Shape::Channel(channel),
        ),
        Command::Grant { repo, user, role } => {
            let level = role_level(&role).ok_or_else(|| refuse("invalid role"))?;
            (
                vec![at(kernel::Command::Grant {
                    repo: bytes(&repo),
                    user,
                    level,
                })],
                Shape::Grant(user, role),
            )
        }
        Command::Approve { repo, patch } => (
            vec![at(kernel::Command::Approve {
                repo: bytes(&repo),
                patch,
            })],
            Shape::Approval(patch),
        ),
        Command::Check {
            repo,
            patch,
            passed,
        } => (
            vec![at(kernel::Command::Check {
                repo: bytes(&repo),
                patch,
                passed,
            })],
            Shape::Check(patch, passed),
        ),
        Command::MergeBatch {
            repo,
            channel,
            mut patches,
        } => {
            if patches.is_empty() || patches.len() > kernel::MAX_BATCH {
                return Err(refuse("1..10000 patches required"));
            }
            patches.sort_unstable();
            patches.dedup();
            (
                vec![at(kernel::Command::Merge {
                    repo: bytes(&repo),
                    channel: bytes(&channel),
                    patches: patches.clone(),
                })],
                Shape::Batch(patches, channel),
            )
        }
        Command::Merge {
            repo,
            channel,
            patch,
        } => (
            vec![at(kernel::Command::Merge {
                repo: bytes(&repo),
                channel: bytes(&channel),
                patches: vec![patch],
            })],
            Shape::Merge(patch, channel),
        ),
        proposal => {
            let view = view.ok_or_else(|| refuse("repository not found"))?;
            let prepared = view.prepare(actor.user, proposal)?;
            let hash = prepared.hash.clone();
            (
                vec![at(kernel::Command::Propose {
                    repo: bytes(&prepared.repo),
                    hash: bytes(&prepared.hash),
                    dependencies: prepared.dependencies,
                    paths: prepared.paths,
                    body: prepared.body,
                })],
                Shape::Proposal(hash),
            )
        }
    };
    Ok(Plan { steps, shape })
}
/// A proposal ready for the kernel.
pub struct Prepared {
    pub repo: String,
    pub hash: String,
    pub dependencies: Vec<u64>,
    pub paths: Vec<kernel::PathUse>,
    pub body: Vec<u8>,
}

impl Repository {
    /// Validate a proposal against immutable dependency records and encode its
    /// canonical body. Registry facts (existence, identity, path models) are
    /// decided by the kernel.
    pub fn prepare(&self, author: u64, command: Command) -> Result<Prepared, Refusal> {
        limits::check_patches(self.patches.len())?;
        let (repo, intent, mut dependencies, mut edits, line_edits, graph_edits) = match command {
            Command::Propose {
                repo,
                intent,
                dependencies,
                edits,
            } => (repo, intent, dependencies, edits, Vec::new(), Vec::new()),
            Command::ProposeLines {
                repo,
                intent,
                mut dependencies,
                mut edits,
            } => {
                let previews = self.prepare_lines(&mut edits, &mut dependencies)?;
                (repo, intent, dependencies, previews, edits, Vec::new())
            }
            Command::ProposeGraph {
                repo,
                intent,
                dependencies,
                edits,
                files,
            } => (repo, intent, dependencies, files, Vec::new(), edits),
            _ => return Err(refuse("not a proposal")),
        };
        if intent.trim().is_empty() || intent.len() > 8192 {
            return Err(refuse("intent required (max 8192 bytes)"));
        }
        if (edits.is_empty() && graph_edits.is_empty()) || edits.len() + graph_edits.len() > 1024 {
            return Err(refuse("1..1024 edits required"));
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        if dependencies.len() > 10_000 {
            return Err(refuse("too many dependencies"));
        }
        if dependencies.iter().any(|d| !self.patches.contains_key(d)) {
            return Err(refuse("unknown dependency"));
        }
        edits.sort_by(|a, b| a.path.cmp(&b.path));
        if edits.iter().any(|e| !valid_path(&e.path))
            || edits.windows(2).any(|e| e[0].path == e[1].path)
            || edits
                .iter()
                .any(|e| graph_edits.iter().any(|g| g.path == e.path))
        {
            return Err(refuse("invalid or duplicate edit path"));
        }
        for e in &mut edits {
            match &e.content {
                // A removal has no mode or encoding.
                None => {
                    e.executable = false;
                    e.base64 = false;
                    e.kind = Kind::File;
                }
                Some(text) if e.base64 && base64_decode(text).is_none() => {
                    return Err(refuse("binary file content must be standard padded base64"));
                }
                Some(text) if !valid_special(e.kind, text, e.executable, e.base64) => {
                    return Err(refuse(
                        "a symlink needs a target (no NUL, at most 4096 bytes) and a submodule a 40-digit lowercase commit id; neither is executable or base64",
                    ));
                }
                Some(_) => {}
            }
        }
        let body = body::encode(
            author,
            &intent,
            &dependencies,
            &edits,
            &line_edits,
            &graph_edits,
            self,
        )?;
        let hash = body::hash(&body);
        self.validate_graph_edits(&dependencies, &graph_edits, &hash)?;
        let mut paths: Vec<kernel::PathUse> = edits
            .iter()
            .map(|e| kernel::PathUse {
                path: e.path.as_bytes().to_vec(),
                graph: false,
            })
            .collect();
        paths.extend(graph_edits.iter().map(|e| kernel::PathUse {
            path: e.path.as_bytes().to_vec(),
            graph: true,
        }));
        Ok(Prepared {
            repo,
            hash,
            dependencies,
            paths,
            body,
        })
    }

    /// The handle of the patch with this content identity.
    pub fn handle(&self, hash: &str) -> Option<u64> {
        self.by_hash.get(hash).copied()
    }

    /// Apply committed kernel writes to this view, in commit order.
    pub fn project(&mut self, writes: &[kernel::Write]) -> Result<(), Refusal> {
        use kernel::Write as W;
        let corrupt = || refuse("corrupt registry rows");
        for write in writes {
            match write {
                W::SetRepository(r) => self.generation = r.events,
                W::PutPatch(p) => {
                    let hash = String::from_utf8(p.hash.clone()).map_err(|_| corrupt())?;
                    self.by_hash.insert(hash.clone(), p.id);
                    self.patches.insert(
                        p.id,
                        Patch {
                            id: p.id,
                            hash,
                            author: p.author,
                            intent: String::new(),
                            dependencies: Vec::new(),
                            edits: Vec::new(),
                            line_edits: Vec::new(),
                            graph_edits: Vec::new(),
                            approvals: BTreeSet::new(),
                            checks_passed: false,
                        },
                    );
                }
                W::PutBody(b) => {
                    let decoded = body::decode(&b.bytes, self)?;
                    let patch = self.patches.get_mut(&b.patch).ok_or_else(corrupt)?;
                    if body::hash(&b.bytes) != patch.hash {
                        return Err(refuse("patch body does not match its identity"));
                    }
                    patch.intent = decoded.intent;
                    patch.edits = decoded.edits;
                    patch.line_edits = decoded.line_edits;
                    patch.graph_edits = decoded.graph_edits;
                }
                W::PutDependency(d) => {
                    let patch = self.patches.get_mut(&d.patch).ok_or_else(corrupt)?;
                    if let Err(at) = patch.dependencies.binary_search(&d.dependency) {
                        patch.dependencies.insert(at, d.dependency);
                    }
                }
                W::PutApproval(a) => {
                    let patch = self.patches.get_mut(&a.patch).ok_or_else(corrupt)?;
                    patch.approvals.insert(a.reviewer);
                }
                W::PutCheck(c) => {
                    let patch = self.patches.get_mut(&c.patch).ok_or_else(corrupt)?;
                    patch.checks_passed = c.passed;
                }
                W::PutChannel(c) => {
                    let name = String::from_utf8(c.label.clone()).map_err(|_| corrupt())?;
                    self.channels.entry(name.clone()).or_default();
                    self.channel_names.insert(c.id, name);
                }
                W::PutMember(m) => {
                    let name = self.channel_names.get(&m.channel).ok_or_else(corrupt)?;
                    let members = self.channels.get_mut(name).ok_or_else(corrupt)?;
                    let position = usize::try_from(m.position).map_err(|_| corrupt())?;
                    match position.cmp(&members.len()) {
                        std::cmp::Ordering::Less => members[position] = m.patch,
                        std::cmp::Ordering::Equal => members.push(m.patch),
                        std::cmp::Ordering::Greater => return Err(corrupt()),
                    }
                }
                W::PutEvent(e) => {
                    let channel = match e.channel {
                        Some(id) => Some(self.channel_names.get(&id).ok_or_else(corrupt)?.clone()),
                        None => None,
                    };
                    self.events.push(Event {
                        actor: e.actor,
                        action: action_name(e.action).into(),
                        patch: e.patch,
                        channel,
                    });
                }
                W::PutRole(r) => {
                    self.roles.insert(r.user, r.level);
                }
                W::PutPath(_) | W::PutListing(_) => {}
            }
        }
        Ok(())
    }

    /// The view of a complete tenant snapshot.
    pub fn from_rows(rows: &kernel::Snapshot) -> Result<Self, Refusal> {
        let mut repo = Self::default();
        repo.project(&delta(&kernel::Snapshot::default(), rows))?;
        Ok(repo)
    }
}

fn action_name(action: u64) -> &'static str {
    match action {
        kernel::CREATE => "create",
        kernel::FORK => "fork",
        kernel::PROPOSE => "propose",
        kernel::APPROVE => "approve",
        kernel::CHECK => "check",
        kernel::MERGE => "merge",
        kernel::GRANT => "grant",
        _ => "unknown",
    }
}

/// The writes that turn `old` into `new`, in an order `Repository::project`
/// accepts: each patch before its body, channels before members, members by
/// position and events by sequence. Rows only ever change by keyed puts.
pub fn delta(old: &kernel::Snapshot, new: &kernel::Snapshot) -> Vec<kernel::Write> {
    use kernel::Write as W;
    fn fresh<T: Clone + PartialEq, K: Ord>(old: &[T], new: &[T], key: impl Fn(&T) -> K) -> Vec<T> {
        let before: BTreeMap<K, &T> = old.iter().map(|r| (key(r), r)).collect();
        let mut out: Vec<T> = new
            .iter()
            .filter(|r| before.get(&key(r)) != Some(r))
            .cloned()
            .collect();
        out.sort_by_key(|r| key(r));
        out
    }
    let mut out = Vec::new();
    let bodies: BTreeMap<u64, &kernel::Body> = new.bodies.iter().map(|b| (b.patch, b)).collect();
    let known: BTreeSet<u64> = old.bodies.iter().map(|b| b.patch).collect();
    for p in fresh(&old.patches, &new.patches, |p| p.id) {
        let id = p.id;
        out.push(W::PutPatch(p));
        if let Some(b) = bodies.get(&id) {
            out.push(W::PutBody((*b).clone()));
        }
    }
    for b in &new.bodies {
        if !known.contains(&b.patch) && old.patches.iter().any(|p| p.id == b.patch) {
            out.push(W::PutBody(b.clone()));
        }
    }
    out.extend(
        fresh(&old.dependencies, &new.dependencies, |d| {
            (d.patch, d.dependency)
        })
        .into_iter()
        .map(W::PutDependency),
    );
    out.extend(
        fresh(&old.approvals, &new.approvals, |a| (a.patch, a.reviewer))
            .into_iter()
            .map(W::PutApproval),
    );
    out.extend(
        fresh(&old.checks, &new.checks, |c| c.patch)
            .into_iter()
            .map(W::PutCheck),
    );
    out.extend(
        fresh(&old.channels, &new.channels, |c| c.id)
            .into_iter()
            .map(W::PutChannel),
    );
    out.extend(
        fresh(&old.members, &new.members, |m| (m.channel, m.position))
            .into_iter()
            .map(W::PutMember),
    );
    out.extend(
        fresh(&old.paths, &new.paths, |k| k.id)
            .into_iter()
            .map(W::PutPath),
    );
    out.extend(
        fresh(&old.events, &new.events, |e| e.seq)
            .into_iter()
            .map(W::PutEvent),
    );
    out.extend(
        fresh(&old.listings, &new.listings, |l| l.repo)
            .into_iter()
            .map(W::PutListing),
    );
    out.extend(
        fresh(&old.roles, &new.roles, |r| r.user)
            .into_iter()
            .map(W::PutRole),
    );
    if old.repository != new.repository {
        out.push(W::SetRepository(new.repository.clone()));
    }
    out
}

/// The committed writes of one tenant scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Effect {
    pub scope: u64,
    pub writes: Vec<kernel::Write>,
}

/// What an API command commits, and its reply.
#[derive(Clone, Debug)]
pub struct Decision {
    pub effects: Vec<Effect>,
    pub reply: serde_json::Value,
}
impl Decision {
    /// No writes: a duplicate proposal returns its existing identity.
    pub fn is_noop(&self) -> bool {
        self.effects.iter().all(|e| e.writes.is_empty())
    }
}

/// The local engine: kernel rows per tenant scope plus repository views.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct Database {
    /// Kernel rows without events and bodies; transitions never read them.
    pub registry: BTreeMap<u64, kernel::Snapshot>,
    /// Repository views by repository scope, shared with readers.
    pub views: BTreeMap<u64, std::sync::Arc<Repository>>,
}
#[derive(Deserialize)]
struct Stored {
    registry: BTreeMap<u64, kernel::Snapshot>,
    views: BTreeMap<u64, std::sync::Arc<Repository>>,
}
impl From<Stored> for Database {
    fn from(stored: Stored) -> Self {
        let mut views = stored.views;
        for (scope, view) in &mut views {
            if let Some(rows) = stored.registry.get(scope) {
                std::sync::Arc::make_mut(view).reindex(rows);
            }
        }
        Self {
            registry: stored.registry,
            views,
        }
    }
}
impl Repository {
    /// Rebuild the identity and channel indexes after deserialization.
    pub fn reindex(&mut self, rows: &kernel::Snapshot) {
        self.by_hash = self
            .patches
            .values()
            .map(|p| (p.hash.clone(), p.id))
            .collect();
        self.channel_names = rows
            .channels
            .iter()
            .filter_map(|c| Some((c.id, String::from_utf8(c.label.clone()).ok()?)))
            .collect();
    }
}
impl Database {
    /// The repository `name` of the actor's organization, if the kernel says
    /// the actor may see it.
    pub fn repo(&self, actor: &Actor, name: &str) -> Result<&Repository, Refusal> {
        let scope = repository_key(actor.tenant, name);
        let visible = self.registry.get(&scope).is_some_and(|s| {
            kernel::readable(&principal(actor, scope), s, &name.as_bytes().to_vec())
        });
        if !visible {
            return Err(refuse("repository not found"));
        }
        self.views
            .get(&scope)
            .map(|v| v.as_ref())
            .ok_or_else(|| refuse("repository not found"))
    }
    /// A shared handle on the view, for readers that outlive a lock.
    pub fn repo_shared(
        &self,
        actor: &Actor,
        name: &str,
    ) -> Result<std::sync::Arc<Repository>, Refusal> {
        self.repo(actor, name)?;
        Ok(self.views[&repository_key(actor.tenant, name)].clone())
    }
    /// Names in the actor's organization directory whose repository exists.
    pub fn repositories(&self, actor: &Actor) -> Vec<String> {
        let mut names: Vec<String> = self
            .registry
            .get(&actor.tenant)
            .map(|s| {
                s.listings
                    .iter()
                    .filter_map(|l| String::from_utf8(l.label.clone()).ok())
                    // Names of restricted repositories stay listed to the
                    // organization; their contents do not.
                    .filter(|name| {
                        let scope = repository_key(actor.tenant, name);
                        self.registry.get(&scope).is_some_and(|s| {
                            kernel::visible(
                                &principal(actor, scope),
                                &s.repository,
                                &name.as_bytes().to_vec(),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
    /// Decide an API command without changing anything.
    pub fn decide(&self, actor: &Actor, command: Command) -> Result<Decision, Refusal> {
        let view = if command.needs_view() {
            Some(self.repo(actor, command.repo())?)
        } else {
            None
        };
        let plan = plan(actor, view, command)?;
        decide_plan(&plan, |scope| self.registry.get(&scope))
    }
    /// Apply decided effects. Callers persist them first.
    pub fn commit(&mut self, effects: &[Effect]) -> Result<(), Refusal> {
        for effect in effects {
            let snapshot = self.registry.entry(effect.scope).or_default();
            let kept: Vec<kernel::Write> = effect
                .writes
                .iter()
                .filter(|w| !matches!(w, kernel::Write::PutBody(_) | kernel::Write::PutEvent(_)))
                .cloned()
                .collect();
            kernel::commit(snapshot, &kept);
            if effect.scope >= REPOSITORY_SCOPE {
                let view = self.views.entry(effect.scope).or_default();
                std::sync::Arc::make_mut(view).project(&effect.writes)?;
            }
        }
        Ok(())
    }
    /// Decide and commit, for tests and in-memory use.
    pub fn execute(
        &mut self,
        actor: &Actor,
        command: Command,
    ) -> Result<serde_json::Value, Refusal> {
        let decision = self.decide(actor, command)?;
        self.commit(&decision.effects)?;
        Ok(decision.reply)
    }
}

/// Run each step's kernel transition on its scope's rows. Steps of one plan
/// use distinct scopes, so no step reads another's writes.
pub fn decide_plan<'a>(
    plan: &Plan,
    rows: impl Fn(u64) -> Option<&'a kernel::Snapshot>,
) -> Result<Decision, Refusal> {
    let empty = kernel::Snapshot::default();
    let mut effects = Vec::new();
    let mut replies = Vec::new();
    for step in &plan.steps {
        let snapshot = rows(step.principal.scope).unwrap_or(&empty);
        let (writes, reply) = kernel::transition(&step.principal, snapshot, &step.command)
            .map_err(|e| refuse(describe(e)))?;
        effects.push(Effect {
            scope: step.principal.scope,
            writes,
        });
        replies.push(reply);
    }
    Ok(Decision {
        effects,
        reply: plan.reply(&replies),
    })
}

pub mod body;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alternative {
    pub patch: u64,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub executable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub base64: bool,
    #[serde(default, skip_serializing_if = "Kind::is_file")]
    pub kind: Kind,
}
impl Alternative {
    fn of(patch: u64, value: &Option<Content>) -> Self {
        Self {
            patch,
            content: value.as_ref().map(|c| c.text.clone()),
            executable: value.as_ref().is_some_and(|c| c.executable),
            base64: value.as_ref().is_some_and(|c| c.base64),
            kind: value.as_ref().map_or(Kind::File, |c| c.kind),
        }
    }
}
/// Files of a channel. Text files hold UTF-8; paths in `binary` hold base64.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct View {
    pub files: BTreeMap<String, String>,
    pub conflicts: BTreeMap<String, Vec<Alternative>>,
    pub patches: Vec<u64>,
    /// Files with Git mode 100755.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub executable: BTreeSet<String>,
    /// Files whose `files` entry is base64.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub binary: BTreeSet<String>,
    /// Symbolic links; `files` holds the target.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub symlinks: BTreeSet<String>,
    /// Submodules; `files` holds the commit id.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub submodules: BTreeSet<String>,
}
impl View {
    /// The bytes of a path and whether it is executable.
    pub fn bytes(&self, path: &str) -> Option<(Vec<u8>, bool)> {
        self.entry(path).map(|(b, m)| (b, m == Mode::Executable))
    }
    /// The bytes and Git mode of a path. A submodule's bytes are its commit id.
    pub fn entry(&self, path: &str) -> Option<(Vec<u8>, Mode)> {
        let text = self.files.get(path)?;
        if self.submodules.contains(path) {
            let id: Option<Vec<u8>> = (0..20)
                .map(|i| u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok())
                .collect();
            return Some((id?, Mode::Gitlink));
        }
        let bytes = if self.binary.contains(path) {
            base64_decode(text)?
        } else {
            text.as_bytes().to_vec()
        };
        let mode = if self.symlinks.contains(path) {
            Mode::Symlink
        } else if self.executable.contains(path) {
            Mode::Executable
        } else {
            Mode::Regular
        };
        Some((bytes, mode))
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Inverse of `base64_encode`; `None` on anything else.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let value = |c: u8| BASE64.iter().position(|b| *b == c).map(|v| v as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (k, chunk) in bytes.chunks(4).enumerate() {
        let last = k + 1 == bytes.len() / 4;
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = if i >= 4 - pad { 0 } else { value(*c)? };
            n |= v << (18 - 6 * i);
        }
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    (base64_encode(&out) == text).then_some(out)
}
impl Repository {
    /// Compute causal ancestry once, in registry order (dependencies always
    /// precede their child), without recursion or synthetic pseudo-edges.
    pub fn materialize_reference(&self, channel: &str) -> Result<View, Refusal> {
        let active: BTreeSet<u64> = self
            .channels
            .get(channel)
            .ok_or_else(|| refuse("channel not found"))?
            .iter()
            .copied()
            .collect();
        let mut ancestors: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        let mut heads: BTreeMap<String, BTreeMap<u64, Option<Content>>> = BTreeMap::new();
        let mut versions = BTreeMap::new();
        for id in &active {
            let p = self
                .patches
                .get(id)
                .ok_or_else(|| refuse("corrupt patch registry"))?;
            let mut ancestry = BTreeSet::new();
            for d in &p.dependencies {
                let prior = ancestors
                    .get(d)
                    .ok_or_else(|| refuse("channel is not dependency closed"))?;
                ancestry.extend(prior.iter().copied());
                ancestry.insert(*d);
            }
            for e in &p.edits {
                let h = heads.entry(e.path.clone()).or_default();
                h.retain(|id, _| !ancestry.contains(id));
                h.insert(*id, e.value());
            }
            for e in &p.edits {
                let changes: Vec<_> = p
                    .line_edits
                    .iter()
                    .filter(|line| line.path == e.path)
                    .cloned()
                    .collect();
                if !changes.is_empty() {
                    versions.insert((e.path.clone(), *id), self.line_version(&e.path, &changes)?);
                }
            }
            ancestors.insert(*id, ancestry);
        }
        render_heads(&heads, &versions, &active)
    }

    /// Fresh replay uses the same incremental engine as the native cache.
    pub fn materialize(&self, channel: &str) -> Result<View, Refusal> {
        let mut cache = IncrementalView::default();
        cache.sync(self, channel)?;
        cache.view()
    }

    /// The snapshot a patch defines: the patch and its causal past, which
    /// never changes. Executed checks run against it.
    pub fn materialize_patch(&self, id: u64) -> Result<View, Refusal> {
        if !self.patches.contains_key(&id) {
            return Err(refuse("patch not found"));
        }
        let rows = documents::rows(self);
        let found =
            o5s_kernel::indexed::reachable(&rows, &vec![id as usize], vec![false; rows.len()]);
        let mut pending: Vec<u64> = found.queue.into_iter().map(|h| h as u64).collect();
        pending.sort_unstable();
        let mut cache = IncrementalView::default();
        cache.integrate(self, pending)?;
        cache.view()
    }
}

fn render_heads(
    heads: &BTreeMap<String, BTreeMap<u64, Option<Content>>>,
    versions: &BTreeMap<(String, u64), lines::LineVersion>,
    active: &BTreeSet<u64>,
) -> Result<View, Refusal> {
    // Structural conflicts count too: Git cannot represent both `a`
    // and `a/b`. Preserve real patch provenance for every alternative.
    let live: BTreeSet<String> = heads
        .iter()
        .filter(|(_, h)| h.values().any(Option::is_some))
        .map(|(path, _)| path.clone())
        .collect();
    let mut collisions = BTreeSet::new();
    for path in &live {
        for (i, c) in path.char_indices() {
            if c == '/' && live.contains(&path[..i]) {
                collisions.insert(path.clone());
                collisions.insert(path[..i].to_string());
            }
        }
    }
    let outputs = heads
        .iter()
        .map(|(path, h)| {
            (
                path.clone(),
                render_path(path, h, versions, collisions.contains(path)),
            )
        })
        .collect();
    let mut view = collect_outputs(&outputs);
    view.patches = active.iter().copied().collect();
    Ok(view)
}

#[derive(Clone)]
enum PathOutput {
    File(Content),
    Conflict(Vec<Alternative>),
    Deleted,
}
fn render_path(
    path: &str,
    heads: &BTreeMap<u64, Option<Content>>,
    versions: &BTreeMap<(String, u64), lines::LineVersion>,
    collision: bool,
) -> PathOutput {
    // Line edits merge only when every head is a text file with one mode.
    let modes: BTreeSet<Option<(bool, Kind)>> = heads
        .values()
        .map(|v| v.as_ref().map(|c| (c.executable, c.kind)))
        .collect();
    let text = modes.iter().all(|m| matches!(m, Some((_, Kind::File))));
    if heads.len() > 1 && !collision && modes.len() == 1 && text {
        if let Some(text) = lines::LineVersion::merge_all(
            heads
                .keys()
                .map(|id| versions.get(&(path.to_string(), *id))),
        )
        .and_then(|v| v.render().ok())
        {
            let executable = modes.into_iter().next().flatten().is_some_and(|m| m.0);
            return PathOutput::File(Content {
                text,
                executable,
                base64: false,
                kind: Kind::File,
            });
        }
    }
    let Some((_, first)) = heads.first_key_value() else {
        return PathOutput::Deleted;
    };
    if !collision && heads.values().all(|v| v == first) {
        first.clone().map_or(PathOutput::Deleted, PathOutput::File)
    } else {
        PathOutput::Conflict(
            heads
                .iter()
                .map(|(patch, value)| Alternative::of(*patch, value))
                .collect(),
        )
    }
}
fn collect_outputs(outputs: &BTreeMap<String, PathOutput>) -> View {
    let mut view = View::default();
    for (path, output) in outputs {
        match output {
            PathOutput::File(content) => view.insert_file(path, content),
            PathOutput::Conflict(alternatives) => {
                view.conflicts.insert(path.clone(), alternatives.clone());
            }
            PathOutput::Deleted => {}
        }
    }
    view
}
impl View {
    fn insert_file(&mut self, path: &str, content: &Content) {
        self.files.insert(path.to_string(), content.text.clone());
        for (set, on) in [
            (&mut self.symlinks, content.kind == Kind::Symlink),
            (&mut self.submodules, content.kind == Kind::Submodule),
        ] {
            if on {
                set.insert(path.to_string());
            } else {
                set.remove(path);
            }
        }
        if content.executable {
            self.executable.insert(path.to_string());
        } else {
            self.executable.remove(path);
        }
        if content.base64 {
            self.binary.insert(path.to_string());
        } else {
            self.binary.remove(path);
        }
    }
    fn remove_file(&mut self, path: &str) -> Option<String> {
        self.symlinks.remove(path);
        self.submodules.remove(path);
        self.executable.remove(path);
        self.binary.remove(path);
        self.files.remove(path)
    }
}

/// Work counters describe dependency search and changed registers, not wall time.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ViewWork {
    pub processed_patches: usize,
    pub dependency_edges: usize,
    pub updated_paths: usize,
    pub rendered_paths: usize,
}

/// Append-only channel cache. No transitive-closure table: look backwards only
/// for current heads of touched paths, stopping as soon as all are found.
/// Registry handles are monotone and every dependency precedes its child.
#[derive(Default)]
pub struct IncrementalView {
    applied: BTreeSet<u64>,
    cursor: usize,
    heads: BTreeMap<String, BTreeMap<u64, Option<Content>>>,
    versions: BTreeMap<(String, u64), lines::LineVersion>,
    originals: BTreeMap<(String, u64), std::sync::Weak<str>>,
    live: BTreeSet<String>,
    outputs: BTreeMap<String, PathOutput>,
    /// Graph documents by identity, and the identity each edit resolves to.
    docs: documents::Docs,
    documents: BTreeMap<documents::DocId, o5s_kernel::model::DocumentGraph>,
    /// State heads (exists, executable) of each graph document.
    document_heads: BTreeMap<documents::DocId, BTreeMap<u64, documents::FileState>>,
    /// Location heads (path) of each graph document.
    document_locations: BTreeMap<documents::DocId, BTreeMap<u64, String>>,
    /// What each graph document shows at each of its paths.
    document_outputs: BTreeMap<documents::DocId, Vec<(String, PathOutput)>>,
}
impl IncrementalView {
    pub fn sync(&mut self, repo: &Repository, channel: &str) -> Result<ViewWork, Refusal> {
        let active: BTreeSet<u64> = repo
            .channels
            .get(channel)
            .ok_or_else(|| refuse("channel not found"))?
            .iter()
            .copied()
            .collect();
        if !self.applied.is_subset(&active) {
            *self = Self::default();
        }
        let pending: Vec<u64> = active.difference(&self.applied).copied().collect();
        let work = self.integrate(repo, pending)?;
        self.cursor = repo.channels[channel].len();
        Ok(work)
    }
    /// For channels maintained by admission only: existing membership is never
    /// removed or rewritten. Reads inspect just the appended suffix. Call `sync`
    /// instead for arbitrary externally mutated vectors, or discard the cache.
    pub fn sync_append_only(
        &mut self,
        repo: &Repository,
        channel: &str,
    ) -> Result<ViewWork, Refusal> {
        let ids = repo
            .channels
            .get(channel)
            .ok_or_else(|| refuse("channel not found"))?;
        if ids.len() < self.cursor {
            *self = Self::default();
        }
        let mut pending: Vec<_> = ids[self.cursor..]
            .iter()
            .copied()
            .filter(|id| !self.applied.contains(id))
            .collect();
        pending.sort_unstable();
        pending.dedup();
        let work = self.integrate(repo, pending)?;
        self.cursor = ids.len();
        Ok(work)
    }
    fn integrate(&mut self, repo: &Repository, pending: Vec<u64>) -> Result<ViewWork, Refusal> {
        let mut work = ViewWork::default();
        let mut dirty = BTreeSet::new();
        let mut dirty_documents = BTreeSet::new();
        for id in pending {
            let p = repo
                .patches
                .get(&id)
                .ok_or_else(|| refuse("corrupt patch registry"))?;
            if p.dependencies
                .iter()
                .any(|d| *d >= id || !self.applied.contains(d))
            {
                return Err(refuse("channel is not dependency closed"));
            }
            let mut targets = BTreeSet::new();
            for e in p.edits.iter().filter(|_| !p.dependencies.is_empty()) {
                if let Some(heads) = self.heads.get(&e.path) {
                    targets.extend(heads.keys().copied());
                }
            }
            let min = targets.first().copied().unwrap_or(u64::MAX);
            let mut found = BTreeSet::new();
            let mut seen = BTreeSet::new();
            let mut stack = p.dependencies.clone();
            while !targets.is_empty() {
                let Some(d) = stack.pop() else { break };
                work.dependency_edges += 1;
                if d < min || !seen.insert(d) {
                    continue;
                }
                if targets.remove(&d) {
                    found.insert(d);
                }
                if !targets.is_empty() {
                    let parent = repo
                        .patches
                        .get(&d)
                        .ok_or_else(|| refuse("corrupt dependency"))?;
                    stack.extend(parent.dependencies.iter().copied());
                }
            }
            for e in &p.edits {
                let heads = self.heads.entry(e.path.clone()).or_default();
                for d in &found {
                    heads.remove(d);
                    if let Some(old) = self.versions.remove(&(e.path.clone(), *d)) {
                        let key = (e.path.clone(), old.base);
                        drop(old);
                        if self
                            .originals
                            .get(&key)
                            .is_some_and(|v| v.strong_count() == 0)
                        {
                            self.originals.remove(&key);
                        }
                    }
                }
                heads.insert(id, e.value());
                let changes: Vec<_> = p
                    .line_edits
                    .iter()
                    .filter(|line| line.path == e.path)
                    .cloned()
                    .collect();
                if !changes.is_empty() {
                    let key = (e.path.clone(), changes[0].base);
                    let shared = self.originals.get(&key).and_then(std::sync::Weak::upgrade);
                    let version = repo.line_version_shared(&e.path, &changes, shared)?;
                    self.originals
                        .insert(key, std::sync::Arc::downgrade(&version.original));
                    self.versions.insert((e.path.clone(), id), version);
                }
                if heads.values().any(Option::is_some) {
                    self.live.insert(e.path.clone());
                } else {
                    self.live.remove(&e.path);
                }
                dirty.insert(e.path.clone());
                // Only ancestors and descendants can change structural status.
                for (i, c) in e.path.char_indices() {
                    if c == '/' && self.heads.contains_key(&e.path[..i]) {
                        dirty.insert(e.path[..i].to_string());
                    }
                }
                let prefix = format!("{}/", e.path);
                dirty.extend(
                    self.heads
                        .range(prefix.clone()..)
                        .take_while(|(path, _)| path.starts_with(&prefix))
                        .map(|(path, _)| path.clone()),
                );
                work.updated_paths += 1;
            }
            if !p.graph_edits.is_empty() {
                self.docs.prime(repo, id)?;
            }
            for edit in &p.graph_edits {
                let doc = self
                    .docs
                    .get(id, &edit.path)
                    .ok_or_else(|| refuse("graph document not resolved"))?
                    .clone();
                let graph = self.documents.entry(doc.clone()).or_default();
                documents::apply_graph_patch(graph, repo, &self.docs, p, edit)?;
                let superseded = documents::nearest(repo, &self.docs, &p.dependencies, &doc)?;
                let heads = self.document_heads.entry(doc.clone()).or_default();
                for d in superseded {
                    heads.remove(&d);
                }
                heads.insert(id, (!edit.remove, edit.executable));
                if self.docs.moves(id, &edit.path) {
                    let before = self.docs.locations(repo, &p.dependencies, &doc)?;
                    let at = self.document_locations.entry(doc.clone()).or_default();
                    for d in before.keys() {
                        at.remove(d);
                    }
                    at.insert(id, edit.path.clone());
                }
                dirty_documents.insert(doc);
                work.updated_paths += 1;
            }
            self.applied.insert(id);
            work.processed_patches += 1;
        }
        for path in dirty {
            let prefix = format!("{path}/");
            let ancestor = path
                .char_indices()
                .any(|(i, c)| c == '/' && self.live.contains(&path[..i]));
            let descendant = self
                .live
                .range(prefix.clone()..)
                .next()
                .is_some_and(|p| p.starts_with(&prefix));
            let collision = self.live.contains(&path) && (ancestor || descendant);
            self.outputs.insert(
                path.clone(),
                render_path(&path, &self.heads[&path], &self.versions, collision),
            );
            work.rendered_paths += 1;
        }
        for doc in dirty_documents {
            self.document_outputs.insert(
                doc.clone(),
                documents::render_document(
                    &self.documents[&doc],
                    &self.document_heads[&doc],
                    &self.document_locations[&doc],
                )?,
            );
            work.rendered_paths += 1;
        }
        Ok(work)
    }
    /// Render content without copying the full channel membership history.
    pub fn content(&self) -> Result<View, Refusal> {
        let mut view = collect_outputs(&self.outputs);
        let (outputs, heads) = documents::by_path(&self.document_outputs, &self.document_heads);
        documents::overlay_outputs(&outputs, &heads, &mut view);
        Ok(view)
    }
    pub fn patch_count(&self) -> usize {
        self.applied.len()
    }
    pub fn view(&self) -> Result<View, Refusal> {
        let mut view = self.content()?;
        view.patches = self.applied.iter().copied().collect();
        Ok(view)
    }
}

/// The reply of `GET /api/snapshot`: a patch's snapshot with the patch's
/// hash and the hashes it contains, so a check names exactly what it ran on.
pub fn snapshot_json(repo: &Repository, patch: u64, view: &View) -> serde_json::Value {
    let hashes: Vec<&str> = view
        .patches
        .iter()
        .filter_map(|id| repo.patches.get(id).map(|p| p.hash.as_str()))
        .collect();
    serde_json::json!({"patch":patch,"hash":repo.patches.get(&patch).map(|p| p.hash.as_str()),
        "patches":view.patches,"hashes":hashes,"files":view.files,"conflicts":view.conflicts,
        "executable":view.executable,"binary":view.binary,"symlinks":view.symlinks,
        "submodules":view.submodules})
}

pub mod projection;

pub mod lines;

pub mod documents;

pub mod workspace;

pub mod limits;
