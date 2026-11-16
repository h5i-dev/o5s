//! Hugging Face Hub-compatible front (`huggingface_hub` works against it unchanged).
//!
//! **An HF repo is a Git repo.** Repo `<ns>/<name>` of type `model|dataset|space` is the Git
//! repository `hf/<type>s/<ns>/<name>`, which `/git/hf/<type>s/<ns>/<name>.git` also serves:
//! - Small files are Git blobs.
//! - Large files are Git LFS pointer blobs whose content is a core object, indexed by sha256 in
//!   the global segment map `git/_lfs` (sha256 → BLAKE3, aux = size).
//! - Commits made through the HF API are real Git commits (trees, commit object, SHA-1 ids),
//!   created with OCC on `git/hf/<type>s/<ns>/<name>/refs/heads/<branch>`.
//!
//! **Authorization** is on that Git namespace (`git/hf/<type>s/<ns>/<name>`): one policy covers
//! the HF API and `git clone`.
//!
//! **Protocol** (what `huggingface_hub` uses):
//! - `POST /api/repos/create`
//! - `GET /api/<type>s/<repo>[/revision/<rev>]` (repo info and siblings)
//! - `GET /api/<type>s/<repo>/tree/<rev>[/<path>]`
//! - `POST /api/<type>s/<repo>/preupload/<rev>`
//! - `POST /<repo>.git/info/lfs/objects/batch`, then signed `PUT`/`POST` `/api/_lfs/...`
//! - `POST /api/<type>s/<repo>/commit/<rev>` (NDJSON)
//! - `HEAD`/`GET /<repo>/resolve/<rev>/<path>` with `Range`, `X-Repo-Commit`, `ETag`, and
//!   `X-Linked-Etag`/`X-Linked-Size` for LFS files
//!
//! The Xet transfer protocol is not implemented yet. With `hf_xet` installed, uploads need
//! `HF_HUB_DISABLE_XET=1`, while downloads work either way (Xet is only used when the server
//! advertises it). See docs/NET_PROGRESS.md.

// Handlers short-circuit with a ready `Response` as the error value.
#![allow(clippy::result_large_err)]

use crate::api::{AppState, Caller, IDLE_TIMEOUT, Unsatisfiable, parse_range, stream_reader};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::Access;
use crate::git::{GitState, Kind, Oid, View, canonical, hex, key_of, oid_of, parse_oid, refs_of};
use crate::segmap::{Key, SegCache, SegMap, Value};
use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use pico2peta_core::{Error, Hash, Name, Observed, ReplaceOutcome, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::Arc;
use tokio::sync::mpsc;

#[cfg(test)]
const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";
/// Files at least this large (or with a NUL byte in their sample) are uploaded via LFS.
const LFS_THRESHOLD: u64 = 64 * 1024;
const MAX_COMMIT_BODY: usize = 256 << 20;
const TOKEN_TTL_SECS: u64 = 3600;
const DEFAULT_GITATTRIBUTES: &str = "*.bin filter=lfs diff=lfs merge=lfs -text\n*.safetensors filter=lfs diff=lfs merge=lfs -text\n*.pt filter=lfs diff=lfs merge=lfs -text\n*.onnx filter=lfs diff=lfs merge=lfs -text\n*.gguf filter=lfs diff=lfs merge=lfs -text\n*.parquet filter=lfs diff=lfs merge=lfs -text\n*.zip filter=lfs diff=lfs merge=lfs -text\n";

pub struct HfState {
    git: Arc<GitState>,
    /// The concrete repository, for named puts (namespace chunking profiles).
    local: Option<pico2peta_repo::LocalRepo>,
    /// sha256 → content (global; GC follows LFS pointers through it).
    lfs: Arc<SegMap>,
    /// sha256(repo namespace, sha256) → content: the objects a repo may reference. Without it,
    /// anyone who learned a sha256 could commit, and then read, another repo's private file.
    acl: Arc<SegMap>,
    /// Xet xorb hash → xorb record (see `xet.rs`).
    pub(crate) xet: Arc<SegMap>,
    /// Xet file hash → file record, and sha256 → the same record (Xet downloads).
    pub(crate) xetfile: Arc<SegMap>,
    pub(crate) xetsha: Arc<SegMap>,
    secret: [u8; 32],
}

impl HfState {
    pub fn new(git: Arc<GitState>, local: Option<pico2peta_repo::LocalRepo>) -> std::result::Result<HfState, String> {
        let map = |n: &str| -> std::result::Result<Arc<SegMap>, String> {
            let n = Name::new(n).map_err(|e| e.to_string())?;
            Ok(Arc::new(SegMap::new(git.repo.clone(), n, Arc::new(SegCache::default()))))
        };
        let (lfs, acl, xet) = (map("git/_lfs")?, map("git/_lfsacl")?, map("git/_xet")?);
        let (xetfile, xetsha) = (map("git/_xetfile")?, map("git/_xetsha")?);
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| format!("no system randomness: {e}"))?;
        Ok(HfState { git, local, lfs, acl, xet, xetfile, xetsha, secret })
    }

    pub(crate) fn git_repo(&self) -> &dyn pico2peta_core::Repository {
        &*self.git.repo
    }

    /// Stores file content for `repo` (with its namespace's encoding profile when local).
    pub(crate) fn put_content(&self, repo: &RepoId, src: &mut dyn Read) -> Result<Hash> {
        match (&self.local, repo.content_name()) {
            (Some(l), Some(n)) => l.put_stream_for(&n, src),
            _ => self.git.repo.put_stream(src),
        }
    }

    fn acl_key(repo: &RepoId, sha: &str) -> Key {
        Sha256::new().chain_update(repo.authz()).chain_update([0u8]).chain_update(sha).finalize().into()
    }

    /// Makes `sha` (content `h`, `size` bytes) referencable from `repo`.
    pub(crate) fn register_lfs(&self, repo: &RepoId, sha: &str, h: Hash, size: u64) -> Result<()> {
        let key = sha_key(sha).ok_or_else(|| Error::Parse(format!("bad sha256 {sha}")))?;
        let v = Value { hash: h, aux: size };
        self.lfs.insert_blocking(vec![(key, v)])?;
        self.acl.insert_blocking(vec![(Self::acl_key(repo, sha), v)])?;
        Ok(())
    }

    /// The content of `sha` if `repo` may reference it (and it still exists).
    fn lfs_for(&self, repo: &RepoId, sha: &str) -> Result<Option<Value>> {
        self.acl.get_live_blocking(&Self::acl_key(repo, sha))
    }

    /// [`Self::lfs_for`] for many objects, with one batched map lookup (input order).
    fn lfs_for_many(&self, repo: &RepoId, shas: &[String]) -> Result<Vec<Option<Value>>> {
        let keys: Vec<Key> = shas.iter().map(|s| Self::acl_key(repo, s)).collect();
        let (_, root) = self.acl.root_blocking()?;
        let mut out = self.acl.get_many(&root, &keys)?;
        for v in &mut out {
            // Same as get_live_blocking: collected content is a miss; a hit is made safe to use.
            if let Some(x) = v
                && !self.git.repo.ensure_live(&x.hash)?
            {
                *v = None;
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Repo identity
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct RepoId {
    /// `model`, `dataset` or `space`.
    kind: &'static str,
    ns: String,
    name: String,
}

impl RepoId {
    fn id(&self) -> String {
        format!("{}/{}", self.ns, self.name)
    }
    /// The Git repository name (served at `/git/<this>.git`).
    fn git_repo(&self) -> String {
        format!("hf/{}s/{}/{}", self.kind, self.ns, self.name)
    }
    /// Authorization namespace (the Git namespace).
    fn authz(&self) -> String {
        format!("git/{}", self.git_repo())
    }
    fn url_prefix(&self) -> &'static str {
        match self.kind {
            "dataset" => "datasets/",
            "space" => "spaces/",
            _ => "",
        }
    }
    fn branch_ref(&self, branch: &str) -> Option<Name> {
        crate::git::ref_name(&self.git_repo(), &format!("refs/heads/{branch}"))
    }
    /// Chunking/encoding name for this repo's content (namespace profiles apply).
    fn content_name(&self) -> Option<Name> {
        Name::new(format!("hf/{}s/{}/{}", self.kind, self.ns, self.name)).ok()
    }
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s != "."
        && s != ".."
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && !s.starts_with('.')
        && !s.ends_with(".git")
}

fn kind_from_plural(s: &str) -> Option<&'static str> {
    Some(match s {
        "models" => "model",
        "datasets" => "dataset",
        "spaces" => "space",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn hf_err(status: StatusCode, code: Option<&'static str>, msg: impl Into<String>) -> Response {
    let mut r = (status, axum::Json(serde_json::json!({ "error": msg.into() }))).into_response();
    if let Some(c) = code {
        r.headers_mut().insert("x-error-code", HeaderValue::from_static(c));
    }
    r
}

fn internal(e: impl std::fmt::Display) -> Response {
    hf_err(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> std::result::Result<T, Response> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(core_err(e)),
        Err(e) => Err(internal(e)),
    }
}

fn core_err(e: Error) -> Response {
    let api = crate::error::ApiError::from(e);
    hf_err(api.status, None, api.message)
}

fn check(st: &AppState, p: &Principal, c: &Caller, repo: &RepoId, need: Access) -> std::result::Result<(), Response> {
    if st.authz.allows(p, &repo.authz(), need) {
        return st.admit(p, &repo.authz(), need >= Access::Write).map_err(IntoResponse::into_response);
    }
    st.metrics.denied.inc();
    st.audit.record_nowait(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "hf",
        target: repo.authz(),
        outcome: "denied".into(),
        detail: None,
        remote: c.remote.clone(),
    });
    // Like the Hub: don't reveal existence to readers without access.
    if need == Access::Read {
        Err(hf_err(StatusCode::NOT_FOUND, Some("RepoNotFound"), format!("repository {} not found", repo.id())))
    } else {
        Err(hf_err(StatusCode::FORBIDDEN, None, format!("{} may not write {}", p.name, repo.id())))
    }
}

// ---------------------------------------------------------------------------
// Git object helpers
// ---------------------------------------------------------------------------

/// A tree entry: mode, name, oid.
type TreeEntry = (String, String, Oid);

fn parse_tree(body: &[u8]) -> Result<Vec<TreeEntry>> {
    let bad = || Error::Corrupt("bad git tree".into());
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let sp = body[i..].iter().position(|&c| c == b' ').ok_or_else(bad)? + i;
        let nul = body[sp..].iter().position(|&c| c == 0).ok_or_else(bad)? + sp;
        if nul + 21 > body.len() {
            return Err(bad());
        }
        let mut o = [0u8; 20];
        o.copy_from_slice(&body[nul + 1..nul + 21]);
        out.push((
            String::from_utf8_lossy(&body[i..sp]).into_owned(),
            String::from_utf8_lossy(&body[sp + 1..nul]).into_owned(),
            Oid::from(o), // HF repos are SHA-1 repositories
        ));
        i = nul + 21;
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum Node {
    File { mode: String, oid: Oid },
    Dir(BTreeMap<String, Node>),
}

fn load_tree(view: &View<'_>, oid: &Oid) -> Result<BTreeMap<String, Node>> {
    let (kind, body) = view.load(oid)?.ok_or_else(|| Error::Corrupt(format!("missing tree {}", hex(oid))))?;
    if kind != Kind::Tree {
        return Err(Error::Corrupt(format!("{} is not a tree", hex(oid))));
    }
    let mut m = BTreeMap::new();
    for (mode, name, o) in parse_tree(&body)? {
        let node = if mode == "40000" { Node::Dir(load_tree(view, &o)?) } else { Node::File { mode, oid: o } };
        m.insert(name, node);
    }
    Ok(m)
}

/// Collects new git objects (canonical bytes stored, oid-map entries pending).
struct Writer<'a> {
    st: &'a GitState,
    entries: Vec<(Key, Value)>,
}

impl Writer<'_> {
    fn put(&mut self, kind: Kind, body: &[u8]) -> Result<Oid> {
        let canon = canonical(kind, body);
        let oid = oid_of(&canon);
        let h = self.st.repo.put(&canon)?;
        self.entries.push((key_of(&oid), Value { hash: h, aux: ((kind as u64) << 56) | body.len() as u64 }));
        Ok(oid)
    }

    fn write_tree(&mut self, m: &BTreeMap<String, Node>) -> Result<Oid> {
        // Git order: byte order of names, with directories compared as "name/".
        let mut items: Vec<(Vec<u8>, String, Oid)> = Vec::with_capacity(m.len());
        for (name, node) in m {
            match node {
                Node::File { mode, oid } => items.push((name.as_bytes().to_vec(), format!("{mode} {name}"), *oid)),
                Node::Dir(sub) => {
                    if sub.is_empty() {
                        continue; // git has no empty directories
                    }
                    let t = self.write_tree(sub)?;
                    let mut k = name.as_bytes().to_vec();
                    k.push(b'/');
                    items.push((k, format!("40000 {name}"), t));
                }
            }
        }
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let mut body = Vec::new();
        for (_, head, oid) in items {
            body.extend_from_slice(head.as_bytes());
            body.push(0);
            body.extend_from_slice(&oid);
        }
        self.put(Kind::Tree, &body)
    }

    fn finish(self) -> Result<()> {
        if !self.entries.is_empty() {
            self.st.oidmap.insert_blocking(self.entries)?;
        }
        Ok(())
    }
}

fn lfs_pointer(sha: &str, size: u64) -> Vec<u8> {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{sha}\nsize {size}\n").into_bytes()
}

/// `(sha256 hex, size)` if `body` is a Git LFS pointer.
pub fn parse_lfs_pointer(body: &[u8]) -> Option<(String, u64)> {
    // The extracted net kernel's parser (`net-kernel/src/parse.rs`): the version line, then
    // `oid sha256:<64 hex>` and `size <strict decimal>` lines, the last of each winning.
    let (oid, size) = pico2peta_net_kernel::parse::parse_lfs_pointer(body)?;
    Some((hex(&oid), size))
}

pub(crate) fn sha_key(hex64: &str) -> Option<Key> {
    crate::hexs::decode32(hex64)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A resolved revision.
struct Resolved {
    commit: Oid,
    tree: Oid,
    /// Branch ref and its value, when the revision is a branch.
    branch: Option<(Name, pico2peta_core::RefValue)>,
}

fn resolve_rev(st: &GitState, view: &View<'_>, repo: &RepoId, rev: &str) -> Result<Option<Resolved>> {
    let (mut commit, branch) = if let Some(o) = parse_oid(rev.as_bytes()) {
        (o, None)
    } else if let Some(t) = rev.strip_prefix("refs/tags/") {
        let Some(o) = tag_target(st, repo, t)? else { return Ok(None) };
        (o, None)
    } else {
        let b = rev.strip_prefix("refs/heads/").unwrap_or(rev);
        match repo.branch_ref(b) {
            Some(n) if let Some(v) = st.repo.resolve(&n)? => (oid_of(&st.repo.get(&v.hash, None)?), Some((n, v))),
            // Not a branch: a tag of that name.
            _ => match tag_target(st, repo, b)? {
                Some(o) => (o, None),
                None => return Ok(None),
            },
        }
    };
    // Peel annotated tags (pushed with git).
    for _ in 0..8 {
        match view.load(&commit)? {
            Some((Kind::Tag, body)) => match refs_of(Kind::Tag, &body)?.first() {
                Some(o) => commit = *o,
                None => return Ok(None),
            },
            _ => break,
        }
    }
    let Some((Kind::Commit, body)) = view.load(&commit)? else { return Ok(None) };
    let tree = *refs_of(Kind::Commit, &body)?.first().ok_or_else(|| Error::Corrupt("commit without tree".into()))?;
    Ok(Some(Resolved { commit, tree, branch }))
}

/// The object a tag points at (unpeeled).
fn tag_target(st: &GitState, repo: &RepoId, tag: &str) -> Result<Option<Oid>> {
    let Some(n) = crate::git::ref_name(&repo.git_repo(), &format!("refs/tags/{tag}")) else { return Ok(None) };
    Ok(match st.repo.resolve(&n)? {
        Some(v) => Some(oid_of(&st.repo.get(&v.hash, None)?)),
        None => None,
    })
}

/// Moves (creates, or deletes with `None`) a git ref of the HF repo. `create_only`: fail if it exists.
fn set_ref(
    st: &GitState,
    view: &View<'_>,
    name: &Name,
    to: Option<Oid>,
    create_only: bool,
) -> Result<std::result::Result<(), &'static str>> {
    let cur = st.repo.resolve(name)?;
    if create_only && cur.is_some() {
        return Ok(Err("exists"));
    }
    let obs = cur.as_ref().map_or(Observed::Absent, |c| Observed::Version(c.version));
    let out = match to {
        Some(oid) => {
            let (_, h) = view.lookup(&oid)?.ok_or_else(|| Error::Corrupt(format!("{} not indexed", hex(&oid))))?;
            st.repo.replace(name, &[(name.clone(), obs)], h)?
        }
        None if cur.is_none() => return Ok(Err("missing")),
        None => st.repo.delete_ref(name, &[(name.clone(), obs)])?,
    };
    Ok(match out {
        ReplaceOutcome::Ok { .. } => Ok(()),
        ReplaceOutcome::Conflict { .. } => Err("conflict"),
    })
}

fn repo_exists(st: &GitState, repo: &RepoId) -> Result<bool> {
    let prefix = format!("git/{}/refs", repo.git_repo());
    Ok(!st.repo.list_refs(&prefix)?.is_empty())
}

/// Flattened file listing of a tree: `(path, mode, blob oid)`.
fn walk_files(
    view: &View<'_>,
    tree: &Oid,
    prefix: &str,
    out: &mut Vec<(String, String, Oid)>,
    dirs: &mut Vec<(String, Oid)>,
) -> Result<()> {
    let (_, body) = view.load(tree)?.ok_or_else(|| Error::Corrupt(format!("missing tree {}", hex(tree))))?;
    for (mode, name, oid) in parse_tree(&body)? {
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if mode == "40000" {
            dirs.push((path.clone(), oid));
            walk_files(view, &oid, &path, out, dirs)?;
        } else if mode != "160000" {
            out.push((path, mode, oid));
        }
    }
    Ok(())
}

/// Finds `path` under `tree`: `(mode, oid)`.
fn find_path(view: &View<'_>, tree: &Oid, path: &str) -> Result<Option<(String, Oid)>> {
    let mut cur = *tree;
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        let Some((Kind::Tree, body)) = view.load(&cur)? else { return Ok(None) };
        let Some((mode, _, oid)) = parse_tree(&body)?.into_iter().find(|e| e.1 == *part) else { return Ok(None) };
        if i + 1 == parts.len() {
            return Ok(Some((mode, oid)));
        }
        if mode != "40000" {
            return Ok(None);
        }
        cur = oid;
    }
    Ok(Some(("40000".into(), cur)))
}

/// Blob metadata for API listings: `(size, lfs (sha, size, pointer size))`.
/// LFS details of a pointer blob: `(sha256, size, pointer size)`.
type LfsInfo = (String, u64, u64);

fn blob_info(view: &View<'_>, oid: &Oid) -> Result<(u64, Option<LfsInfo>)> {
    let (_, body) = view.load(oid)?.ok_or_else(|| Error::Corrupt(format!("missing blob {}", hex(oid))))?;
    Ok(match parse_lfs_pointer(&body) {
        Some((sha, size)) => (size, Some((sha, size, body.len() as u64))),
        None => (body.len() as u64, None),
    })
}

// ---------------------------------------------------------------------------
// Commit creation
// ---------------------------------------------------------------------------

enum Op {
    Add { path: String, content: Vec<u8> },
    AddLfs { path: String, sha: String, size: u64 },
    Delete { path: String },
    DeleteFolder { path: String },
}

fn commit_object(tree: &Oid, parent: Option<&Oid>, who: &str, summary: &str, description: &str) -> Vec<u8> {
    let ts = now_secs();
    let mut s = format!("tree {}\n", hex(tree));
    if let Some(p) = parent {
        s.push_str(&format!("parent {}\n", hex(p)));
    }
    let ident = format!("{who} <{who}@pico2peta> {ts} +0000");
    s.push_str(&format!("author {ident}\ncommitter {ident}\n\n{summary}\n"));
    if !description.is_empty() {
        s.push_str(&format!("\n{description}\n"));
    }
    s.into_bytes()
}

fn insert_path(root: &mut BTreeMap<String, Node>, path: &str, node: Node) -> Result<()> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, dirs)) = parts.split_last() else { return Err(Error::Parse("empty path".into())) };
    let mut cur = root;
    for d in dirs {
        let e = cur.entry((*d).to_string()).or_insert_with(|| Node::Dir(BTreeMap::new()));
        if let Node::File { .. } = e {
            *e = Node::Dir(BTreeMap::new());
        }
        let Node::Dir(m) = e else { unreachable!("just ensured a directory") };
        cur = m;
    }
    cur.insert((*last).to_string(), node);
    Ok(())
}

fn remove_path(root: &mut BTreeMap<String, Node>, path: &str, folder: bool) {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, dirs)) = parts.split_last() else { return };
    let mut cur = root;
    for d in dirs {
        match cur.get_mut(*d) {
            Some(Node::Dir(m)) => cur = m,
            _ => return,
        }
    }
    match cur.get(*last) {
        Some(Node::Dir(_)) if folder => {
            cur.remove(*last);
        }
        Some(Node::File { .. }) if !folder => {
            cur.remove(*last);
        }
        _ => {}
    }
}

/// Fuzzing: tree parsing, and commit path operations on a tree built from the input's lines.
pub(crate) fn fuzz_paths(data: &[u8]) {
    let _ = parse_tree(data);
    let text = String::from_utf8_lossy(data);
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    let oid = Oid::from_slice(&[7u8; 20]).unwrap_or_else(|| unreachable!("20 bytes is an oid"));
    for (i, line) in text.lines().enumerate() {
        if !valid_repo_path(line) {
            continue;
        }
        if i % 3 == 2 {
            remove_path(&mut root, line, i % 2 == 0);
        } else {
            let _ = insert_path(&mut root, line, Node::File { mode: "100644".into(), oid });
        }
    }
}

fn valid_repo_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !p.starts_with('/')
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != ".." && s != ".git" && !s.contains('\0'))
}

/// Applies `ops` on top of branch `rev` with OCC, rebasing on conflict.
#[allow(clippy::too_many_arguments)]
fn make_commit(
    hf: &HfState,
    repo: &RepoId,
    rev: &str,
    who: &str,
    summary: &str,
    description: &str,
    parent: Option<Oid>,
    ops: &[Op],
) -> Result<std::result::Result<Oid, (StatusCode, &'static str, String)>> {
    let st = &*hf.git;
    for _attempt in 0..8 {
        let view = View::new(st)?;
        let Some(res) = resolve_rev(st, &view, repo, rev)? else {
            return Ok(Err((StatusCode::NOT_FOUND, "RevisionNotFound", format!("revision {rev} not found"))));
        };
        let Some((ref_name, ref_val)) = res.branch.clone() else {
            return Ok(Err((StatusCode::BAD_REQUEST, "BadRequest", "commits must target a branch".to_string())));
        };
        let mut tree = load_tree(&view, &res.tree)?;
        let mut w = Writer { st, entries: Vec::new() };
        // Every LFS file of the commit, checked with one batched lookup.
        let lfs_shas: Vec<String> = ops
            .iter()
            .filter_map(|op| match op {
                Op::AddLfs { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect();
        let lfs_have: HashMap<String, Value> = lfs_shas
            .iter()
            .cloned()
            .zip(hf.lfs_for_many(repo, &lfs_shas)?)
            .filter_map(|(s, v)| v.map(|v| (s, v)))
            .collect();
        // The acceptance rule is the extracted kernel's (`net-kernel/src/publish.rs`,
        // `decide_commit`): a given parent must be the head, paths valid (checked while
        // parsing), and every LFS file uploaded with its declared size.
        let missing_lfs = ops.iter().find_map(|op| match op {
            Op::AddLfs { sha, size, .. } if !lfs_have.get(sha).is_some_and(|v| v.aux == *size) => {
                Some((sha.clone(), *size))
            }
            _ => None,
        });
        let parent_bytes = parent.map(|p| p.to_vec());
        match pico2peta_net_kernel::publish::decide_commit(
            &parent_bytes,
            &Some(res.commit.to_vec()),
            true,
            missing_lfs.is_none(),
        ) {
            Err(pico2peta_net_kernel::publish::CommitNg::StaleParent) => {
                return Ok(Err((
                    StatusCode::PRECONDITION_FAILED,
                    "BadRequest",
                    format!("parent commit {} is not the head of {rev}", parent.map(|p| hex(&p)).unwrap_or_default()),
                )));
            }
            Err(_) => {
                let (sha, size) = missing_lfs.unwrap_or_default();
                return Ok(Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "EntryNotFound",
                    format!("LFS object {sha} ({size} B) was not uploaded"),
                )));
            }
            Ok(()) => {}
        }
        for op in ops {
            match op {
                Op::Add { path, content } => {
                    let oid = w.put(Kind::Blob, content)?;
                    insert_path(&mut tree, path, Node::File { mode: "100644".into(), oid })?;
                }
                Op::AddLfs { path, sha, size } => {
                    let oid = w.put(Kind::Blob, &lfs_pointer(sha, *size))?;
                    insert_path(&mut tree, path, Node::File { mode: "100644".into(), oid })?;
                }
                Op::Delete { path } => remove_path(&mut tree, path, false),
                Op::DeleteFolder { path } => remove_path(&mut tree, path, true),
            }
        }
        let t = w.write_tree(&tree)?;
        let c = w.put(Kind::Commit, &commit_object(&t, Some(&res.commit), who, summary, description))?;
        w.finish()?;
        let (_, ch) = View::new(st)?.lookup(&c)?.ok_or_else(|| Error::Corrupt("commit not indexed".into()))?;
        match st.repo.replace(&ref_name, &[(ref_name.clone(), Observed::Version(ref_val.version))], ch)? {
            ReplaceOutcome::Ok { .. } => return Ok(Ok(c)),
            ReplaceOutcome::Conflict { .. } => {
                if parent.is_some() {
                    return Ok(Err((
                        StatusCode::PRECONDITION_FAILED,
                        "BadRequest",
                        "branch moved concurrently".into(),
                    )));
                }
                continue; // rebase onto the new head
            }
        }
    }
    Ok(Err((StatusCode::CONFLICT, "BadRequest", "too much contention on the branch".into())))
}

/// Creates the repo with an initial commit holding `.gitattributes`.
fn create_repo(hf: &HfState, repo: &RepoId, who: &str) -> Result<bool> {
    let st = &*hf.git;
    if repo_exists(st, repo)? {
        return Ok(false);
    }
    let n = repo.branch_ref("main").ok_or_else(|| Error::Parse("bad repo name".into()))?;
    let mut w = Writer { st, entries: Vec::new() };
    let mut tree = BTreeMap::new();
    let ga = w.put(Kind::Blob, DEFAULT_GITATTRIBUTES.as_bytes())?;
    tree.insert(".gitattributes".to_string(), Node::File { mode: "100644".into(), oid: ga });
    let t = w.write_tree(&tree)?;
    let c = w.put(Kind::Commit, &commit_object(&t, None, who, "initial commit", ""))?;
    w.finish()?;
    let (_, ch) = View::new(st)?.lookup(&c)?.ok_or_else(|| Error::Corrupt("commit not indexed".into()))?;
    Ok(matches!(st.repo.replace(&n, &[(n.clone(), Observed::Absent)], ch)?, ReplaceOutcome::Ok { .. }))
}

// ---------------------------------------------------------------------------
// Signed LFS capabilities (the LFS PUT carries no credentials)
// ---------------------------------------------------------------------------

fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(key);
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

fn b64url(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn sign_token(hf: &HfState, op: &str, repo: &RepoId, sha: &str, size: u64) -> String {
    let exp = now_secs() + TOKEN_TTL_SECS;
    let payload = format!("{op}|{}|{}|{}|{sha}|{size}|{exp}", repo.kind, repo.ns, repo.name);
    let sig = hmac_sha256(&hf.secret, payload.as_bytes());
    format!("{}.{}", b64url(payload.as_bytes()), hex(&sig))
}

/// Verifies a capability: `(repo, sha, size)` if valid for `op` and not expired.
fn verify_token(hf: &HfState, op: &str, token: &str) -> Option<(RepoId, String, u64)> {
    use base64::Engine as _;
    let (p, sig) = token.split_once('.')?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()?;
    let want = hex(&hmac_sha256(&hf.secret, &payload));
    // Constant-time comparison.
    if want.len() != sig.len() || want.bytes().zip(sig.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) != 0 {
        return None;
    }
    let s = String::from_utf8(payload).ok()?;
    let f: Vec<&str> = s.split('|').collect();
    if f.len() != 7 || f[0] != op || f[6].parse::<u64>().ok()? < now_secs() {
        return None;
    }
    let kind = match f[1] {
        "model" => "model",
        "dataset" => "dataset",
        "space" => "space",
        _ => return None,
    };
    Some((RepoId { kind, ns: f[2].into(), name: f[3].into() }, f[4].into(), f[5].parse().ok()?))
}

// ---------------------------------------------------------------------------
// HTTP entry points
// ---------------------------------------------------------------------------

fn base_url(headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    let proto = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    format!("{proto}://{host}")
}

/// `/api/{*rest}`.
#[allow(clippy::too_many_arguments)]
pub async fn api(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    method: Method,
    Path(rest): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hf) = st.hf.clone() else { return hf_err(StatusCode::NOT_FOUND, None, "HF front disabled") };
    let parts: Vec<&str> = rest.split('/').collect();
    match (method.clone(), parts.as_slice()) {
        (Method::POST, ["repos", "create"]) => create(&st, &hf, &p, &c, &headers, body).await,
        (Method::GET, ["whoami-v2"]) => axum::Json(serde_json::json!({
            "type": "user", "name": p.name, "fullname": p.name, "auth": {"type": "access_token"}, "orgs": []
        }))
        .into_response(),
        (_, [plural, ns, name, tail @ ..]) => {
            let Some(kind) = kind_from_plural(plural) else {
                return hf_err(StatusCode::NOT_FOUND, None, "unknown API");
            };
            if !valid_segment(ns) || !valid_segment(name) {
                return hf_err(StatusCode::BAD_REQUEST, None, "invalid repo id");
            }
            let repo = RepoId { kind, ns: ns.to_string(), name: name.to_string() };
            let dec = |s: &str| urlencoding_decode(s);
            match (method, tail) {
                (Method::GET, []) => info(&st, &hf, &p, &c, &repo, "main", &headers).await,
                (Method::GET, ["revision", rev @ ..]) => {
                    info(&st, &hf, &p, &c, &repo, &dec(&rev.join("/")), &headers).await
                }
                (Method::GET, ["tree", rev, path @ ..]) => {
                    let recursive = q.get("recursive").is_some_and(|v| v == "true" || v == "1" || v == "True");
                    tree(&st, &hf, &p, &c, &repo, &dec(rev), &dec(&path.join("/")), recursive).await
                }
                (Method::POST, ["preupload", rev]) => preupload(&st, &hf, &p, &c, &repo, &dec(rev), body).await,
                (Method::POST, ["commit", rev]) => commit(&st, &hf, &p, &c, &repo, &dec(rev), &headers, body).await,
                (Method::GET, ["refs"]) => refs_api(&st, &hf, &p, &c, &repo).await,
                (Method::GET, ["commits", rev @ ..]) => {
                    commits_api(&st, &hf, &p, &c, &repo, &dec(&rev.join("/"))).await
                }
                (m @ (Method::POST | Method::DELETE), ["branch", name @ ..]) => {
                    ref_api(&st, &hf, &p, &c, &repo, m, "heads", &dec(&name.join("/")), body).await
                }
                (m @ (Method::POST | Method::DELETE), ["tag", name @ ..]) => {
                    ref_api(&st, &hf, &p, &c, &repo, m, "tags", &dec(&name.join("/")), body).await
                }
                (Method::GET, ["xet-write-token", ..]) => xet_token(&st, &hf, &p, &c, &repo, true, &headers),
                (Method::GET, ["xet-read-token", ..]) => xet_token(&st, &hf, &p, &c, &repo, false, &headers),
                _ => hf_err(StatusCode::NOT_FOUND, None, "unknown API"),
            }
        }
        _ => hf_err(StatusCode::NOT_FOUND, None, "unknown API"),
    }
}

/// `GET /api/<type>s/<repo>/refs`: branches and tags (the git refs).
async fn refs_api(st: &Arc<AppState>, hf: &Arc<HfState>, p: &Principal, c: &Caller, repo: &RepoId) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Read) {
        return r;
    }
    let (hf2, r2) = (hf.clone(), repo.clone());
    let res = blocking(move || {
        let st = &*hf2.git;
        if !repo_exists(st, &r2)? {
            return Ok(None);
        }
        let view = View::new(st)?;
        let (mut branches, mut tags) = (Vec::new(), Vec::new());
        for (gitref, oid, _) in crate::git::list_refs(st, &r2.git_repo())? {
            // Tags point at commits for HF clients: peel annotated ones.
            let mut target = oid;
            if let Some((Kind::Tag, body)) = view.load(&oid)?
                && let Some(t) = refs_of(Kind::Tag, &body)?.first()
            {
                target = *t;
            }
            let e = |name: &str| serde_json::json!({ "name": name, "ref": gitref, "targetCommit": hex(&target) });
            if let Some(b) = gitref.strip_prefix("refs/heads/") {
                branches.push(e(b));
            } else if let Some(t) = gitref.strip_prefix("refs/tags/") {
                tags.push(e(t));
            }
        }
        Ok(Some(serde_json::json!({ "branches": branches, "tags": tags, "converts": [], "pullRequests": [] })))
    })
    .await;
    match res {
        Ok(Some(v)) => axum::Json(v).into_response(),
        Ok(None) => hf_err(StatusCode::NOT_FOUND, Some("RepoNotFound"), format!("repository {} not found", repo.id())),
        Err(r) => r,
    }
}

/// `POST|DELETE /api/<type>s/<repo>/branch/<name>` and `.../tag/<rev>` (create: body `{tag}`) /
/// `.../tag/<tag>` (delete). Lightweight tags; branches start at `startingPoint` or `main`.
#[allow(clippy::too_many_arguments)]
async fn ref_api(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    method: Method,
    kind: &'static str,
    name: &str,
    body: Body,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Write) {
        return r;
    }
    let raw = axum::body::to_bytes(body, 1 << 16).await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    // For tag creation the path names the revision and the body names the tag.
    let (refname, start) = match (kind, &method) {
        ("tags", &Method::POST) => (v["tag"].as_str().unwrap_or_default().to_string(), name.to_string()),
        ("heads", &Method::POST) => (name.to_string(), v["startingPoint"].as_str().unwrap_or("main").to_string()),
        _ => (name.to_string(), String::new()),
    };
    if kind == "heads" && refname == "main" && method == Method::DELETE {
        return hf_err(StatusCode::BAD_REQUEST, None, "the main branch cannot be deleted");
    }
    let Some(n) = crate::git::ref_name(&repo.git_repo(), &format!("refs/{kind}/{refname}")) else {
        return hf_err(
            StatusCode::BAD_REQUEST,
            None,
            format!("invalid {} name {refname:?}", if kind == "heads" { "branch" } else { "tag" }),
        );
    };
    let (hf2, r2, create, start2) = (hf.clone(), repo.clone(), method == Method::POST, start.clone());
    let res = blocking(move || {
        let st = &*hf2.git;
        let view = View::new(st)?;
        let to = if create {
            match resolve_rev(st, &view, &r2, &start2)? {
                Some(res) => Some(res.commit),
                None => return Ok(Err("revision")),
            }
        } else {
            None
        };
        set_ref(st, &view, &n, to, create)
    })
    .await;
    let what = if kind == "heads" { "branch" } else { "tag" };
    match res {
        Ok(Ok(())) => {
            audit(st, p, c, repo, "hf_ref", "ok", Some(format!("{method} refs/{kind}/{refname}"))).await;
            axum::Json(serde_json::json!({ "ok": true })).into_response()
        }
        Ok(Err("exists")) => hf_err(StatusCode::CONFLICT, None, format!("{what} {refname} already exists")),
        Ok(Err("missing")) => {
            hf_err(StatusCode::NOT_FOUND, Some("RevisionNotFound"), format!("{what} {refname} not found"))
        }
        Ok(Err("revision")) => {
            hf_err(StatusCode::NOT_FOUND, Some("RevisionNotFound"), format!("revision {start} not found"))
        }
        Ok(Err(_)) => hf_err(StatusCode::CONFLICT, None, format!("{what} {refname} moved concurrently")),
        Err(r) => r,
    }
}

/// `GET /api/<type>s/<repo>/commits/<rev>`: history (newest first, up to 1000 commits).
async fn commits_api(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Read) {
        return r;
    }
    let (hf2, r2, rev2) = (hf.clone(), repo.clone(), rev.to_string());
    let res = blocking(move || {
        let st = &*hf2.git;
        let view = View::new(st)?;
        let Some(res) = resolve_rev(st, &view, &r2, &rev2)? else { return Ok(None) };
        // Walk by commit time, newest first.
        let mut heap = std::collections::BinaryHeap::new();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let load = |o: &Oid| -> Result<Option<(u64, Vec<u8>)>> {
            Ok(match view.load(o)? {
                Some((Kind::Commit, body)) => Some((commit_time(&body), body)),
                _ => None,
            })
        };
        if let Some((t, b)) = load(&res.commit)? {
            heap.push((t, res.commit, b));
            seen.insert(res.commit);
        }
        while let Some((t, oid, body)) = heap.pop() {
            if out.len() >= 1000 {
                break;
            }
            let text = String::from_utf8_lossy(&body);
            let (head, msg) = text.split_once("\n\n").unwrap_or((&text, ""));
            let author = head
                .lines()
                .find_map(|l| l.strip_prefix("author "))
                .and_then(|a| a.split(" <").next())
                .unwrap_or("")
                .to_string();
            let (title, rest) = msg.split_once('\n').unwrap_or((msg, ""));
            out.push(serde_json::json!({
                "id": hex(&oid), "title": title.trim(), "message": rest.trim(),
                "authors": [{ "user": author }], "date": crate::audit::rfc3339(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t)),
            }));
            for parent in refs_of(Kind::Commit, &body)?.into_iter().skip(1) {
                if seen.insert(parent)
                    && let Some((pt, pb)) = load(&parent)?
                {
                    heap.push((pt, parent, pb));
                }
            }
        }
        Ok(Some(out))
    })
    .await;
    match res {
        Ok(Some(v)) => axum::Json(v).into_response(),
        Ok(None) => hf_err(StatusCode::NOT_FOUND, Some("RevisionNotFound"), format!("revision {rev} not found")),
        Err(r) => r,
    }
}

/// Committer time of a commit body (seconds).
fn commit_time(body: &[u8]) -> u64 {
    String::from_utf8_lossy(body)
        .lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.strip_prefix("committer ").map(str::to_string))
        .and_then(|l| l.rsplit(' ').nth(1).and_then(|t| t.parse().ok()))
        .unwrap_or(0)
}

/// `{casUrl, accessToken, exp}` for `hf_xet`: an HMAC capability scoped to this repo.
fn xet_token(
    st: &AppState,
    hf: &HfState,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    write: bool,
    headers: &HeaderMap,
) -> Response {
    if let Err(r) = check(st, p, c, repo, if write { Access::Write } else { Access::Read }) {
        return r;
    }
    let tok = sign_token(hf, if write { "xet-w" } else { "xet-r" }, repo, "-", 0);
    let (cas, exp) = (format!("{}/xet", base_url(headers)), now_secs() + TOKEN_TTL_SECS);
    // hf_xet reads the JSON body; huggingface_hub's Python side reads the headers.
    let mut r = axum::Json(serde_json::json!({ "casUrl": cas, "accessToken": tok, "exp": exp })).into_response();
    for (k, v) in [("x-xet-cas-url", cas), ("x-xet-access-token", tok), ("x-xet-token-expiration", exp.to_string())] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            r.headers_mut().insert(k, v);
        }
    }
    r
}

/// The Xet file record for LFS object `sha` if `repo` may read it.
fn xet_file_for(hf: &HfState, repo: &RepoId, sha: &str) -> Result<Option<crate::xet::FileRecord>> {
    let Some(key) = sha_key(sha) else { return Ok(None) };
    if hf.lfs_for(repo, sha)?.is_none() {
        return Ok(None);
    }
    let Some(v) = hf.xetsha.get_live_blocking(&key)? else { return Ok(None) };
    Ok(crate::xet::FileRecord::decode(&hf.git_repo().get(&v.hash, None)?))
}

/// `GET <cas>/v1/reconstructions/<file>`: how to rebuild a Xet-uploaded file from its virtual xorbs.
async fn xet_reconstruction(hf: &Arc<HfState>, repo: RepoId, file: &str, headers: &HeaderMap) -> Response {
    let Some(fh) = crate::xet::from_hex(file) else { return hf_err(StatusCode::BAD_REQUEST, None, "bad file id") };
    let (hf2, r2) = (hf.clone(), repo.clone());
    let rec = blocking(move || {
        let Some(v) = hf2.xetfile.get_live_blocking(&fh)? else { return Ok(None) };
        let Some(rec) = crate::xet::FileRecord::decode(&hf2.git_repo().get(&v.hash, None)?) else { return Ok(None) };
        // The token's repo must be allowed to read this content.
        Ok(hf2.lfs_for(&r2, &hex(&rec.sha256))?.map(|_| rec))
    })
    .await;
    let rec = match rec {
        Ok(Some(r)) => r,
        Ok(None) => return hf_err(StatusCode::NOT_FOUND, None, "file not found"),
        Err(r) => return r,
    };
    let size = rec.chunks.last().map_or(0, |c| c.1);
    let (from, to) =
        match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes=")) {
            Some(r) => {
                let (a, b) = r.split_once('-').unwrap_or((r, ""));
                let a: u64 = a.parse().unwrap_or(0);
                let b: u64 = b.parse::<u64>().map_or(size, |e| (e + 1).min(size));
                if a >= size {
                    return hf_err(StatusCode::RANGE_NOT_SATISFIABLE, None, "range starts past the end");
                }
                (a, b)
            }
            None => (0, size),
        };
    let base = base_url(headers);
    let url = |gi: usize| format!("{base}/xet/xorb/{}", sign_token(hf, "xorb", &repo, file, gi as u64));
    axum::Json(rec.reconstruction(from, to, &url)).into_response()
}

/// `GET <cas>/xorb/<token>` with `Range`: bytes of a virtual xorb (the token is the capability).
async fn xet_xorb(hf: &Arc<HfState>, token: &str, headers: &HeaderMap) -> Response {
    let Some((repo, file, gi)) = verify_token(hf, "xorb", token) else {
        return hf_err(StatusCode::FORBIDDEN, None, "invalid or expired xorb URL");
    };
    let Some(fh) = crate::xet::from_hex(&file) else { return hf_err(StatusCode::BAD_REQUEST, None, "bad file id") };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .map(str::to_string);
    let (hf2, partial) = (hf.clone(), range.is_some());
    let res = blocking(move || {
        let Some(v) = hf2.xetfile.get_live_blocking(&fh)? else { return Ok(None) };
        let Some(rec) = crate::xet::FileRecord::decode(&hf2.git_repo().get(&v.hash, None)?) else { return Ok(None) };
        if hf2.lfs_for(&repo, &hex(&rec.sha256))?.is_none() {
            return Ok(None);
        }
        let rec = Arc::new(rec);
        let groups = rec.groups();
        let Some(g) = groups.get(gi as usize) else { return Ok(None) };
        let total: u64 = g.clone().map(|i| 8 + rec.chunks[i].1 - if i == 0 { 0 } else { rec.chunks[i - 1].1 }).sum();
        let (bs, be) = match &range {
            Some(r) => {
                let (a, b) = r.split_once('-').unwrap_or((r, ""));
                (a.parse::<u64>().unwrap_or(0), b.parse::<u64>().map_or(total, |e| (e + 1).min(total)))
            }
            None => (0, total),
        };
        Ok(Some((rec, bs, be, total)))
    })
    .await;
    match res {
        Ok(Some((rec, bs, be, total))) => {
            // Streamed in windows, so serializing overlaps sending (a virtual xorb is ~64 MiB).
            const WINDOW: u64 = 4 << 20;
            let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(2);
            let hf3 = hf.clone();
            tokio::task::spawn_blocking(move || {
                let mut at = bs;
                while at < be {
                    let end = (at + WINDOW).min(be);
                    let item = rec
                        .serialize_range(hf3.git_repo(), gi as usize, at, end)
                        .map(Bytes::from)
                        .map_err(|e| std::io::Error::other(e.to_string()));
                    let stop = item.is_err();
                    if tx.blocking_send(item).is_err() || stop {
                        return;
                    }
                    at = end;
                }
            });
            let body = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|i| (i, rx))
            }));
            let mut r = Response::new(body);
            r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(be.saturating_sub(bs)));
            if partial {
                *r.status_mut() = StatusCode::PARTIAL_CONTENT;
                if let Ok(v) = HeaderValue::from_str(&format!("bytes {bs}-{}/{total}", be.saturating_sub(1))) {
                    r.headers_mut().insert(header::CONTENT_RANGE, v);
                }
            }
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            r
        }
        Ok(None) => hf_err(StatusCode::NOT_FOUND, None, "xorb not found"),
        Err(r) => r,
    }
}

/// `/xet/{*rest}`: the Xet CAS API, authorized by the bearer capability from [`xet_token`].
pub async fn xet_cas(
    State(st): State<Arc<AppState>>,
    method: Method,
    Path(rest): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hf) = st.hf.clone() else { return hf_err(StatusCode::NOT_FOUND, None, "HF front disabled") };
    // Xorb fetch URLs are capabilities (like presigned URLs): no bearer token.
    if method == Method::GET
        && let Some(token) = rest.strip_prefix("xorb/")
    {
        return xet_xorb(&hf, token, &headers).await;
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let write = verify_token(&hf, "xet-w", bearer);
    let Some((repo, _, _)) = write.clone().or_else(|| verify_token(&hf, "xet-r", bearer)) else {
        return hf_err(StatusCode::UNAUTHORIZED, None, "invalid or expired xet token");
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let need_write = || hf_err(StatusCode::FORBIDDEN, None, "read-only xet token");
    match (method, parts.as_slice()) {
        (Method::POST, ["v1", "xorbs", "default", h]) => {
            if write.is_none() {
                return need_write();
            }
            let Some(xh) = crate::xet::from_hex(h) else {
                return hf_err(StatusCode::BAD_REQUEST, None, "bad xorb hash");
            };
            let Ok(raw) = axum::body::to_bytes(body, crate::xet::MAX_XORB_BODY).await else {
                return hf_err(StatusCode::PAYLOAD_TOO_LARGE, None, "xorb too large");
            };
            st.metrics.bytes_in.inc_by(raw.len() as u64);
            let hf2 = hf.clone();
            match blocking(move || crate::xet::put_xorb(&hf2, &repo, &xh, &raw)).await {
                Ok(new) => axum::Json(serde_json::json!({ "was_inserted": new })).into_response(),
                Err(r) => r,
            }
        }
        (Method::POST, ["v1", "shards"]) => {
            if write.is_none() {
                return need_write();
            }
            let Ok(raw) = axum::body::to_bytes(body, crate::xet::MAX_SHARD_BODY).await else {
                return hf_err(StatusCode::PAYLOAD_TOO_LARGE, None, "shard too large");
            };
            let hf2 = hf.clone();
            let r2 = repo.clone();
            match tokio::task::spawn_blocking(move || crate::xet::put_shard(&hf2, &r2, &raw)).await {
                Ok(Ok(_)) => axum::Json(serde_json::json!({ "result": 1 })).into_response(),
                Ok(Err(e)) => {
                    tracing::warn!(repo = %repo.authz(), error = %e, "xet shard rejected");
                    core_err(e)
                }
                Err(e) => internal(e),
            }
        }
        // No global dedup index: "not tracked" makes the client upload what it lacks locally.
        (Method::GET, ["v1", "reconstructions", file]) => xet_reconstruction(&hf, repo, file, &headers).await,
        // V2 (multi-range URLs) is optional: 404 makes clients use V1.
        (Method::GET, ["v2", "reconstructions", _]) => hf_err(StatusCode::NOT_FOUND, None, "use v1"),
        (Method::GET, ["v1", "chunks", ..]) => StatusCode::NOT_FOUND.into_response(),
        (Method::POST, ["v1", "telemetry"]) => StatusCode::OK.into_response(),
        (m, _) => {
            tracing::warn!(method = %m, path = %rest, "unsupported xet endpoint");
            hf_err(StatusCode::NOT_FOUND, None, "unsupported xet endpoint")
        }
    }
}

/// `/api/_lfs/<op>/<token>`: HMAC-signed capability URLs handed out by the LFS batch API.
/// They are routed outside authentication (the LFS transfer sends no credentials); the token
/// itself names the repo, object and expiry.
pub async fn lfs_capability(
    State(st): State<Arc<AppState>>,
    method: Method,
    Path((op, token)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hf) = st.hf.clone() else { return hf_err(StatusCode::NOT_FOUND, None, "HF front disabled") };
    match (method, op.as_str()) {
        (Method::PUT, "upload") => lfs_upload(&st, &hf, &token, body).await,
        (Method::POST, "verify") => lfs_verify(&hf, &token).await,
        (Method::GET | Method::HEAD, "download") => lfs_download(&st, &hf, &token, &headers).await,
        _ => hf_err(StatusCode::METHOD_NOT_ALLOWED, None, "unsupported LFS operation"),
    }
}

/// Everything not matched by other routes: `resolve` downloads and the LFS batch API.
pub async fn root(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hf) = st.hf.clone() else { return hf_err(StatusCode::NOT_FOUND, None, "not found") };
    let path = uri.path().trim_start_matches('/');
    let (kind, rest) = if let Some(r) = path.strip_prefix("datasets/") {
        ("dataset", r)
    } else if let Some(r) = path.strip_prefix("spaces/") {
        ("space", r)
    } else {
        ("model", path)
    };
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    // <ns>/<name>.git/info/lfs/objects/batch
    if method == Method::POST
        && let Some((repo_part, "info/lfs/objects/batch")) = rest.split_once(".git/")
        && let Some((ns, name)) = repo_part.split_once('/')
        && valid_segment(ns)
        && valid_segment(name)
    {
        let repo = RepoId { kind, ns: ns.into(), name: name.into() };
        return lfs_batch(&st, &hf, &p, &c, &repo, &headers, body).await;
    }
    // <ns>/<name>/resolve/<rev>/<path>
    if let [ns, name, "resolve", tail] = parts.as_slice()
        && (method == Method::GET || method == Method::HEAD)
        && valid_segment(ns)
        && valid_segment(name)
        && let Some((rev, file)) = tail.split_once('/')
    {
        let repo = RepoId { kind, ns: (*ns).into(), name: (*name).into() };
        return resolve(
            &st,
            &hf,
            &p,
            &c,
            &repo,
            &urlencoding_decode(rev),
            &urlencoding_decode(file),
            &headers,
            method == Method::HEAD,
        )
        .await;
    }
    hf_err(StatusCode::NOT_FOUND, None, "not found")
}

fn urlencoding_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = {
                let mut o = [0u8; 1];
                crate::hexs::decode_into(&b[i + 1..i + 3], &mut o).map(|_| o[0])
            }
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn create(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Ok(raw) = axum::body::to_bytes(body, 1 << 20).await else {
        return hf_err(StatusCode::BAD_REQUEST, None, "body too large");
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return hf_err(StatusCode::BAD_REQUEST, None, "bad JSON");
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let ns = v.get("organization").and_then(|x| x.as_str()).map(str::to_string).unwrap_or_else(|| p.name.clone());
    let kind = match v.get("type").and_then(|x| x.as_str()).unwrap_or("model") {
        "dataset" => "dataset",
        "space" => "space",
        _ => "model",
    };
    if !valid_segment(&ns) || !valid_segment(&name) {
        return hf_err(StatusCode::BAD_REQUEST, None, "invalid repo id");
    }
    let repo = RepoId { kind, ns, name };
    if let Err(r) = check(st, p, c, &repo, Access::Write) {
        return r;
    }
    let (hf2, r2, who) = (hf.clone(), repo.clone(), p.name.clone());
    let created = match blocking(move || create_repo(&hf2, &r2, &who)).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let url = format!("{}/{}{}", base_url(headers), repo.url_prefix(), repo.id());
    if !created {
        return hf_err(StatusCode::CONFLICT, None, format!("You already created this {} repo", repo.kind));
    }
    audit(st, p, c, &repo, "hf_create", "ok", None).await;
    axum::Json(serde_json::json!({ "url": url, "name": repo.id() })).into_response()
}

async fn audit(
    st: &AppState,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    action: &'static str,
    outcome: &str,
    detail: Option<String>,
) {
    let _ = st
        .audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action,
            target: repo.authz(),
            outcome: outcome.into(),
            detail,
            remote: c.remote.clone(),
        })
        .await;
}

fn iso_now() -> String {
    crate::audit::rfc3339_now()
}

async fn info(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
    _h: &HeaderMap,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Read) {
        return r;
    }
    let (hf2, r2, rev2) = (hf.clone(), repo.clone(), rev.to_string());
    let res = blocking(move || {
        let st = &*hf2.git;
        if !repo_exists(st, &r2)? {
            return Ok(Err("RepoNotFound"));
        }
        let view = View::new(st)?;
        let Some(res) = resolve_rev(st, &view, &r2, &rev2)? else { return Ok(Err("RevisionNotFound")) };
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        walk_files(&view, &res.tree, "", &mut files, &mut dirs)?;
        let mut siblings = Vec::new();
        for (path, _, oid) in &files {
            let (size, lfs) = blob_info(&view, oid)?;
            let mut s = serde_json::json!({ "rfilename": path, "size": size, "blobId": hex(oid) });
            if let Some((sha, size, ptr)) = lfs {
                s["lfs"] = serde_json::json!({ "sha256": sha, "size": size, "pointerSize": ptr });
            }
            siblings.push(s);
        }
        Ok(Ok((hex(&res.commit), siblings)))
    })
    .await;
    let (sha, siblings) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(code)) => return hf_err(StatusCode::NOT_FOUND, Some(code), format!("{} {rev} not found", repo.id())),
        Err(r) => return r,
    };
    let now = iso_now();
    axum::Json(serde_json::json!({
        "_id": sha, "id": repo.id(), "modelId": repo.id(), "author": repo.ns, "sha": sha,
        "lastModified": now, "createdAt": now, "private": false, "disabled": false, "gated": false,
        "downloads": 0, "likes": 0, "tags": [], "siblings": siblings, "cardData": null,
    }))
    .into_response()
}

#[allow(clippy::too_many_arguments)]
async fn tree(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
    path: &str,
    recursive: bool,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Read) {
        return r;
    }
    let (hf2, r2, rev2, path2) = (hf.clone(), repo.clone(), rev.to_string(), path.to_string());
    let res = blocking(move || {
        let st = &*hf2.git;
        let view = View::new(st)?;
        let Some(res) = resolve_rev(st, &view, &r2, &rev2)? else { return Ok(Err("RevisionNotFound")) };
        let start = if path2.is_empty() {
            res.tree
        } else {
            match find_path(&view, &res.tree, &path2)? {
                Some((m, o)) if m == "40000" => o,
                _ => return Ok(Err("EntryNotFound")),
            }
        };
        let mut out = Vec::new();
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        if recursive {
            walk_files(&view, &start, &path2, &mut files, &mut dirs)?;
        } else {
            let (_, body) = view.load(&start)?.ok_or_else(|| Error::Corrupt("missing tree".into()))?;
            for (mode, name, oid) in parse_tree(&body)? {
                let full = if path2.is_empty() { name } else { format!("{path2}/{name}") };
                if mode == "40000" {
                    dirs.push((full, oid));
                } else if mode != "160000" {
                    files.push((full, mode, oid));
                }
            }
        }
        for (path, oid) in dirs {
            out.push(serde_json::json!({ "type": "directory", "oid": hex(&oid), "size": 0, "path": path }));
        }
        for (path, _, oid) in files {
            let (size, lfs) = blob_info(&view, &oid)?;
            let mut e = serde_json::json!({ "type": "file", "oid": hex(&oid), "size": size, "path": path });
            if let Some((sha, size, ptr)) = lfs {
                e["lfs"] = serde_json::json!({ "oid": sha, "size": size, "pointerSize": ptr });
            }
            out.push(e);
        }
        Ok(Ok(out))
    })
    .await;
    match res {
        Ok(Ok(v)) => axum::Json(v).into_response(),
        Ok(Err(code)) => hf_err(StatusCode::NOT_FOUND, Some(code), format!("{rev}/{path} not found")),
        Err(r) => r,
    }
}

async fn preupload(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
    body: Body,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Write) {
        return r;
    }
    let Ok(raw) = axum::body::to_bytes(body, 16 << 20).await else {
        return hf_err(StatusCode::BAD_REQUEST, None, "body too large");
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return hf_err(StatusCode::BAD_REQUEST, None, "bad JSON");
    };
    let (hf2, r2, rev2) = (hf.clone(), repo.clone(), rev.to_string());
    if let Err(r) = blocking(move || {
        let view = View::new(&hf2.git)?;
        Ok(resolve_rev(&hf2.git, &view, &r2, &rev2)?.is_some())
    })
    .await
    .and_then(|ok| {
        if ok { Ok(()) } else { Err(hf_err(StatusCode::NOT_FOUND, Some("RevisionNotFound"), "revision not found")) }
    }) {
        return r;
    }
    let mut files = Vec::new();
    for f in v.get("files").and_then(|f| f.as_array()).into_iter().flatten() {
        let path = f.get("path").and_then(|x| x.as_str()).unwrap_or_default();
        let size = f.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
        let sample = f
            .get("sample")
            .and_then(|x| x.as_str())
            .and_then(|s| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.decode(s).ok()
            })
            .unwrap_or_default();
        let lfs = size >= LFS_THRESHOLD || sample.contains(&0);
        files.push(serde_json::json!({ "path": path, "uploadMode": if lfs { "lfs" } else { "regular" }, "shouldIgnore": false }));
    }
    axum::Json(serde_json::json!({ "files": files })).into_response()
}

async fn lfs_batch(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Ok(raw) = axum::body::to_bytes(body, 16 << 20).await else {
        return hf_err(StatusCode::BAD_REQUEST, None, "body too large");
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return hf_err(StatusCode::BAD_REQUEST, None, "bad JSON");
    };
    let op = v.get("operation").and_then(|x| x.as_str()).unwrap_or("download").to_string();
    let need = if op == "upload" { Access::Write } else { Access::Read };
    if let Err(r) = check(st, p, c, repo, need) {
        return r;
    }
    let base = base_url(headers);
    let reqs: Vec<(String, u64)> = v
        .get("objects")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
        .map(|o| {
            let sha = o.get("oid").and_then(|x| x.as_str()).unwrap_or_default().to_ascii_lowercase();
            (sha, o.get("size").and_then(|x| x.as_u64()).unwrap_or(0))
        })
        .collect();
    // One batched lookup for every valid oid of the request.
    let valid: Vec<String> = reqs.iter().filter(|(s, _)| sha_key(s).is_some()).map(|(s, _)| s.clone()).collect();
    let (hf2, r2) = (hf.clone(), repo.clone());
    let found: HashMap<String, Value> = match blocking(move || {
        let got = hf2.lfs_for_many(&r2, &valid)?;
        Ok(valid.into_iter().zip(got).filter_map(|(s, v)| v.map(|v| (s, v))).collect())
    })
    .await
    {
        Ok(m) => m,
        Err(r) => return r,
    };
    let mut out = Vec::new();
    for (sha, size) in reqs {
        if sha_key(&sha).is_none() {
            out.push(serde_json::json!({ "oid": sha, "size": size, "error": { "code": 422, "message": "bad oid" } }));
            continue;
        }
        let have = found.get(&sha).is_some_and(|v| v.aux == size);
        let mut e = serde_json::json!({ "oid": sha, "size": size, "authenticated": true });
        if op == "upload" {
            if !have {
                e["actions"] = serde_json::json!({
                    "upload": { "href": format!("{base}/api/_lfs/upload/{}", sign_token(hf, "up", repo, &sha, size)), "expires_in": TOKEN_TTL_SECS },
                    "verify": { "href": format!("{base}/api/_lfs/verify/{}", sign_token(hf, "verify", repo, &sha, size)), "expires_in": TOKEN_TTL_SECS },
                });
            }
        } else if have {
            e["actions"] = serde_json::json!({
                "download": { "href": format!("{base}/api/_lfs/download/{}", sign_token(hf, "down", repo, &sha, size)), "expires_in": TOKEN_TTL_SECS },
            });
        } else {
            e["error"] = serde_json::json!({ "code": 404, "message": "object not found" });
        }
        out.push(e);
    }
    let mut r =
        axum::Json(serde_json::json!({ "transfer": "basic", "objects": out, "hash_algo": "sha256" })).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/vnd.git-lfs+json"));
    r
}

pub(crate) struct ChanRead {
    pub(crate) rx: mpsc::Receiver<std::io::Result<Bytes>>,
    pub(crate) cur: Bytes,
}

impl Read for ChanRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.cur = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.cur.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        let _ = self.cur.split_to(n);
        Ok(n)
    }
}

async fn lfs_upload(st: &Arc<AppState>, hf: &Arc<HfState>, token: &str, body: Body) -> Response {
    let Some((repo, sha, size)) = verify_token(hf, "up", token) else {
        return hf_err(StatusCode::FORBIDDEN, None, "invalid or expired upload token");
    };
    if size > st.max_upload_bytes {
        return hf_err(StatusCode::PAYLOAD_TOO_LARGE, None, "object too large");
    }
    let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(8);
    // SHA-256 runs on its own thread, fed the same (refcounted) buffers as the store.
    let (stx, mut srx) = mpsc::channel::<Bytes>(64);
    let hasher = tokio::task::spawn_blocking(move || {
        let mut sha = Sha256::new();
        let mut n = 0u64;
        while let Some(b) = srx.blocking_recv() {
            sha.update(&b);
            n += b.len() as u64;
        }
        (hex(&sha.finalize()), n)
    });
    let metrics = st.metrics.clone();
    let pump = tokio::spawn(async move {
        let mut s = body.into_data_stream();
        loop {
            let item = match tokio::time::timeout(IDLE_TIMEOUT, s.next()).await {
                Err(_) => Some(Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "upload idle"))),
                Ok(None) => None,
                Ok(Some(Ok(b))) => {
                    metrics.bytes_in.inc_by(b.len() as u64);
                    if stx.send(b.clone()).await.is_err() {
                        return;
                    }
                    Some(Ok(b))
                }
                Ok(Some(Err(e))) => Some(Err(std::io::Error::other(e.to_string()))),
            };
            match item {
                None => return,
                Some(it) => {
                    let stop = it.is_err();
                    if tx.send(it).await.is_err() || stop {
                        return;
                    }
                }
            }
        }
    });
    let (hf2, r2) = (hf.clone(), repo.clone());
    let res = tokio::task::spawn_blocking(move || -> Result<Hash> {
        let mut r = ChanRead { rx, cur: Bytes::new() };
        hf2.put_content(&r2, &mut r)
    })
    .await;
    // The pump has ended (the store read to EOF) or failed; either way the hasher finishes.
    if !pump.is_finished() {
        pump.abort();
    }
    let h = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return core_err(e),
        Err(e) => return internal(e),
    };
    let (got, n) = match hasher.await {
        Ok(x) => x,
        Err(e) => return internal(e),
    };
    if got != sha || n != size {
        return hf_err(
            StatusCode::BAD_REQUEST,
            None,
            format!("content is sha256 {got} ({n} B), expected {sha} ({size} B)"),
        );
    }
    let hf2 = hf.clone();
    if let Err(r) = blocking(move || hf2.register_lfs(&repo, &sha, h, size)).await {
        return r;
    }
    StatusCode::OK.into_response()
}

async fn lfs_verify(hf: &Arc<HfState>, token: &str) -> Response {
    let Some((repo, sha, size)) = verify_token(hf, "verify", token) else {
        return hf_err(StatusCode::FORBIDDEN, None, "invalid or expired verify token");
    };
    let hf2 = hf.clone();
    match blocking(move || hf2.lfs_for(&repo, &sha)).await {
        Ok(Some(v)) if v.aux == size => StatusCode::OK.into_response(),
        Ok(_) => hf_err(StatusCode::NOT_FOUND, None, "object not uploaded"),
        Err(r) => r,
    }
}

async fn lfs_download(st: &Arc<AppState>, hf: &Arc<HfState>, token: &str, headers: &HeaderMap) -> Response {
    let Some((repo, sha, _)) = verify_token(hf, "down", token) else {
        return hf_err(StatusCode::FORBIDDEN, None, "invalid or expired download token");
    };
    let hf2 = hf.clone();
    match blocking(move || hf2.lfs_for(&repo, &sha)).await {
        Ok(Some(v)) => serve_object(st, v.hash, headers, false, |_| {}).await,
        Ok(None) => hf_err(StatusCode::NOT_FOUND, None, "object not found"),
        Err(r) => r,
    }
}

/// Streams core object `h` with Range support; `decorate` adds headers.
pub(crate) async fn serve_object(
    st: &Arc<AppState>,
    h: Hash,
    headers: &HeaderMap,
    head: bool,
    decorate: impl FnOnce(&mut HeaderMap),
) -> Response {
    let r = st.repo.clone();
    let size = match tokio::task::spawn_blocking(move || r.size(&h)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return core_err(e),
        Err(e) => return internal(e),
    };
    let range = match parse_range(headers.get(header::RANGE), size) {
        Ok(r) => r,
        Err(Unsatisfiable) => {
            let mut r = hf_err(StatusCode::RANGE_NOT_SATISFIABLE, None, "unsatisfiable range");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let len = range.as_ref().map_or(size, |r| r.end - r.start);
    let mut resp = if head {
        Response::new(Body::empty())
    } else {
        let r = st.repo.clone();
        let rr = range.clone();
        let reader = match tokio::task::spawn_blocking(move || r.open(&h, rr)).await {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => return core_err(e),
            Err(e) => return internal(e),
        };
        Response::new(stream_reader(reader, len, st.metrics.clone()))
    };
    let hm = resp.headers_mut();
    hm.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    hm.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    if let Some(r) = range {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{size}", r.start, r.end - 1)) {
            resp.headers_mut().insert(header::CONTENT_RANGE, v);
        }
    }
    decorate(resp.headers_mut());
    resp
}

#[allow(clippy::too_many_arguments)]
async fn resolve(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
    file: &str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Read) {
        return r;
    }
    let (hf2, r2, rev2, file2) = (hf.clone(), repo.clone(), rev.to_string(), file.to_string());
    let found = blocking(move || {
        let st = &*hf2.git;
        if !repo_exists(st, &r2)? {
            return Ok(Err("RepoNotFound"));
        }
        let view = View::new(st)?;
        let Some(res) = resolve_rev(st, &view, &r2, &rev2)? else { return Ok(Err("RevisionNotFound")) };
        let Some((mode, oid)) = find_path(&view, &res.tree, &file2)? else { return Ok(Err("EntryNotFound")) };
        if mode == "40000" {
            return Ok(Err("EntryNotFound"));
        }
        let (_, blob) = view.load(&oid)?.ok_or_else(|| Error::Corrupt(format!("missing blob {}", hex(&oid))))?;
        let lfs = match parse_lfs_pointer(&blob) {
            Some((sha, size)) => {
                // Only objects uploaded to this repo: a pushed pointer can't expose others' files.
                let Some(v) = hf2.lfs_for(&r2, &sha)? else { return Ok(Err("EntryNotFound")) };
                let xet = xet_file_for(&hf2, &r2, &sha)?.map(|f| crate::xet::to_hex(&f.file_hash));
                Some((sha, size, v.hash, xet))
            }
            None => None,
        };
        Ok(Ok((res.commit, oid, blob, lfs)))
    })
    .await;
    let (commit, oid, blob, lfs) = match found {
        Ok(Ok(x)) => x,
        Ok(Err(code)) => return hf_err(StatusCode::NOT_FOUND, Some(code), format!("{file} not found at {rev}")),
        Err(r) => return r,
    };
    let commit_hex = hex(&commit);
    let refresh = format!("{}/api/{}s/{}/xet-read-token/{commit_hex}", base_url(headers), repo.kind, repo.id());
    match lfs {
        Some((sha, size, h, xet)) => {
            serve_object(st, h, headers, head, |hm| {
                // Uploaded through Xet: clients may download it through Xet too.
                if let Some(x) = xet
                    && let (Ok(a), Ok(b)) = (HeaderValue::from_str(&x), HeaderValue::from_str(&refresh))
                {
                    hm.insert("x-xet-hash", a);
                    hm.insert("x-xet-refresh-route", b);
                }
                if let Ok(v) = HeaderValue::from_str(&commit_hex) {
                    hm.insert("x-repo-commit", v);
                }
                if let Ok(v) = HeaderValue::from_str(&format!("\"{sha}\"")) {
                    hm.insert("x-linked-etag", v.clone());
                    hm.insert(header::ETAG, v);
                }
                hm.insert("x-linked-size", HeaderValue::from(size));
            })
            .await
        }
        None => {
            // Regular (git) file: small, served from memory.
            let size = blob.len() as u64;
            let range = match parse_range(headers.get(header::RANGE), size) {
                Ok(r) => r,
                Err(Unsatisfiable) => return hf_err(StatusCode::RANGE_NOT_SATISFIABLE, None, "unsatisfiable range"),
            };
            let data = match &range {
                Some(r) => blob[r.start as usize..r.end as usize].to_vec(),
                None => blob,
            };
            let len = data.len();
            let mut resp = if head { Response::new(Body::empty()) } else { Response::new(Body::from(data)) };
            let hm = resp.headers_mut();
            hm.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            hm.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            if let Ok(v) = HeaderValue::from_str(&commit_hex) {
                hm.insert("x-repo-commit", v);
            }
            if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", hex(&oid))) {
                hm.insert(header::ETAG, v);
            }
            if let Some(r) = range {
                *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{size}", r.start, r.end - 1)) {
                    resp.headers_mut().insert(header::CONTENT_RANGE, v);
                }
            }
            resp
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit(
    st: &Arc<AppState>,
    hf: &Arc<HfState>,
    p: &Principal,
    c: &Caller,
    repo: &RepoId,
    rev: &str,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    if let Err(r) = check(st, p, c, repo, Access::Write) {
        return r;
    }
    if !st.audit.is_healthy() {
        return hf_err(StatusCode::SERVICE_UNAVAILABLE, None, "audit log unavailable; refusing commits");
    }
    let Ok(raw) = axum::body::to_bytes(body, MAX_COMMIT_BODY).await else {
        return hf_err(StatusCode::PAYLOAD_TOO_LARGE, None, "commit payload too large");
    };
    let mut summary = String::new();
    let mut description = String::new();
    let mut parent = None;
    let mut ops = Vec::new();
    for line in raw.split(|&b| b == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            return hf_err(StatusCode::BAD_REQUEST, None, "bad NDJSON line");
        };
        let val = &v["value"];
        let path = val.get("path").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        match v.get("key").and_then(|x| x.as_str()) {
            Some("header") => {
                summary = val.get("summary").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                description = val.get("description").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                parent = val.get("parentCommit").and_then(|x| x.as_str()).and_then(|s| parse_oid(s.as_bytes()));
            }
            Some("file") => {
                use base64::Engine as _;
                let content = match val.get("encoding").and_then(|x| x.as_str()) {
                    Some("base64") => match base64::engine::general_purpose::STANDARD
                        .decode(val.get("content").and_then(|x| x.as_str()).unwrap_or_default())
                    {
                        Ok(b) => b,
                        Err(_) => return hf_err(StatusCode::BAD_REQUEST, None, "bad base64"),
                    },
                    _ => val.get("content").and_then(|x| x.as_str()).unwrap_or_default().as_bytes().to_vec(),
                };
                if !valid_repo_path(&path) {
                    return hf_err(StatusCode::BAD_REQUEST, None, format!("invalid path {path:?}"));
                }
                ops.push(Op::Add { path, content });
            }
            Some("lfsFile") => {
                let sha = val.get("oid").and_then(|x| x.as_str()).unwrap_or_default().to_ascii_lowercase();
                let size = val.get("size").and_then(|x| x.as_u64());
                if !valid_repo_path(&path) || sha_key(&sha).is_none() {
                    return hf_err(StatusCode::BAD_REQUEST, None, format!("invalid LFS entry for {path:?}"));
                }
                // Size may be omitted (copies); take it from the index.
                let size = match size {
                    Some(s) => s,
                    None => {
                        let (hf2, r2, s2) = (hf.clone(), repo.clone(), sha.clone());
                        match blocking(move || hf2.lfs_for(&r2, &s2)).await {
                            Ok(Some(v)) => v.aux,
                            _ => {
                                return hf_err(
                                    StatusCode::UNPROCESSABLE_ENTITY,
                                    None,
                                    format!("LFS object {sha} unknown"),
                                );
                            }
                        }
                    }
                };
                ops.push(Op::AddLfs { path, sha, size });
            }
            Some("deletedFile") => ops.push(Op::Delete { path }),
            Some("deletedFolder") => ops.push(Op::DeleteFolder { path }),
            _ => return hf_err(StatusCode::BAD_REQUEST, None, "unknown commit operation"),
        }
    }
    let (hf2, r2, rev2, who) = (hf.clone(), repo.clone(), rev.to_string(), p.name.clone());
    let n_ops = ops.len();
    let res = blocking(move || make_commit(&hf2, &r2, &rev2, &who, &summary, &description, parent, &ops)).await;
    let oid = match res {
        Ok(Ok(oid)) => oid,
        Ok(Err((status, code, msg))) => {
            audit(st, p, c, repo, "hf_commit", "rejected", Some(msg.clone())).await;
            return hf_err(status, Some(code), msg);
        }
        Err(r) => return r,
    };
    audit(st, p, c, repo, "hf_commit", "ok", Some(format!("{} ops={n_ops} rev={rev}", hex(&oid)))).await;
    let hf2 = hf.clone();
    if let Ok(Ok(Some((_, ch)))) = tokio::task::spawn_blocking(move || View::new(&hf2.git)?.lookup(&oid)).await {
        let m = [
            ("repo", repo.id()),
            ("type", repo.kind.to_string()),
            ("revision", rev.to_string()),
            ("commit", hex(&oid)),
        ]
        .into_iter()
        .map(|(k, v): (&str, String)| (k.to_string(), v))
        .collect();
        crate::prov_api::record_publish(st, "hf", ch, &p.name, m);
    }
    let url = format!("{}/{}{}/commit/{}", base_url(headers), repo.url_prefix(), repo.id(), hex(&oid));
    axum::Json(serde_json::json!({ "commitUrl": url, "commitOid": hex(&oid), "pullRequestUrl": null, "success": true }))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfs_pointers() {
        let sha = "a".repeat(64);
        let p = lfs_pointer(&sha, 1234);
        assert_eq!(parse_lfs_pointer(&p), Some((sha.clone(), 1234)));
        assert_eq!(parse_lfs_pointer(b"not a pointer"), None);
        assert_eq!(parse_lfs_pointer(&[LFS_POINTER_PREFIX, b"oid sha256:zz\nsize 1\n"].concat()), None);
    }

    #[test]
    fn hmac_matches_rfc4231_case_2_shape() {
        // HMAC(key, msg) is deterministic and key-sensitive.
        let (k1, k2) = ([1u8; 32], [2u8; 32]);
        assert_eq!(hmac_sha256(&k1, b"x"), hmac_sha256(&k1, b"x"));
        assert_ne!(hmac_sha256(&k1, b"x"), hmac_sha256(&k2, b"x"));
    }

    #[test]
    fn paths_and_segments() {
        assert!(valid_segment("SmolLM2-135M") && valid_segment("my_model.v2"));
        assert!(
            !valid_segment("")
                && !valid_segment("..")
                && !valid_segment(".hidden")
                && !valid_segment("a b")
                && !valid_segment("x.git")
        );
        assert!(valid_repo_path("a/b/c.txt") && valid_repo_path(".gitattributes"));
        assert!(
            !valid_repo_path("/abs")
                && !valid_repo_path("a/../b")
                && !valid_repo_path("a//b")
                && !valid_repo_path(".git/config")
        );
        assert_eq!(urlencoding_decode("refs%2Fpr%2F1"), "refs/pr/1");
    }

    #[test]
    fn tree_insert_and_remove() {
        let o = Oid::from([7u8; 20]);
        let mut t = BTreeMap::new();
        insert_path(&mut t, "a/b/c.txt", Node::File { mode: "100644".into(), oid: o }).unwrap();
        insert_path(&mut t, "a/d.txt", Node::File { mode: "100644".into(), oid: o }).unwrap();
        remove_path(&mut t, "a/b", true);
        let Some(Node::Dir(a)) = t.get("a") else { panic!() };
        assert!(a.contains_key("d.txt") && !a.contains_key("b"));
        remove_path(&mut t, "a/d.txt", false);
        let Some(Node::Dir(a)) = t.get("a") else { panic!() };
        assert!(a.is_empty());
    }
}
