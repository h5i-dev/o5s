//! The transactional o5s patch registry.
//!
//! Every registry decision the server commits is made by [`transition`]: the
//! repository directory, immutable patch registration, independent review and
//! check facts, channel forks and dependency-closed admission. The kernel is
//! Rust in the subset Aeneas translates to Lean (no iterators, no `?`, index
//! loops), and the proofs in `proofs/` are about this code. Admission calls
//! the proven [`o5s_kernel::patch::merge`].
//!
//! Each repository is its own h5i tenant (`Principal::scope`), so the
//! PostgreSQL engine loads and locks one repository per transaction. Every
//! organization's directory of repository names is a separate tenant whose
//! scope is the organization itself. Native code supplies the scope, the
//! SHA-256 identity of a patch body and the validated document paths; the
//! kernel refuses any repository whose stored organization and name are not
//! the caller's, so a wrong scope can never reveal or change another
//! organization's repository.

use h5i_app_std::bytes;
use h5i_app_std::set;
use o5s_kernel::patch;

pub type Text = Vec<u8>;

/// Scopes at or above this value are repositories; organizations are below it.
pub const REPOSITORY_SCOPE: u64 = 4611686018427387904;
/// The longest repository or channel name, in bytes.
pub const MAX_NAME: usize = 64;
/// Hex SHA-256 identities.
pub const HASH_LEN: usize = 64;
/// The most patches one admission batch may name.
pub const MAX_BATCH: usize = 10000;

/// Event actions, as stored in `Event::action`.
pub const CREATE: u64 = 1;
pub const FORK: u64 = 2;
pub const PROPOSE: u64 = 3;
pub const APPROVE: u64 = 4;
pub const CHECK: u64 = 5;
pub const MERGE: u64 = 6;
pub const GRANT: u64 = 7;

/// Repository access levels. A repository without role rows is open: every
/// member of its organization has `ADMIN`. Once restricted, a user without a
/// row has no access and does not see the repository.
pub const READ: u64 = 1;
/// Propose and fork.
pub const WRITE: u64 = 2;
/// Approve, report checks and merge.
pub const REVIEW: u64 = 3;
/// Grant and revoke access.
pub const ADMIN: u64 = 4;

/// The authenticated caller and the tenant the engine loads for the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    /// Organization from a verified token, never from a request body.
    pub org: u64,
    pub user: u64,
    /// The organization for directory commands; a repository key otherwise.
    pub scope: u64,
}

h5i_app_schema::schema! {
    mapping registry_tables for o5s_registry, writes Write, lean "proofs/generated/Schema.lean";

    /// One tenant's rows: either a repository or an organization directory.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Snapshot {
        repository: Repository,
        patches: Vec<PatchRow>,
        dependencies: Vec<Dependency>,
        approvals: Vec<Approval>,
        checks: Vec<CheckReport>,
        channels: Vec<Channel>,
        members: Vec<Member>,
        paths: Vec<PathKind>,
        events: Vec<Event>,
        bodies: Vec<Body>,
        listings: Vec<Listing>,
        roles: Vec<Role>,
    }

    /// The repository of this tenant and its identifier counters. A fresh
    /// tenant reads as zeros: no repository has been created.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Repository in "o5s_repository" {
        key {}
        org: u64,
        label: Text,
        created: bool,
        patches: u64,
        channels: u64,
        paths: u64,
        events: u64,
    }

    /// An immutable patch: its content identity and author.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct PatchRow in "o5s_patches" {
        key { id: u64 }
        hash: Text,
        author: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Dependency in "o5s_dependencies" {
        key { patch: u64, dependency: u64 }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Approval in "o5s_approvals" {
        key { patch: u64, reviewer: u64 }
    }

    /// The latest independent check report on a patch.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct CheckReport in "o5s_checks" {
        key { patch: u64 }
        passed: bool,
        reporter: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Channel in "o5s_channels" {
        key { id: u64 }
        label: Text,
        size: u64,
    }

    /// The patch admitted at `position` of a channel's append-only membership.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Member in "o5s_members" {
        key { channel: u64, position: u64 }
        patch: u64,
    }

    /// Whether a path holds a graph document or a legacy whole-file/line document.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct PathKind in "o5s_paths" {
        key { id: u64 }
        path: Text,
        graph: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Event in "o5s_events" {
        key { seq: u64 }
        actor: u64,
        action: u64,
        patch: Option<u64>,
        channel: Option<u64>,
    }

    /// The canonical patch record whose SHA-256 is `PatchRow::hash`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Body in "o5s_bodies" {
        key { patch: u64 }
        bytes: Text,
    }

    /// A user's access level to a restricted repository (`READ`..`ADMIN`; `0`
    /// revokes).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Role in "o5s_roles" {
        key { user: u64 }
        level: u64,
    }

    /// A repository name in an organization's directory.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Listing in "o5s_listings" {
        key { repo: u64 }
        label: Text,
    }
}

/// A document path a proposal touches, and the edit model it uses.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PathUse {
    pub path: Text,
    pub graph: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Command {
    /// List `name` under key `repo` in the caller's organization directory.
    Register {
        name: Text,
        repo: u64,
    },
    Create {
        name: Text,
    },
    Fork {
        repo: Text,
        source: Text,
        channel: Text,
    },
    /// Register an immutable patch. `hash` is the SHA-256 of `body`.
    Propose {
        repo: Text,
        hash: Text,
        dependencies: Vec<u64>,
        paths: Vec<PathUse>,
        body: Text,
    },
    Approve {
        repo: Text,
        patch: u64,
    },
    Check {
        repo: Text,
        patch: u64,
        passed: bool,
    },
    /// Admit strictly ascending patch handles, all or none.
    Merge {
        repo: Text,
        channel: Text,
        patches: Vec<u64>,
    },
    /// Set `user`'s access level. On an open repository this restricts it
    /// and makes the caller an administrator.
    Grant {
        repo: Text,
        user: u64,
        level: u64,
    },
    /// Loads for reads; they never commit. `Probe` reads only the repository row.
    Probe,
    Inspect,
    ReadBody {
        patch: u64,
    },
    ReadEvents,
    List,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Write {
    SetRepository(Repository),
    PutPatch(PatchRow),
    PutDependency(Dependency),
    PutApproval(Approval),
    PutCheck(CheckReport),
    PutChannel(Channel),
    PutMember(Member),
    PutPath(PathKind),
    PutEvent(Event),
    PutBody(Body),
    PutListing(Listing),
    PutRole(Role),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Reply {
    Registered {
        repo: u64,
    },
    Created,
    Forked {
        channel: u64,
    },
    Proposed {
        patch: u64,
        duplicate: bool,
    },
    Approved {
        patch: u64,
    },
    Checked {
        patch: u64,
        passed: bool,
    },
    /// `admitted` counts patches that were not already members.
    Merged {
        channel: u64,
        admitted: u64,
    },
    Granted {
        user: u64,
        level: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Error {
    /// The scope does not fit the command: a directory command outside the
    /// caller's organization, or a repository command on an organization.
    WrongScope,
    InvalidName,
    RepositoryExists,
    RepositoryNotFound,
    /// The repository key already names another repository.
    KeyCollision,
    InvalidChannelName,
    ChannelExists,
    SourceChannelNotFound,
    ChannelNotFound,
    InvalidHash,
    UnknownDependency,
    /// The path holds a graph document; whole-file and line edits are refused.
    GraphDocument,
    /// The path holds a legacy document; graph operations need a migration.
    LegacyDocument,
    PatchNotFound,
    IndependentReviewerRequired,
    IndependentCheckReporterRequired,
    /// Empty, too large, or not strictly ascending.
    InvalidBatch,
    Unapproved,
    ChecksFailed,
    MissingDependency,
    /// An identifier counter would overflow.
    Exhausted,
    /// A read-only load, never a transition.
    ReadOnly,
    /// The caller's access level does not allow the command.
    Forbidden,
    /// Not a level between `0` and `ADMIN`.
    InvalidLevel,
    /// Users cannot change their own level on a restricted repository, so
    /// one administrator always remains.
    OwnLevel,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

/// An ASCII letter, digit, `-` or `_`.
pub fn name_byte(c: u8) -> bool {
    (c >= b'a' && c <= b'z')
        || (c >= b'A' && c <= b'Z')
        || (c >= b'0' && c <= b'9')
        || c == b'-'
        || c == b'_'
}

/// 1 to 64 bytes of ASCII letters, digits, `-` and `_`.
pub fn valid_name(name: &Text) -> bool {
    if name.len() == 0 || name.len() > MAX_NAME {
        return false;
    }
    let mut i = 0;
    while i < name.len() {
        if !name_byte(name[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Lowercase hexadecimal of the expected length.
pub fn valid_hash(hash: &Text) -> bool {
    if hash.len() != HASH_LEN {
        return false;
    }
    let mut i = 0;
    while i < hash.len() {
        let c = hash[i];
        if !((c >= b'0' && c <= b'9') || (c >= b'a' && c <= b'f')) {
            return false;
        }
        i += 1;
    }
    true
}

/// May `actor` read or change this tenant's repository named `name`?
pub fn visible(actor: &Principal, repository: &Repository, name: &Text) -> bool {
    actor.scope >= REPOSITORY_SCOPE
        && repository.created
        && repository.org == actor.org
        && bytes::eq(&repository.label, name)
}

/// `user`'s access level: `ADMIN` on an open repository (no role rows).
pub fn level(roles: &Vec<Role>, user: u64) -> u64 {
    if roles.len() == 0 {
        return ADMIN;
    }
    let mut i = 0;
    while i < roles.len() {
        if roles[i].user == user {
            return roles[i].level;
        }
        i += 1;
    }
    0
}

/// May `actor` read this tenant's repository named `name`?
pub fn readable(actor: &Principal, s: &Snapshot, name: &Text) -> bool {
    visible(actor, &s.repository, name) && level(&s.roles, actor.user) >= READ
}

/// Refuse a command that needs level `need`: an invisible repository, or
/// one the caller cannot read, is not found; a readable one is forbidden.
pub fn authorize(actor: &Principal, s: &Snapshot, name: &Text, need: u64) -> Option<Error> {
    if !readable(actor, s, name) {
        Some(Error::RepositoryNotFound)
    } else if level(&s.roles, actor.user) < need {
        Some(Error::Forbidden)
    } else {
        None
    }
}

pub fn find_patch(ps: &Vec<PatchRow>, id: u64) -> Option<PatchRow> {
    let mut i = 0;
    while i < ps.len() {
        if ps[i].id == id {
            return Some(ps[i].clone());
        }
        i += 1;
    }
    None
}

/// The handle of the patch with this content identity.
pub fn find_hash(ps: &Vec<PatchRow>, hash: &Text) -> Option<u64> {
    let mut i = 0;
    while i < ps.len() {
        if bytes::eq(&ps[i].hash, hash) {
            return Some(ps[i].id);
        }
        i += 1;
    }
    None
}

pub fn find_channel(cs: &Vec<Channel>, name: &Text) -> Option<Channel> {
    let mut i = 0;
    while i < cs.len() {
        if bytes::eq(&cs[i].label, name) {
            return Some(cs[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_path(ks: &Vec<PathKind>, path: &Text) -> Option<PathKind> {
    let mut i = 0;
    while i < ks.len() {
        if bytes::eq(&ks[i].path, path) {
            return Some(ks[i].clone());
        }
        i += 1;
    }
    None
}

/// The listing with this name in a directory.
pub fn find_listing(ls: &Vec<Listing>, name: &Text) -> Option<Listing> {
    let mut i = 0;
    while i < ls.len() {
        if bytes::eq(&ls[i].label, name) {
            return Some(ls[i].clone());
        }
        i += 1;
    }
    None
}

pub fn listing_key_used(ls: &Vec<Listing>, repo: u64) -> bool {
    let mut i = 0;
    while i < ls.len() {
        if ls[i].repo == repo {
            return true;
        }
        i += 1;
    }
    false
}

/// The patches admitted to channel `channel`.
pub fn members_of(ms: &Vec<Member>, channel: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ms.len() {
        if ms[i].channel == channel {
            out.push(ms[i].patch);
        }
        i += 1;
    }
    out
}

/// The recorded dependencies of `patch`.
pub fn dependencies_of(ds: &Vec<Dependency>, patch: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ds.len() {
        if ds[i].patch == patch {
            out.push(ds[i].dependency);
        }
        i += 1;
    }
    out
}

pub fn approved(as_: &Vec<Approval>, patch: u64) -> bool {
    let mut i = 0;
    while i < as_.len() {
        if as_[i].patch == patch {
            return true;
        }
        i += 1;
    }
    false
}

/// The latest check report on `patch` passed.
pub fn checks_passed(cs: &Vec<CheckReport>, patch: u64) -> bool {
    let mut i = 0;
    while i < cs.len() {
        if cs[i].patch == patch {
            return cs[i].passed;
        }
        i += 1;
    }
    false
}

/// Every handle names a patch.
pub fn all_known(ps: &Vec<PatchRow>, ids: &Vec<u64>) -> bool {
    let mut i = 0;
    while i < ids.len() {
        match find_patch(ps, ids[i]) {
            Some(_) => {}
            None => return false,
        }
        i += 1;
    }
    true
}

/// Non-empty, at most `MAX_BATCH`, strictly ascending.
pub fn valid_batch(ids: &Vec<u64>) -> bool {
    if ids.len() == 0 || ids.len() > MAX_BATCH {
        return false;
    }
    let mut i = 1;
    while i < ids.len() {
        if ids[i - 1] >= ids[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The repository row with one more event.
fn bump_events(r: &Repository) -> Repository {
    Repository {
        org: r.org,
        label: r.label.clone(),
        created: r.created,
        patches: r.patches,
        channels: r.channels,
        paths: r.paths,
        events: r.events + 1,
    }
}

fn event(seq: u64, actor: u64, action: u64, patch: Option<u64>, channel: Option<u64>) -> Event {
    Event {
        seq,
        actor,
        action,
        patch,
        channel,
    }
}

fn grant(actor: &Principal, s: &Snapshot, user: u64, lvl: u64) -> Outcome {
    let r = &s.repository;
    if lvl > ADMIN {
        return Err(Error::InvalidLevel);
    }
    if r.events == u64::MAX {
        return Err(Error::Exhausted);
    }
    let open = s.roles.len() == 0;
    if user == actor.user && (!open || lvl != ADMIN) {
        return Err(Error::OwnLevel);
    }
    let mut ws = Vec::new();
    if open {
        ws.push(Write::PutRole(Role {
            user: actor.user,
            level: ADMIN,
        }));
    }
    if user != actor.user {
        ws.push(Write::PutRole(Role { user, level: lvl }));
    }
    ws.push(Write::SetRepository(bump_events(r)));
    ws.push(Write::PutEvent(event(
        r.events + 1,
        actor.user,
        GRANT,
        None,
        None,
    )));
    Ok((ws, Reply::Granted { user, level: lvl }))
}

fn register(actor: &Principal, ls: &Vec<Listing>, name: &Text, repo: u64) -> Outcome {
    if actor.scope != actor.org || actor.org >= REPOSITORY_SCOPE || repo < REPOSITORY_SCOPE {
        return Err(Error::WrongScope);
    }
    if !valid_name(name) {
        return Err(Error::InvalidName);
    }
    match find_listing(ls, name) {
        Some(l) => {
            if l.repo == repo {
                Ok((Vec::new(), Reply::Registered { repo }))
            } else {
                Err(Error::KeyCollision)
            }
        }
        None => {
            if listing_key_used(ls, repo) {
                return Err(Error::KeyCollision);
            }
            let mut ws = Vec::new();
            ws.push(Write::PutListing(Listing {
                repo,
                label: name.clone(),
            }));
            Ok((ws, Reply::Registered { repo }))
        }
    }
}

fn create(actor: &Principal, r: &Repository, name: &Text) -> Outcome {
    if actor.scope < REPOSITORY_SCOPE {
        return Err(Error::WrongScope);
    }
    if !valid_name(name) {
        return Err(Error::InvalidName);
    }
    if r.created {
        if r.org == actor.org && bytes::eq(&r.label, name) {
            return Err(Error::RepositoryExists);
        }
        return Err(Error::KeyCollision);
    }
    let mut main = Vec::new();
    main.push(b'm');
    main.push(b'a');
    main.push(b'i');
    main.push(b'n');
    let mut ws = Vec::new();
    ws.push(Write::SetRepository(Repository {
        org: actor.org,
        label: name.clone(),
        created: true,
        patches: 0,
        channels: 1,
        paths: 0,
        events: 1,
    }));
    ws.push(Write::PutChannel(Channel {
        id: 1,
        label: main,
        size: 0,
    }));
    ws.push(Write::PutEvent(event(1, actor.user, CREATE, None, None)));
    Ok((ws, Reply::Created))
}

fn fork(user: u64, s: &Snapshot, source: &Text, name: &Text) -> Outcome {
    let r = &s.repository;
    if !valid_name(name) {
        return Err(Error::InvalidChannelName);
    }
    match find_channel(&s.channels, name) {
        Some(_) => return Err(Error::ChannelExists),
        None => {}
    }
    let src = match find_channel(&s.channels, source) {
        Some(c) => c,
        None => return Err(Error::SourceChannelNotFound),
    };
    if r.channels == u64::MAX || r.events == u64::MAX {
        return Err(Error::Exhausted);
    }
    let id = r.channels + 1;
    let mut ws = Vec::new();
    ws.push(Write::PutChannel(Channel {
        id,
        label: name.clone(),
        size: src.size,
    }));
    let mut i = 0;
    while i < s.members.len() {
        let m = s.members[i];
        if m.channel == src.id {
            ws.push(Write::PutMember(Member {
                channel: id,
                position: m.position,
                patch: m.patch,
            }));
        }
        i += 1;
    }
    ws.push(Write::SetRepository(Repository {
        org: r.org,
        label: r.label.clone(),
        created: r.created,
        patches: r.patches,
        channels: id,
        paths: r.paths,
        events: r.events + 1,
    }));
    ws.push(Write::PutEvent(event(
        r.events + 1,
        user,
        FORK,
        None,
        Some(id),
    )));
    Ok((ws, Reply::Forked { channel: id }))
}

/// Register the paths a proposal touches, keeping each path's edit model.
/// Returns the path writes and the new path counter.
fn register_paths(
    known: &Vec<PathKind>,
    uses: &Vec<PathUse>,
    counter: u64,
) -> Result<(Vec<PathKind>, u64), Error> {
    let mut added: Vec<PathKind> = Vec::new();
    let mut next = counter;
    let mut i = 0;
    while i < uses.len() {
        let u = &uses[i];
        let existing = match find_path(known, &u.path) {
            Some(k) => Some(k),
            None => find_path(&added, &u.path),
        };
        match existing {
            Some(k) => {
                if k.graph != u.graph {
                    if k.graph {
                        return Err(Error::GraphDocument);
                    }
                    return Err(Error::LegacyDocument);
                }
            }
            None => {
                if next == u64::MAX {
                    return Err(Error::Exhausted);
                }
                next += 1;
                added.push(PathKind {
                    id: next,
                    path: u.path.clone(),
                    graph: u.graph,
                });
            }
        }
        i += 1;
    }
    Ok((added, next))
}

fn propose(
    user: u64,
    s: &Snapshot,
    hash: &Text,
    deps: &Vec<u64>,
    uses: &Vec<PathUse>,
    body: &Text,
) -> Outcome {
    let r = &s.repository;
    if !valid_hash(hash) {
        return Err(Error::InvalidHash);
    }
    if !all_known(&s.patches, deps) {
        return Err(Error::UnknownDependency);
    }
    match find_hash(&s.patches, hash) {
        Some(id) => {
            return Ok((
                Vec::new(),
                Reply::Proposed {
                    patch: id,
                    duplicate: true,
                },
            ));
        }
        None => {}
    }
    let (added, paths) = match register_paths(&s.paths, uses, r.paths) {
        Ok(x) => x,
        Err(e) => return Err(e),
    };
    if r.patches == u64::MAX || r.events == u64::MAX {
        return Err(Error::Exhausted);
    }
    let id = r.patches + 1;
    let mut ws = Vec::new();
    ws.push(Write::PutPatch(PatchRow {
        id,
        hash: hash.clone(),
        author: user,
    }));
    ws.push(Write::PutBody(Body {
        patch: id,
        bytes: body.clone(),
    }));
    let mut i = 0;
    while i < deps.len() {
        ws.push(Write::PutDependency(Dependency {
            patch: id,
            dependency: deps[i],
        }));
        i += 1;
    }
    let mut j = 0;
    while j < added.len() {
        ws.push(Write::PutPath(added[j].clone()));
        j += 1;
    }
    ws.push(Write::SetRepository(Repository {
        org: r.org,
        label: r.label.clone(),
        created: r.created,
        patches: id,
        channels: r.channels,
        paths,
        events: r.events + 1,
    }));
    ws.push(Write::PutEvent(event(
        r.events + 1,
        user,
        PROPOSE,
        Some(id),
        None,
    )));
    Ok((
        ws,
        Reply::Proposed {
            patch: id,
            duplicate: false,
        },
    ))
}

fn approve(user: u64, s: &Snapshot, patch: u64) -> Outcome {
    let r = &s.repository;
    let p = match find_patch(&s.patches, patch) {
        Some(p) => p,
        None => return Err(Error::PatchNotFound),
    };
    if p.author == user {
        return Err(Error::IndependentReviewerRequired);
    }
    if r.events == u64::MAX {
        return Err(Error::Exhausted);
    }
    let mut ws = Vec::new();
    ws.push(Write::PutApproval(Approval {
        patch,
        reviewer: user,
    }));
    ws.push(Write::SetRepository(bump_events(r)));
    ws.push(Write::PutEvent(event(
        r.events + 1,
        user,
        APPROVE,
        Some(patch),
        None,
    )));
    Ok((ws, Reply::Approved { patch }))
}

fn check(user: u64, s: &Snapshot, patch: u64, passed: bool) -> Outcome {
    let r = &s.repository;
    let p = match find_patch(&s.patches, patch) {
        Some(p) => p,
        None => return Err(Error::PatchNotFound),
    };
    if p.author == user {
        return Err(Error::IndependentCheckReporterRequired);
    }
    if r.events == u64::MAX {
        return Err(Error::Exhausted);
    }
    let mut ws = Vec::new();
    ws.push(Write::PutCheck(CheckReport {
        patch,
        passed,
        reporter: user,
    }));
    ws.push(Write::SetRepository(bump_events(r)));
    ws.push(Write::PutEvent(event(
        r.events + 1,
        user,
        CHECK,
        Some(patch),
        None,
    )));
    Ok((ws, Reply::Checked { patch, passed }))
}

fn merge_error(e: patch::MergeError) -> Error {
    match e {
        patch::MergeError::Unapproved => Error::Unapproved,
        patch::MergeError::ChecksFailed => Error::ChecksFailed,
        patch::MergeError::MissingDependency => Error::MissingDependency,
    }
}

/// The state of an admission batch after each patch.
pub struct Admission {
    pub members: Vec<u64>,
    pub size: u64,
    pub seq: u64,
    pub admitted: u64,
    pub writes: Vec<Write>,
}

/// Admit one patch through `patch::merge`, or say why not.
fn admit_one(user: u64, s: &Snapshot, channel: u64, id: u64, st: &mut Admission) -> Option<Error> {
    match find_patch(&s.patches, id) {
        Some(_) => {}
        None => return Some(Error::PatchNotFound),
    }
    let present = set::contains(&st.members, &id);
    let deps = dependencies_of(&s.dependencies, id);
    match patch::merge(
        &st.members,
        &deps,
        approved(&s.approvals, id),
        checks_passed(&s.checks, id),
        id,
    ) {
        Err(e) => return Some(merge_error(e)),
        Ok(next) => st.members = next,
    }
    if !present {
        if st.size == u64::MAX || st.admitted == u64::MAX {
            return Some(Error::Exhausted);
        }
        st.writes.push(Write::PutMember(Member {
            channel,
            position: st.size,
            patch: id,
        }));
        st.size += 1;
        st.admitted += 1;
    }
    if st.seq == u64::MAX {
        return Some(Error::Exhausted);
    }
    st.seq += 1;
    st.writes.push(Write::PutEvent(event(
        st.seq,
        user,
        MERGE,
        Some(id),
        Some(channel),
    )));
    None
}

/// Admit `ids` in ascending order, all or none. Every dependency of an
/// admitted patch is already a member or earlier in the batch.
fn merge(user: u64, s: &Snapshot, name: &Text, ids: &Vec<u64>) -> Outcome {
    let r = &s.repository;
    if !valid_batch(ids) {
        return Err(Error::InvalidBatch);
    }
    let c = match find_channel(&s.channels, name) {
        Some(c) => c,
        None => return Err(Error::ChannelNotFound),
    };
    let mut st = Admission {
        members: members_of(&s.members, c.id),
        size: c.size,
        seq: r.events,
        admitted: 0,
        writes: Vec::new(),
    };
    let mut failure: Option<Error> = None;
    let mut i = 0;
    while i < ids.len() {
        if failure.is_none() {
            failure = admit_one(user, s, c.id, ids[i], &mut st);
        }
        i += 1;
    }
    match failure {
        Some(e) => Err(e),
        None => {
            let mut ws = st.writes;
            ws.push(Write::PutChannel(Channel {
                id: c.id,
                label: c.label.clone(),
                size: st.size,
            }));
            ws.push(Write::SetRepository(Repository {
                org: r.org,
                label: r.label.clone(),
                created: r.created,
                patches: r.patches,
                channels: r.channels,
                paths: r.paths,
                events: st.seq,
            }));
            Ok((
                ws,
                Reply::Merged {
                    channel: c.id,
                    admitted: st.admitted,
                },
            ))
        }
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing. Repository commands read neither events, bodies nor listings, so
/// the store need not load them (`Frame.lean`).
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Register { name, repo } => register(actor, &s.listings, name, *repo),
        Command::Create { name } => create(actor, &s.repository, name),
        Command::Fork {
            repo,
            source,
            channel,
        } => match authorize(actor, s, repo, WRITE) {
            Some(e) => Err(e),
            None => fork(actor.user, s, source, channel),
        },
        Command::Propose {
            repo,
            hash,
            dependencies,
            paths,
            body,
        } => match authorize(actor, s, repo, WRITE) {
            Some(e) => Err(e),
            None => propose(actor.user, s, hash, dependencies, paths, body),
        },
        Command::Approve { repo, patch } => match authorize(actor, s, repo, REVIEW) {
            Some(e) => Err(e),
            None => approve(actor.user, s, *patch),
        },
        Command::Check {
            repo,
            patch,
            passed,
        } => match authorize(actor, s, repo, REVIEW) {
            Some(e) => Err(e),
            None => check(actor.user, s, *patch, *passed),
        },
        Command::Merge {
            repo,
            channel,
            patches,
        } => match authorize(actor, s, repo, REVIEW) {
            Some(e) => Err(e),
            None => merge(actor.user, s, channel, patches),
        },
        Command::Grant { repo, user, level } => match authorize(actor, s, repo, ADMIN) {
            Some(e) => Err(e),
            None => grant(actor, s, *user, *level),
        },
        Command::Probe
        | Command::Inspect
        | Command::ReadBody { .. }
        | Command::ReadEvents
        | Command::List => Err(Error::ReadOnly),
    }
}

/// `apply` in place, without cloning the snapshot: the local engine's commit.
/// `Apply.lean` proves it computes what `apply` does.
pub fn commit(s: &mut Snapshot, ws: &Vec<Write>) {
    let mut i = 0;
    while i < ws.len() {
        apply_write(s, ws[i].clone());
        i += 1;
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::SetRepository(r) => s.repository = r,
        Write::PutPatch(p) => PatchRow::put(&mut s.patches, p),
        Write::PutDependency(d) => Dependency::put(&mut s.dependencies, d),
        Write::PutApproval(a) => Approval::put(&mut s.approvals, a),
        Write::PutCheck(c) => CheckReport::put(&mut s.checks, c),
        Write::PutChannel(c) => Channel::put(&mut s.channels, c),
        Write::PutMember(m) => Member::put(&mut s.members, m),
        Write::PutPath(k) => PathKind::put(&mut s.paths, k),
        Write::PutEvent(e) => Event::put(&mut s.events, e),
        Write::PutBody(b) => Body::put(&mut s.bodies, b),
        Write::PutListing(l) => Listing::put(&mut s.listings, l),
        Write::PutRole(r) => Role::put(&mut s.roles, r),
    }
}

/// The table writes one write makes; `Storage.lean` proves they store what
/// `apply` computes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::SetRepository(r) => out.push(r.sql_put()),
        Write::PutPatch(p) => out.push(p.sql_put()),
        Write::PutDependency(d) => out.push(d.sql_put()),
        Write::PutApproval(a) => out.push(a.sql_put()),
        Write::PutCheck(c) => out.push(c.sql_put()),
        Write::PutChannel(c) => out.push(c.sql_put()),
        Write::PutMember(m) => out.push(m.sql_put()),
        Write::PutPath(k) => out.push(k.sql_put()),
        Write::PutEvent(e) => out.push(e.sql_put()),
        Write::PutBody(b) => out.push(b.sql_put()),
        Write::PutListing(l) => out.push(l.sql_put()),
        Write::PutRole(r) => out.push(r.sql_put()),
    }
}

#[cfg(test)]
mod tests;
