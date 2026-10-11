//! Immutable patch registry and file-register materialization. The admission
//! decision delegates to the Aeneas-extracted h5i kernel. Registry, hashing,
//! authorization and materialization currently belong to the tested trust boundary.
use h5i_app_core::{Kernel, TenantId};
use o5s_kernel::patch;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    /// None is a tombstone, not an empty file.
    pub content: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Patch {
    pub id: u64,
    pub hash: String,
    pub author: u64,
    pub intent: String,
    pub dependencies: Vec<u64>,
    pub edits: Vec<Edit>,
    pub approvals: BTreeSet<u64>,
    pub checks_passed: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Repository {
    pub patches: BTreeMap<u64, Patch>,
    pub channels: BTreeMap<String, Vec<u64>>,
    pub events: Vec<Event>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub actor: u64,
    pub action: String,
    pub patch: Option<u64>,
    pub channel: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Database {
    /// Tenant IDs from authenticated tokens, never request bodies.
    pub tenants: BTreeMap<u64, BTreeMap<String, Repository>>,
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
    Approve {
        repo: String,
        patch: u64,
    },
    Check {
        repo: String,
        patch: u64,
        passed: bool,
    },
    Merge {
        repo: String,
        channel: String,
        patch: u64,
    },
}
impl Command {
    pub fn repo(&self) -> &str {
        match self {
            Self::Create { repo }
            | Self::Fork { repo, .. }
            | Self::Propose { repo, .. }
            | Self::Approve { repo, .. }
            | Self::Check { repo, .. }
            | Self::Merge { repo, .. } => repo,
        }
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
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
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

/// This adapter carries only server-derived facts to the extracted decision.
pub struct Admission;
pub struct AdmissionCommand {
    pub dependencies: Vec<u64>,
    pub approved: bool,
    pub checks_passed: bool,
    pub patch: u64,
}
impl Kernel for Admission {
    type Principal = Actor;
    type Snapshot = Vec<u64>;
    type Command = AdmissionCommand;
    type WriteSet = Vec<u64>;
    type Reply = Vec<u64>;
    type Error = patch::MergeError;
    fn tenant(a: &Actor) -> TenantId {
        TenantId(a.tenant)
    }
    fn transition(
        _: &Actor,
        s: &Vec<u64>,
        c: &AdmissionCommand,
    ) -> Result<(Vec<u64>, Vec<u64>), Self::Error> {
        let out = patch::merge(s, &c.dependencies, c.approved, c.checks_passed, c.patch)?;
        Ok((out.clone(), out))
    }
    fn apply(_: &Vec<u64>, w: &Vec<u64>) -> Vec<u64> {
        w.clone()
    }
}

impl Database {
    pub fn repo(&self, actor: &Actor, name: &str) -> Result<&Repository, Refusal> {
        self.tenants
            .get(&actor.tenant)
            .and_then(|t| t.get(name))
            .ok_or_else(|| refuse("repository not found"))
    }
    /// Called under a single transaction lock. Errors leave the database intact.
    pub fn execute(
        &mut self,
        actor: &Actor,
        command: Command,
    ) -> Result<serde_json::Value, Refusal> {
        if !valid_name(command.repo()) {
            return Err(refuse("invalid repository name"));
        }
        if let Command::Create { repo } = &command {
            let t = self.tenants.entry(actor.tenant).or_default();
            if t.contains_key(repo) {
                return Err(refuse("repository already exists"));
            }
            let mut r = Repository::default();
            r.channels.insert("main".into(), vec![]);
            r.events.push(Event {
                actor: actor.user,
                action: "create".into(),
                patch: None,
                channel: None,
            });
            t.insert(repo.clone(), r);
            return Ok(serde_json::json!({"repo":repo}));
        }
        let repo = self
            .tenants
            .get_mut(&actor.tenant)
            .and_then(|t| t.get_mut(command.repo()))
            .ok_or_else(|| refuse("repository not found"))?;
        let (reply, action, patch_id, channel) = match command {
            Command::Create { .. } => unreachable!(),
            Command::Fork {
                source, channel, ..
            } => {
                if !valid_name(&channel) {
                    return Err(refuse("invalid channel name"));
                }
                if repo.channels.contains_key(&channel) {
                    return Err(refuse("channel already exists"));
                }
                let set = repo
                    .channels
                    .get(&source)
                    .ok_or_else(|| refuse("source channel not found"))?
                    .clone();
                repo.channels.insert(channel.clone(), set);
                (
                    serde_json::json!({"channel":channel}),
                    "fork",
                    None,
                    Some(channel),
                )
            }
            Command::Propose {
                intent,
                mut dependencies,
                mut edits,
                ..
            } => {
                if intent.trim().is_empty() || intent.len() > 8192 {
                    return Err(refuse("intent required (max 8192 bytes)"));
                }
                if edits.is_empty() || edits.len() > 1024 {
                    return Err(refuse("1..1024 edits required"));
                }
                dependencies.sort_unstable();
                dependencies.dedup();
                if dependencies.iter().any(|d| !repo.patches.contains_key(d)) {
                    return Err(refuse("unknown dependency"));
                }
                edits.sort_by(|a, b| a.path.cmp(&b.path));
                if edits.iter().any(|e| !valid_path(&e.path))
                    || edits.windows(2).any(|e| e[0].path == e[1].path)
                {
                    return Err(refuse("invalid or duplicate edit path"));
                }
                let hash = hash_patch(actor.user, &intent, &dependencies, &edits, repo)?;
                if let Some(p) = repo.patches.values().find(|p| p.hash == hash) {
                    return Ok(serde_json::json!({"patch":p.id,"hash":p.hash,"duplicate":true}));
                }
                let id = repo
                    .patches
                    .last_key_value()
                    .map_or(Some(1), |(id, _)| id.checked_add(1))
                    .ok_or_else(|| refuse("patch ID space exhausted"))?;
                repo.patches.insert(
                    id,
                    Patch {
                        id,
                        hash: hash.clone(),
                        author: actor.user,
                        intent,
                        dependencies,
                        edits,
                        approvals: BTreeSet::new(),
                        checks_passed: false,
                    },
                );
                (
                    serde_json::json!({"patch":id,"hash":hash}),
                    "propose",
                    Some(id),
                    None,
                )
            }
            Command::Approve { patch, .. } => {
                let p = repo
                    .patches
                    .get_mut(&patch)
                    .ok_or_else(|| refuse("patch not found"))?;
                if p.author == actor.user {
                    return Err(refuse("independent reviewer required"));
                }
                p.approvals.insert(actor.user);
                (
                    serde_json::json!({"patch":patch,"approved":true}),
                    "approve",
                    Some(patch),
                    None,
                )
            }
            Command::Check { patch, passed, .. } => {
                let p = repo
                    .patches
                    .get_mut(&patch)
                    .ok_or_else(|| refuse("patch not found"))?;
                if p.author == actor.user {
                    return Err(refuse("independent check reporter required"));
                }
                p.checks_passed = passed;
                (
                    serde_json::json!({"patch":patch,"checks_passed":passed}),
                    "check",
                    Some(patch),
                    None,
                )
            }
            Command::Merge { channel, patch, .. } => {
                let s = repo
                    .channels
                    .get(&channel)
                    .ok_or_else(|| refuse("channel not found"))?;
                let p = repo
                    .patches
                    .get(&patch)
                    .ok_or_else(|| refuse("patch not found"))?;
                let cmd = AdmissionCommand {
                    dependencies: p.dependencies.clone(),
                    approved: !p.approvals.is_empty(),
                    checks_passed: p.checks_passed,
                    patch,
                };
                let (w, _) = Admission::transition(actor, s, &cmd).map_err(|e| {
                    refuse(match e {
                        patch::MergeError::Unapproved => "patch unapproved",
                        patch::MergeError::ChecksFailed => "checks failed",
                        patch::MergeError::MissingDependency => "dependency not integrated",
                    })
                })?;
                let out = Admission::apply(s, &w);
                repo.channels.insert(channel.clone(), out);
                (
                    serde_json::json!({"patch":patch,"channel":channel}),
                    "merge",
                    Some(patch),
                    Some(channel),
                )
            }
        };
        repo.events.push(Event {
            actor: actor.user,
            action: action.into(),
            patch: patch_id,
            channel,
        });
        Ok(reply)
    }
}

/// Full hashes include dependency *hashes*, rather than local numeric handles.
/// Canonical sorted edits and dependencies make retry identity deterministic.
fn hash_patch(
    author: u64,
    intent: &str,
    deps: &[u64],
    edits: &[Edit],
    repo: &Repository,
) -> Result<String, Refusal> {
    let mut deps: Vec<&str> = deps
        .iter()
        .map(|id| repo.patches[id].hash.as_str())
        .collect();
    deps.sort_unstable();
    let encoded = serde_json::to_vec(&("o5s-patch-v1", author, intent, deps, edits))
        .map_err(|e| Refusal(e.to_string()))?;
    Ok(Sha256::digest(encoded)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alternative {
    pub patch: u64,
    pub content: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct View {
    pub files: BTreeMap<String, String>,
    pub conflicts: BTreeMap<String, Vec<Alternative>>,
    pub patches: Vec<u64>,
}
impl Repository {
    /// Compute causal ancestry once, in registry order (dependencies always
    /// precede their child), without recursion or synthetic pseudo-edges.
    pub fn materialize(&self, channel: &str) -> Result<View, Refusal> {
        let active: BTreeSet<u64> = self
            .channels
            .get(channel)
            .ok_or_else(|| refuse("channel not found"))?
            .iter()
            .copied()
            .collect();
        let mut ancestors: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        let mut heads: BTreeMap<String, BTreeMap<u64, Option<String>>> = BTreeMap::new();
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
                h.insert(*id, e.content.clone());
            }
            ancestors.insert(*id, ancestry);
        }
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
        let mut files = BTreeMap::new();
        let mut conflicts = BTreeMap::new();
        for (path, h) in heads {
            let first = h.first_key_value().unwrap().1;
            if !collisions.contains(&path) && h.values().all(|v| v == first) {
                if let Some(content) = first {
                    files.insert(path, content.clone());
                }
            } else {
                conflicts.insert(
                    path,
                    h.into_iter()
                        .map(|(patch, content)| Alternative { patch, content })
                        .collect(),
                );
            }
        }
        Ok(View {
            files,
            conflicts,
            patches: active.into_iter().collect(),
        })
    }
}
