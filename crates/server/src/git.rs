//! Git smart HTTP on top of the repository: protocol v2 for fetch (`ls-refs`,
//! `fetch`), v0/v1 for fetch and push.
//!
//! Endpoints, for `<repo>` a path such as `team/project`:
//! * `GET  /git/<repo>.git/info/refs?service=git-upload-pack|git-receive-pack`
//! * `POST /git/<repo>.git/git-upload-pack` (fetch and clone)
//! * `POST /git/<repo>.git/git-receive-pack` (push)
//!
//! Hash mapping (agreed design, docs/NET_PROGRESS.md):
//! * Every git object is stored as its canonical loose-object bytes
//!   `<type> <size>\0<body>`. That is the Experiment 1 payload, and its git oid
//!   is `sha1` of exactly those bytes. The internal id is BLAKE3 of the same bytes.
//! * `git/_oidmap` (one ref) → a global [`SegMap`] holding oid → BLAKE3, with
//!   `aux = type << 56 | size`. Global is safe because the mapping is a pure function.
//! * `git/<repo>/refs/...` → BLAKE3 of the commit or tag. A push command
//!   `old new ref` is a compare-and-swap: `replace` with the observed version.
//!   `--atomic` uses `replace_many`.
//! * GC: [`GitTracer`] follows commit → tree and parents, tree → entries, and
//!   tag → object through the oid map. Map values are weak, so liveness comes
//!   only from refs.
//!
//! Limits of this version (also in the production gaps): SHA-1 repositories
//! only, no shallow clones, deltified
//! (no deltas) in served packs, and delta bases resolved in a bounded
//! in-memory cache.

use crate::api::{AppState, Caller, IDLE_TIMEOUT};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::Access;
use crate::segmap::{Key, Root, SegCache, SegMap, Value};
use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use pico2peta_core::{
    Error, Hash, Name, ObjectStore, Observed, RefValue, ReplaceManyOutcome, ReplaceOutcome, Repository, Result,
};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// A git object id: SHA-1 (20 bytes) or SHA-256 (32 bytes) of the canonical object.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Oid {
    b: [u8; 32],
    n: u8,
}

impl Oid {
    pub fn from_slice(s: &[u8]) -> Option<Oid> {
        if s.len() != 20 && s.len() != 32 {
            return None;
        }
        let mut b = [0u8; 32];
        b[..s.len()].copy_from_slice(s);
        Some(Oid { b, n: s.len() as u8 })
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.b[..self.n as usize]
    }
    pub fn fmt(&self) -> Fmt {
        if self.n == 32 { Fmt::Sha256 } else { Fmt::Sha1 }
    }
}

impl std::ops::Deref for Oid {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl PartialEq<[u8; 20]> for Oid {
    fn eq(&self, o: &[u8; 20]) -> bool {
        self.as_bytes() == o
    }
}

impl From<[u8; 20]> for Oid {
    fn from(a: [u8; 20]) -> Oid {
        Oid::from_slice(&a).unwrap_or_else(|| unreachable!("20 bytes"))
    }
}

/// A repository's object format (`object-format=sha1|sha256`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Fmt {
    #[default]
    Sha1,
    Sha256,
}

impl Fmt {
    pub fn hash_len(self) -> usize {
        match self {
            Fmt::Sha1 => 20,
            Fmt::Sha256 => 32,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Fmt::Sha1 => "sha1",
            Fmt::Sha256 => "sha256",
        }
    }
    pub fn zero(self) -> Oid {
        Oid { b: [0; 32], n: self.hash_len() as u8 }
    }
    /// The object id of canonical bytes (also the pack checksum algorithm).
    pub fn hash(self, data: &[u8]) -> Oid {
        match self {
            Fmt::Sha1 => Oid::from(<[u8; 20]>::from(Sha1::digest(data))),
            Fmt::Sha256 => {
                Oid::from_slice(&<[u8; 32]>::from(sha2::Sha256::digest(data))).unwrap_or_else(|| unreachable!())
            }
        }
    }
}

/// Incremental pack checksum in a repository's format.
pub(crate) enum PackHasher {
    Sha1(Sha1),
    Sha256(sha2::Sha256),
}

impl PackHasher {
    pub(crate) fn new(f: Fmt) -> PackHasher {
        match f {
            Fmt::Sha1 => PackHasher::Sha1(Sha1::new()),
            Fmt::Sha256 => PackHasher::Sha256(sha2::Sha256::new()),
        }
    }
    pub(crate) fn update(&mut self, d: &[u8]) {
        match self {
            PackHasher::Sha1(h) => h.update(d),
            PackHasher::Sha256(h) => h.update(d),
        }
    }
    pub(crate) fn finish(self) -> Vec<u8> {
        match self {
            PackHasher::Sha1(h) => h.finalize().to_vec(),
            PackHasher::Sha256(h) => h.finalize().to_vec(),
        }
    }
}
const AGENT: &str = "agent=pico2peta/0.1";
/// Budget for resolved delta bases kept in memory while indexing a pack.
const DELTA_CACHE_BYTES: usize = 256 << 20;
/// Canonical object bytes kept while indexing a pushed pack, until the batched "already
/// stored?" lookup; objects beyond it are re-resolved if they turn out to be new.
const KEEP_CANON_BYTES: usize = 256 << 20;
/// Largest single object accepted in a push.
const MAX_OBJECT: usize = 1 << 30;
const MAX_DELTA_DEPTH: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
            Kind::Tag => "tag",
        }
    }
    fn from_code(c: u8) -> Option<Kind> {
        Some(match c {
            1 => Kind::Commit,
            2 => Kind::Tree,
            3 => Kind::Blob,
            4 => Kind::Tag,
            _ => return None,
        })
    }
    fn from_name(s: &[u8]) -> Option<Kind> {
        Some(match s {
            b"commit" => Kind::Commit,
            b"tree" => Kind::Tree,
            b"blob" => Kind::Blob,
            b"tag" => Kind::Tag,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Object model
// ---------------------------------------------------------------------------

/// `<type> <size>\0<body>`.
pub fn canonical(kind: Kind, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 16);
    v.extend_from_slice(kind.name().as_bytes());
    v.push(b' ');
    v.extend_from_slice(body.len().to_string().as_bytes());
    v.push(0);
    v.extend_from_slice(body);
    v
}

/// Parses a canonical header: `(kind, body size, header length)`.
pub fn parse_header(b: &[u8]) -> Option<(Kind, usize, usize)> {
    let sp = b.iter().take(8).position(|&c| c == b' ')?;
    let kind = Kind::from_name(&b[..sp])?;
    let nul = sp + 1 + b[sp + 1..].iter().take(21).position(|&c| c == 0)?;
    let digits = &b[sp + 1..nul];
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let size: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((kind, size, nul + 1))
}

/// SHA-1 object id (the HF front's repos, and the default format).
pub fn oid_of(canonical: &[u8]) -> Oid {
    Fmt::Sha1.hash(canonical)
}

/// The oid map key: the id, zero-padded to 32 bytes (SHA-1 and SHA-256 ids never collide in
/// practice; a SHA-256 id would need 12 trailing zero bytes).
pub fn key_of(oid: &Oid) -> Key {
    let mut k = [0u8; 32];
    k[..oid.len()].copy_from_slice(oid);
    k
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 40 hex digits (SHA-1) or 64 (SHA-256).
pub fn parse_oid(s: &[u8]) -> Option<Oid> {
    if s.len() != 40 && s.len() != 64 {
        return None;
    }
    let mut o = [0u8; 32];
    for (i, b) in o.iter_mut().take(s.len() / 2).enumerate() {
        *b = u8::from_str_radix(std::str::from_utf8(&s[2 * i..2 * i + 2]).ok()?, 16).ok()?;
    }
    Oid::from_slice(&o[..s.len() / 2])
}

/// Oids an object of a SHA-1 repository references (submodule commits in trees are skipped).
pub fn refs_of(kind: Kind, body: &[u8]) -> Result<Vec<Oid>> {
    refs_of_fmt(Fmt::Sha1, kind, body)
}

/// References under either object format (for code that doesn't know the repository: GC
/// tracing, repack hints). A wrong-format parse fails or yields ids absent from the oid map.
pub fn refs_any(kind: Kind, body: &[u8]) -> Vec<Oid> {
    let mut out = refs_of_fmt(Fmt::Sha1, kind, body).unwrap_or_default();
    if let Ok(more) = refs_of_fmt(Fmt::Sha256, kind, body) {
        out.extend(more);
    }
    out
}

/// [`refs_of`] for a repository in format `fmt` (the tree entry hash length).
pub fn refs_of_fmt(fmt: Fmt, kind: Kind, body: &[u8]) -> Result<Vec<Oid>> {
    let hl = fmt.hash_len();
    let bad = |m: &str| Error::Corrupt(format!("git {}: {m}", kind.name()));
    let mut out = Vec::new();
    match kind {
        Kind::Blob => {}
        Kind::Commit | Kind::Tag => {
            for line in body.split(|&c| c == b'\n') {
                if line.is_empty() {
                    break; // end of headers
                }
                let (k, v) = match line.iter().position(|&c| c == b' ') {
                    Some(i) => (&line[..i], &line[i + 1..]),
                    None => continue,
                };
                let wanted = match kind {
                    Kind::Commit => k == b"tree" || k == b"parent",
                    _ => k == b"object",
                };
                if wanted {
                    let o = parse_oid(v).filter(|o| o.len() == hl).ok_or_else(|| bad("bad oid in header"))?;
                    out.push(o);
                }
            }
            if kind == Kind::Commit && out.is_empty() {
                return Err(bad("commit without tree"));
            }
        }
        Kind::Tree => {
            let mut i = 0;
            while i < body.len() {
                let sp = body[i..].iter().position(|&c| c == b' ').ok_or_else(|| bad("entry without mode"))? + i;
                let mode = &body[i..sp];
                let nul = body[sp..].iter().position(|&c| c == 0).ok_or_else(|| bad("entry without name"))? + sp;
                if nul + 1 + hl > body.len() {
                    return Err(bad("truncated entry"));
                }
                let o = Oid::from_slice(&body[nul + 1..nul + 1 + hl]).ok_or_else(|| bad("bad entry"))?;
                if mode != b"160000" {
                    out.push(o);
                }
                i = nul + 1 + hl;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// pkt-line
// ---------------------------------------------------------------------------

pub fn pkt(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data);
    v
}

pub const FLUSH: &[u8] = b"0000";

/// A parsed pkt-line: `None` is a flush packet.
fn read_pkt(r: &mut dyn Read) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).map_err(|e| Error::Parse(format!("pkt-line: {e}")))?;
    let n =
        usize::from_str_radix(std::str::from_utf8(&len).map_err(|_| Error::Parse("bad pkt-line length".into()))?, 16)
            .map_err(|_| Error::Parse("bad pkt-line length".into()))?;
    match n {
        0 => Ok(None),
        1..=3 => Ok(Some(Vec::new())), // delim/response-end (v2); treated as empty
        _ => {
            if n > 65520 {
                return Err(Error::Parse("pkt-line too long".into()));
            }
            let mut v = vec![0u8; n - 4];
            r.read_exact(&mut v).map_err(|e| Error::Parse(format!("pkt-line: {e}")))?;
            Ok(Some(v))
        }
    }
}

fn trim_nl(mut b: &[u8]) -> &[u8] {
    if b.last() == Some(&b'\n') {
        b = &b[..b.len() - 1];
    }
    b
}

/// Writes `data` as side-band channel `band` packets (or raw if `!sideband`).
fn sideband(out: &mut Vec<u8>, band: u8, data: &[u8], sideband: bool) {
    if !sideband {
        out.extend_from_slice(data);
        return;
    }
    for chunk in data.chunks(65515) {
        let mut p = Vec::with_capacity(chunk.len() + 1);
        p.push(band);
        p.extend_from_slice(chunk);
        out.extend_from_slice(&pkt(&p));
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct GitState {
    pub(crate) repo: Arc<dyn Repository>,
    pub(crate) oidmap: Arc<SegMap>,
    spool: PathBuf,
    max_push_bytes: u64,
    /// Repo-name prefixes whose repositories use SHA-256 (`[git] sha256_repos`).
    sha256: Vec<String>,
}

impl GitState {
    pub fn new(repo: Arc<dyn Repository>, spool: PathBuf, max_push_bytes: u64) -> std::io::Result<GitState> {
        if spool.exists() {
            std::fs::remove_dir_all(&spool)?;
        }
        std::fs::create_dir_all(&spool)?;
        let n = Name::new("git/_oidmap").unwrap_or_else(|_| unreachable!("static name is valid"));
        let oidmap = Arc::new(SegMap::new(repo.clone(), n, Arc::new(SegCache::default())));
        Ok(GitState { repo, oidmap, spool, max_push_bytes, sha256: Vec::new() })
    }

    /// Repositories under these name prefixes are SHA-256 repositories.
    pub fn with_sha256_repos(mut self, prefixes: Vec<String>) -> GitState {
        self.sha256 = prefixes;
        self
    }

    /// The object format of repository `repo` (fixed by configuration: git announces it
    /// before the first push, so it can't be learned from the push).
    pub(crate) fn fmt_for(&self, repo: &str) -> Fmt {
        if self.sha256.iter().any(|p| pico2peta_core::is_under(repo, p)) { Fmt::Sha256 } else { Fmt::Sha1 }
    }
}

/// A consistent view of the oid map plus object loading.
pub(crate) struct View<'a> {
    st: &'a GitState,
    root: Root,
    /// Objects added by the current push, not yet in the map.
    pub(crate) extra: HashMap<Oid, (Kind, Hash)>,
    /// Object format of the repository being served (hashing new objects, pack checksums).
    pub(crate) fmt: Fmt,
}

/// A looked-up object: kind, stored object, recorded body length (`None` for this push's).
pub(crate) type Located = (Kind, Hash, Option<u64>);
/// A unique object of a received pack: (entry, kind, oid, body length, canonical bytes if kept).
type PackObj = (usize, Kind, Oid, usize, Option<Vec<u8>>);
/// One looked-up object of a walk wave: (oid, path, kind, stored object, body length).
type WaveItem = (Oid, String, Kind, Hash, Option<u64>);

impl<'a> View<'a> {
    pub(crate) fn new(st: &'a GitState) -> Result<View<'a>> {
        Ok(View { st, root: st.oidmap.root_blocking()?.1, extra: HashMap::new(), fmt: Fmt::Sha1 })
    }

    /// References of an object in this view's repository format.
    pub(crate) fn refs(&self, kind: Kind, body: &[u8]) -> Result<Vec<Oid>> {
        refs_of_fmt(self.fmt, kind, body)
    }

    pub(crate) fn for_repo(st: &'a GitState, repo: &str) -> Result<View<'a>> {
        let mut v = View::new(st)?;
        v.fmt = st.fmt_for(repo);
        Ok(v)
    }

    pub(crate) fn lookup(&self, oid: &Oid) -> Result<Option<(Kind, Hash)>> {
        if let Some(x) = self.extra.get(oid) {
            return Ok(Some(*x));
        }
        match self.st.oidmap.get_in(&self.root, &key_of(oid))? {
            Some(v) => {
                let kind = Kind::from_code((v.aux >> 56) as u8)
                    .ok_or_else(|| Error::Corrupt(format!("oid map: bad type for {}", hex(oid))))?;
                Ok(Some((kind, v.hash)))
            }
            None => Ok(None),
        }
    }

    /// [`Self::lookup`] for many oids with one batched oid-map query (results in input order).
    pub(crate) fn lookup_many(&self, oids: &[Oid]) -> Result<Vec<Option<(Kind, Hash)>>> {
        Ok(self.lookup_many_len(oids)?.into_iter().map(|x| x.map(|(k, h, _)| (k, h))).collect())
    }

    /// [`Self::lookup_many`] plus the recorded body length (`None` for objects of this push).
    pub(crate) fn lookup_many_len(&self, oids: &[Oid]) -> Result<Vec<Option<Located>>> {
        let keys: Vec<Key> = oids.iter().filter(|o| !self.extra.contains_key(*o)).map(key_of).collect();
        let mut got = self.st.oidmap.get_many(&self.root, &keys)?.into_iter();
        oids.iter()
            .map(|o| {
                if let Some((k, h)) = self.extra.get(o) {
                    return Ok(Some((*k, *h, None)));
                }
                match got.next().flatten() {
                    Some(v) => {
                        let kind = Kind::from_code((v.aux >> 56) as u8)
                            .ok_or_else(|| Error::Corrupt(format!("oid map: bad type for {}", hex(o))))?;
                        Ok(Some((kind, v.hash, Some(v.aux & ((1 << 56) - 1)))))
                    }
                    None => Ok(None),
                }
            })
            .collect()
    }

    /// `(kind, body)` of `oid`, or `None` if unknown/collected.
    pub(crate) fn load(&self, oid: &Oid) -> Result<Option<(Kind, Vec<u8>)>> {
        let Some((kind, h)) = self.lookup(oid)? else { return Ok(None) };
        Ok(self.load_stored(kind, &h)?.map(|b| (kind, b)))
    }

    /// The body of a stored object already looked up (`None` if collected).
    pub(crate) fn load_stored(&self, kind: Kind, h: &Hash) -> Result<Option<Vec<u8>>> {
        let h = *h;
        let bytes = match self.st.repo.get(&h, None) {
            Ok(b) => b,
            Err(Error::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let (k, size, hl) =
            parse_header(&bytes).ok_or_else(|| Error::Corrupt(format!("object {h}: bad git header")))?;
        if k != kind || hl + size != bytes.len() {
            return Err(Error::Corrupt(format!("object {h}: header disagrees with oid map")));
        }
        Ok(Some(bytes[hl..].to_vec()))
    }
}

// ---------------------------------------------------------------------------
// Refs
// ---------------------------------------------------------------------------

fn ns(repo: &str) -> String {
    format!("git/{repo}")
}

/// `git/<repo>/<gitref>` if `gitref` is representable as a core name.
pub(crate) fn ref_name(repo: &str, gitref: &str) -> Option<Name> {
    if !gitref.starts_with("refs/") || gitref.ends_with(".lock") || gitref.contains("..") {
        return None;
    }
    Name::new(format!("git/{repo}/{gitref}")).ok()
}

/// Current refs of `repo`: `(gitref, oid, core value)`, sorted.
pub(crate) fn list_refs(st: &GitState, repo: &str) -> Result<Vec<(String, Oid, RefValue)>> {
    let prefix = format!("git/{repo}/refs");
    let mut out = Vec::new();
    for (n, v) in st.repo.list_refs(&prefix)? {
        let Some(gitref) = n.as_str().strip_prefix(&format!("git/{repo}/")) else { continue };
        let bytes = st.repo.get(&v.hash, None)?;
        out.push((gitref.to_string(), st.fmt_for(repo).hash(&bytes), v));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn head_target(refs: &[(String, Oid, RefValue)]) -> Option<&(String, Oid, RefValue)> {
    ["refs/heads/main", "refs/heads/master"]
        .iter()
        .find_map(|want| refs.iter().find(|r| r.0 == *want))
        .or_else(|| refs.iter().find(|r| r.0.starts_with("refs/heads/")))
}

fn advertisement(st: &GitState, repo: &str, service: &str) -> Result<Vec<u8>> {
    let refs = list_refs(st, repo)?;
    let fmt = st.fmt_for(repo);
    let of = format!("object-format={}", fmt.name());
    let mut out = pkt(format!("# service={service}\n").as_bytes());
    out.extend_from_slice(FLUSH);
    let mut caps = match service {
        "git-upload-pack" => vec!["side-band-64k", "no-progress", "ofs-delta", of.as_str(), AGENT],
        _ => vec!["report-status", "delete-refs", "side-band-64k", "atomic", "ofs-delta", "quiet", of.as_str(), AGENT],
    }
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let head = if service == "git-upload-pack" { head_target(&refs).cloned() } else { None };
    if let Some(h) = &head {
        caps.insert(0, format!("symref=HEAD:{}", h.0));
    }
    let caps = caps.join(" ");
    let mut lines: Vec<(String, Oid)> = Vec::new();
    if let Some(h) = &head {
        lines.push(("HEAD".into(), h.1));
    }
    lines.extend(refs.iter().map(|r| (r.0.clone(), r.1)));
    if lines.is_empty() {
        out.extend_from_slice(&pkt(format!("{} capabilities^{{}}\0{caps}\n", hex(&fmt.zero())).as_bytes()));
    }
    for (i, (name, oid)) in lines.iter().enumerate() {
        let line = if i == 0 { format!("{} {name}\0{caps}\n", hex(oid)) } else { format!("{} {name}\n", hex(oid)) };
        out.extend_from_slice(&pkt(line.as_bytes()));
    }
    out.extend_from_slice(FLUSH);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pack reading (receive-pack)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Base {
    None,
    Ofs(u64),
    Ref(Oid),
}

struct Entry {
    offset: u64,
    code: u8,
    size: usize,
    data_off: u64,
    base: Base,
    /// Filled once resolved.
    done: Option<(Kind, Oid)>,
}

fn read_entry_header(r: &mut dyn Read, offset: u64, hash_len: usize) -> Result<(u8, usize, Base, u64)> {
    let bad = |m: &str| Error::Parse(format!("pack entry at {offset}: {m}"));
    let mut byte = [0u8; 1];
    let mut used = 0u64;
    let mut next = |r: &mut dyn Read| -> Result<u8> {
        r.read_exact(&mut byte).map_err(|e| Error::Parse(format!("pack entry at {offset}: {e}")))?;
        used += 1;
        Ok(byte[0])
    };
    let mut c = next(r)?;
    let code = (c >> 4) & 7;
    let mut size = (c & 15) as usize;
    let mut shift = 4;
    while c & 0x80 != 0 {
        c = next(r)?;
        if shift > 60 {
            return Err(bad("size varint too long"));
        }
        size |= ((c & 0x7f) as usize) << shift;
        shift += 7;
    }
    let base = match code {
        6 => {
            let mut c = next(r)?;
            let mut off = (c & 0x7f) as u64;
            while c & 0x80 != 0 {
                c = next(r)?;
                off = off.checked_add(1).and_then(|o| o.checked_mul(128)).ok_or_else(|| bad("ofs overflow"))?
                    | (c & 0x7f) as u64;
            }
            if off == 0 || off > offset {
                return Err(bad("ofs-delta base out of range"));
            }
            Base::Ofs(offset - off)
        }
        7 => {
            let mut o = [0u8; 32];
            for b in o.iter_mut().take(hash_len) {
                *b = next(r)?;
            }
            Base::Ref(Oid::from_slice(&o[..hash_len]).ok_or_else(|| bad("bad ref-delta base"))?)
        }
        1..=4 => Base::None,
        _ => return Err(bad("invalid object type")),
    };
    if size > MAX_OBJECT {
        return Err(bad("object too large"));
    }
    Ok((code, size, base, used))
}

/// Inflates the zlib stream at `off`; returns `(data, compressed length)`.
fn inflate_at(f: &mut std::fs::File, off: u64, expect: usize) -> Result<(Vec<u8>, u64)> {
    f.seek(SeekFrom::Start(off))?;
    let mut z = flate2::bufread::ZlibDecoder::new(BufReader::with_capacity(64 << 10, &mut *f));
    // Never trust the declared size for allocation; grow as data arrives.
    let mut out = Vec::with_capacity(expect.min(1 << 20));
    (&mut z)
        .take(expect as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| Error::Parse(format!("pack inflate at {off}: {e}")))?;
    if out.len() != expect {
        return Err(Error::Parse(format!("pack entry at {off}: inflated {} bytes, header says {expect}", out.len())));
    }
    // Drain to the end of the zlib stream so total_in is the full compressed length.
    let mut sink = [0u8; 1];
    if z.read(&mut sink).map_err(|e| Error::Parse(format!("pack inflate at {off}: {e}")))? != 0 {
        return Err(Error::Parse(format!("pack entry at {off}: trailing data in zlib stream")));
    }
    Ok((out, z.total_in()))
}

/// Like [`inflate_at`] but discards the output: returns the compressed length.
/// Used to find entry boundaries without allocating the declared size.
fn skip_zlib_at(f: &mut std::fs::File, off: u64, expect: usize) -> Result<u64> {
    f.seek(SeekFrom::Start(off))?;
    let mut z = flate2::bufread::ZlibDecoder::new(BufReader::with_capacity(64 << 10, &mut *f));
    let mut buf = [0u8; 64 << 10];
    let mut total = 0usize;
    loop {
        let n = z.read(&mut buf).map_err(|e| Error::Parse(format!("pack inflate at {off}: {e}")))?;
        if n == 0 {
            break;
        }
        total += n;
        if total > expect {
            return Err(Error::Parse(format!("pack entry at {off}: inflates beyond its declared {expect} bytes")));
        }
    }
    if total != expect {
        return Err(Error::Parse(format!("pack entry at {off}: inflated {total} bytes, header says {expect}")));
    }
    Ok(z.total_in())
}

fn delta_varint(d: &[u8], i: &mut usize) -> Result<usize> {
    let mut v = 0usize;
    let mut shift = 0;
    loop {
        let c = *d.get(*i).ok_or_else(|| Error::Parse("truncated delta header".into()))?;
        *i += 1;
        if shift > 60 {
            return Err(Error::Parse("delta varint too long".into()));
        }
        v |= ((c & 0x7f) as usize) << shift;
        shift += 7;
        if c & 0x80 == 0 {
            return Ok(v);
        }
    }
}

/// Applies a git delta to `base`.
pub fn apply_delta(base: &[u8], d: &[u8]) -> Result<Vec<u8>> {
    let bad = |m: &str| Error::Parse(format!("delta: {m}"));
    let mut i = 0;
    let src = delta_varint(d, &mut i)?;
    let dst = delta_varint(d, &mut i)?;
    if src != base.len() {
        return Err(bad("base size mismatch"));
    }
    if dst > MAX_OBJECT {
        return Err(bad("result too large"));
    }
    let mut out = Vec::with_capacity(dst.min(1 << 20));
    while i < d.len() {
        let op = d[i];
        i += 1;
        if op & 0x80 != 0 {
            let mut off = 0usize;
            let mut len = 0usize;
            for bit in 0..4 {
                if op & (1 << bit) != 0 {
                    off |= (*d.get(i).ok_or_else(|| bad("truncated copy"))? as usize) << (8 * bit);
                    i += 1;
                }
            }
            for bit in 0..3 {
                if op & (0x10 << bit) != 0 {
                    len |= (*d.get(i).ok_or_else(|| bad("truncated copy"))? as usize) << (8 * bit);
                    i += 1;
                }
            }
            if len == 0 {
                len = 0x10000;
            }
            let end = off.checked_add(len).ok_or_else(|| bad("copy overflow"))?;
            if end > base.len() {
                return Err(bad("copy out of range"));
            }
            out.extend_from_slice(&base[off..end]);
        } else if op != 0 {
            let n = op as usize;
            let end = i + n;
            if end > d.len() {
                return Err(bad("truncated insert"));
            }
            out.extend_from_slice(&d[i..end]);
            i = end;
        } else {
            return Err(bad("reserved opcode 0"));
        }
        if out.len() > dst {
            return Err(bad("result longer than declared"));
        }
    }
    if out.len() != dst {
        return Err(bad("result size mismatch"));
    }
    Ok(out)
}

/// Bounded cache of resolved bodies by pack offset.
struct BodyCache {
    map: HashMap<u64, Arc<Vec<u8>>>,
    order: VecDeque<u64>,
    bytes: usize,
}

impl BodyCache {
    fn new() -> Self {
        BodyCache { map: HashMap::new(), order: VecDeque::new(), bytes: 0 }
    }
    fn get(&self, off: u64) -> Option<Arc<Vec<u8>>> {
        self.map.get(&off).cloned()
    }
    fn put(&mut self, off: u64, b: Arc<Vec<u8>>) {
        if b.len() > DELTA_CACHE_BYTES / 4 {
            return;
        }
        self.bytes += b.len();
        self.map.insert(off, b);
        self.order.push_back(off);
        while self.bytes > DELTA_CACHE_BYTES {
            let Some(o) = self.order.pop_front() else { break };
            if let Some(v) = self.map.remove(&o) {
                self.bytes -= v.len();
            }
        }
    }
}

/// Result of indexing a received pack.
struct Indexed {
    objects: usize,
    new_entries: Vec<(Key, Value)>,
}

/// Reads, verifies and stores every object of the pack at `pack_off` in `file`.
fn index_pack(view: &mut View<'_>, file: &mut std::fs::File, pack_off: u64) -> Result<Indexed> {
    let len = file.metadata()?.len();
    if len < pack_off + 32 {
        return Err(Error::Parse("pack too short".into()));
    }
    let tl = view.fmt.hash_len() as u64;
    // Trailer: the repository's hash (SHA-1 or SHA-256) of everything before it.
    {
        file.seek(SeekFrom::Start(pack_off))?;
        let mut h = PackHasher::new(view.fmt);
        let mut left = len - pack_off - tl;
        let mut buf = vec![0u8; 1 << 20];
        let mut r = BufReader::new(&mut *file);
        while left > 0 {
            let n = (left as usize).min(buf.len());
            r.read_exact(&mut buf[..n])?;
            h.update(&buf[..n]);
            left -= n as u64;
        }
        let mut trailer = vec![0u8; tl as usize];
        r.read_exact(&mut trailer)?;
        if h.finish() != trailer {
            return Err(Error::Parse("pack checksum mismatch".into()));
        }
    }
    file.seek(SeekFrom::Start(pack_off))?;
    let mut hdr = [0u8; 12];
    file.read_exact(&mut hdr)?;
    if &hdr[..4] != b"PACK" || !matches!(u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]), 2 | 3) {
        return Err(Error::Parse("not a version 2/3 pack".into()));
    }
    let count = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
    let end = len - tl;
    // Pass 1: entry boundaries.
    let mut entries: Vec<Entry> = Vec::with_capacity(count.min(1 << 16));
    let mut off = pack_off + 12;
    for _ in 0..count {
        if off >= end {
            return Err(Error::Parse("pack truncated".into()));
        }
        file.seek(SeekFrom::Start(off))?;
        let (code, size, base, hl) = read_entry_header(&mut BufReader::new(&mut *file), off, view.fmt.hash_len())?;
        let data_off = off + hl;
        // Inflate (discarding output) only to find the entry boundary.
        let clen = skip_zlib_at(file, data_off, size)?;
        entries.push(Entry { offset: off, code, size, data_off, base, done: None });
        off = data_off + clen;
    }
    if off != end {
        return Err(Error::Parse("trailing garbage after pack entries".into()));
    }
    let by_offset: HashMap<u64, usize> = entries.iter().enumerate().map(|(i, e)| (e.offset, i)).collect();
    let mut cache = BodyCache::new();
    let mut new_entries = Vec::new();
    let mut needed: HashSet<Oid> = HashSet::new();
    let mut in_pack: HashSet<Oid> = HashSet::new();
    // Unique objects in pack order: (entry, kind, oid, body length, canonical bytes if kept).
    // Whether each is already stored is decided afterwards with one batched oid-map lookup.
    let mut objs: Vec<PackObj> = Vec::new();
    let mut kept = 0usize;
    // Resolved in-pack objects by oid (REF_DELTA bases).
    let mut by_oid: HashMap<Oid, usize> = HashMap::new();

    // Resolve in rounds: REF_DELTA bases may appear later in the pack.
    let mut pending: Vec<usize> = (0..entries.len()).collect();
    while !pending.is_empty() {
        let mut next = Vec::new();
        let before = pending.len();
        for idx in pending {
            match resolve(view, file, &mut entries, &by_offset, &by_oid, &mut cache, idx, 0)? {
                Some((kind, body)) => {
                    let canon = canonical(kind, &body);
                    let oid = view.fmt.hash(&canon);
                    if !in_pack.contains(&oid) {
                        needed.extend(view.refs(kind, &body)?);
                        in_pack.insert(oid);
                        // Keep canonical bytes up to a budget; the rest is re-resolved if new.
                        let keep = kept + canon.len() <= KEEP_CANON_BYTES;
                        if keep {
                            kept += canon.len();
                        }
                        objs.push((idx, kind, oid, body.len(), keep.then_some(canon)));
                    }
                    entries[idx].done = Some((kind, oid));
                    by_oid.entry(oid).or_insert(idx);
                }
                None => next.push(idx),
            }
        }
        if next.len() == before {
            let missing: Vec<String> = next
                .iter()
                .filter_map(|&i| match entries[i].base {
                    Base::Ref(o) => Some(hex(&o)),
                    _ => None,
                })
                .take(3)
                .collect();
            return Err(Error::Parse(format!("unresolvable delta bases: {}", missing.join(", "))));
        }
        pending = next;
    }
    // One batched lookup: the pack's objects (already stored under this oid, e.g. a re-push or
    // a fork's objects: skip the put and the oid-map entry; `ensure_live` rewrites it if GC
    // condemned it) and the outside objects the pack references (connectivity).
    let outside: Vec<Oid> = needed.into_iter().filter(|o| !in_pack.contains(o)).collect();
    let all: Vec<Oid> = objs.iter().map(|o| o.2).chain(outside.iter().copied()).collect();
    let found = view.lookup_many(&all)?;
    let (found_pack, found_out) = found.split_at(objs.len());
    for ((idx, kind, oid, body_len, canon), f) in objs.into_iter().zip(found_pack) {
        let known = match f {
            Some((k, h)) if *k == kind && view.st.repo.ensure_live(h)? => Some(*h),
            _ => None,
        };
        let h = match known {
            Some(h) => h,
            None => {
                let canon = match canon {
                    Some(c) => c,
                    None => {
                        let (k, body) = resolve(view, file, &mut entries, &by_offset, &by_oid, &mut cache, idx, 0)?
                            .ok_or_else(|| Error::Corrupt("pack entry no longer resolves".into()))?;
                        canonical(k, &body)
                    }
                };
                let h = view.st.repo.put(&canon)?;
                new_entries.push((key_of(&oid), Value { hash: h, aux: ((kind as u64) << 56) | body_len as u64 }));
                h
            }
        };
        view.extra.insert(oid, (kind, h));
    }
    // Connectivity: everything referenced exists in the pack or the repository.
    for (oid, f) in outside.iter().zip(found_out) {
        let live = match f {
            Some((_, h)) => view.st.repo.ensure_live(h)?,
            None => false,
        };
        if !live {
            return Err(Error::Parse(format!("pack references missing object {}", hex(oid))));
        }
    }
    Ok(Indexed { objects: count, new_entries })
}

/// Resolves entry `idx` to `(kind, body)`. `None` = its REF_DELTA base is not available yet.
#[allow(clippy::too_many_arguments)]
fn resolve(
    view: &View<'_>,
    file: &mut std::fs::File,
    entries: &mut [Entry],
    by_offset: &HashMap<u64, usize>,
    by_oid: &HashMap<Oid, usize>,
    cache: &mut BodyCache,
    idx: usize,
    depth: usize,
) -> Result<Option<(Kind, Arc<Vec<u8>>)>> {
    if depth > MAX_DELTA_DEPTH {
        return Err(Error::Parse("delta chain too deep".into()));
    }
    let e = &entries[idx];
    let (off, code, size, data_off, base) = (e.offset, e.code, e.size, e.data_off, e.base);
    if let (Some((kind, _)), Some(b)) = (e.done, cache.get(off)) {
        return Ok(Some((kind, b)));
    }
    let (kind, body) = match base {
        Base::None => {
            let kind = Kind::from_code(code).ok_or_else(|| Error::Parse("bad object type".into()))?;
            (kind, Arc::new(inflate_at(file, data_off, size)?.0))
        }
        Base::Ofs(boff) => {
            let bidx = *by_offset.get(&boff).ok_or_else(|| Error::Parse("ofs-delta base is not an entry".into()))?;
            let Some((kind, b)) = resolve(view, file, entries, by_offset, by_oid, cache, bidx, depth + 1)? else {
                return Ok(None);
            };
            let delta = inflate_at(file, data_off, size)?.0;
            (kind, Arc::new(apply_delta(&b, &delta)?))
        }
        Base::Ref(boid) => {
            let in_pack = by_oid.get(&boid).copied();
            let (kind, b) = match in_pack {
                Some(bidx) => match resolve(view, file, entries, by_offset, by_oid, cache, bidx, depth + 1)? {
                    Some(x) => x,
                    None => return Ok(None),
                },
                None => match view.load(&boid)? {
                    // Thin pack: base already in the repository.
                    Some((k, body)) => (k, Arc::new(body)),
                    None => return Ok(None),
                },
            };
            let delta = inflate_at(file, data_off, size)?.0;
            (kind, Arc::new(apply_delta(&b, &delta)?))
        }
    };
    cache.put(off, body.clone());
    Ok(Some((kind, body)))
}

// ---------------------------------------------------------------------------
// Pack writing (upload-pack)
// ---------------------------------------------------------------------------

/// A previously sent object usable as a delta base: (its index in the pack, body, chain depth).
type SentBase = (usize, Arc<Vec<u8>>, u32);

/// Objects encoded together (in parallel) while writing a pack: at most this many...
const SEND_WINDOW: usize = 256;
/// ...and at most about this many body bytes.
const SEND_WINDOW_BYTES: u64 = 64 << 20;

/// `HashMap` hasher for keys that are already well-mixed `u64`s (delta block hashes).
#[derive(Default)]
struct IdHash(u64);

impl std::hash::Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 << 8) ^ u64::from(x);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

const DELTA_BLOCK: usize = 16;
/// Longest delta chain we emit (git's default `pack.depth` is 50).
const MAX_SEND_DEPTH: u32 = 50;
/// Objects larger than this are always sent whole.
const MAX_DELTA_INPUT: usize = 8 << 20;
/// Budget for delta-base bodies kept while writing one pack.
const SEND_BASE_CACHE: usize = 256 << 20;

fn block_hash(b: &[u8]) -> u64 {
    let a = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    let c = u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
    (a ^ c.rotate_left(29)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn push_varint(out: &mut Vec<u8>, mut v: usize) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn push_insert(out: &mut Vec<u8>, data: &[u8]) {
    for c in data.chunks(127) {
        out.push(c.len() as u8);
        out.extend_from_slice(c);
    }
}

fn push_copy(out: &mut Vec<u8>, mut off: usize, mut len: usize) {
    while len > 0 {
        let n = len.min(0xff_ffff);
        let mut op = 0x80u8;
        let mut args = Vec::with_capacity(7);
        for i in 0..4 {
            let b = ((off >> (8 * i)) & 0xff) as u8;
            if b != 0 {
                op |= 1 << i;
                args.push(b);
            }
        }
        for i in 0..3 {
            let b = ((n >> (8 * i)) & 0xff) as u8;
            if b != 0 {
                op |= 0x10 << i;
                args.push(b);
            }
        }
        out.push(op);
        out.extend_from_slice(&args);
        off += n;
        len -= n;
    }
}

/// Encodes a git delta turning `base` into `target` (greedy block matching).
pub fn encode_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(target.len() / 4 + 16);
    push_varint(&mut out, base.len());
    push_varint(&mut out, target.len());
    if base.len() < DELTA_BLOCK || target.len() < DELTA_BLOCK {
        push_insert(&mut out, target);
        return out;
    }
    let mut index: HashMap<u64, usize, std::hash::BuildHasherDefault<IdHash>> =
        HashMap::with_capacity_and_hasher(base.len() / DELTA_BLOCK + 1, Default::default());
    let mut p = 0;
    while p + DELTA_BLOCK <= base.len() {
        index.entry(block_hash(&base[p..p + DELTA_BLOCK])).or_insert(p);
        p += DELTA_BLOCK;
    }
    let (mut i, mut pending) = (0usize, 0usize);
    while i + DELTA_BLOCK <= target.len() {
        let hit = index.get(&block_hash(&target[i..i + DELTA_BLOCK])).copied();
        match hit {
            Some(mut b) if base[b..b + DELTA_BLOCK] == target[i..i + DELTA_BLOCK] => {
                let mut len = DELTA_BLOCK;
                while b + len < base.len() && i + len < target.len() && base[b + len] == target[i + len] {
                    len += 1;
                }
                while i > pending && b > 0 && base[b - 1] == target[i - 1] {
                    i -= 1;
                    b -= 1;
                    len += 1;
                }
                push_insert(&mut out, &target[pending..i]);
                push_copy(&mut out, b, len);
                i += len;
                pending = i;
            }
            _ => i += 1,
        }
    }
    push_insert(&mut out, &target[pending..]);
    out
}

/// Encodes an OFS_DELTA back-distance (inverse of the decoder in `read_entry_header`).
fn ofs_encode(mut off: u64) -> Vec<u8> {
    let mut b = vec![(off & 0x7f) as u8];
    off >>= 7;
    while off > 0 {
        off -= 1;
        b.push(0x80 | (off & 0x7f) as u8);
        off >>= 7;
    }
    b.reverse();
    b
}

/// What the client already has, cheaply (git's no-bitmap "edge" strategy):
/// every commit reachable from `common` (commit objects only), plus the full
/// trees of up to [`MAX_BOUNDARY`] common commits. Objects outside this set
/// that the client has anyway may be re-sent, which is harmless.
/// Fetch options beyond wants/haves (protocol v2 `fetch` arguments).
#[derive(Default, Clone)]
pub(crate) struct FetchOpts {
    /// `deepen N`: commits at most N deep from the wants.
    depth: Option<u32>,
    /// `deepen-since <t>`: commits at least this new (the wants themselves always).
    since: Option<u64>,
    /// `shallow <oid>`: the client's current shallow boundary.
    client_shallow: HashSet<Oid>,
    /// `filter blob:none` / `blob:limit=<n>`: omit blobs larger than this.
    blob_limit: Option<u64>,
}

impl FetchOpts {
    fn deepens(&self) -> bool {
        self.depth.is_some() || self.since.is_some()
    }
}

/// Parses `blob:none` / `blob:limit=<n>[kmg]`.
fn parse_filter(spec: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(spec).map_err(|_| Error::Parse("bad filter".into()))?.trim();
    if s == "blob:none" {
        return Ok(0);
    }
    if let Some(n) = s.strip_prefix("blob:limit=") {
        let (num, mul) = match n.as_bytes().last() {
            Some(b'k' | b'K') => (&n[..n.len() - 1], 1u64 << 10),
            Some(b'm' | b'M') => (&n[..n.len() - 1], 1 << 20),
            Some(b'g' | b'G') => (&n[..n.len() - 1], 1 << 30),
            _ => (n, 1),
        };
        return num.parse::<u64>().map(|v| v.saturating_mul(mul)).map_err(|_| Error::Parse(format!("bad filter {s}")));
    }
    Err(Error::Unsupported("only blob:none and blob:limit=<n> filters are supported"))
}

/// The commits a shallow fetch sends, and its new shallow boundary (included commits whose
/// parents are not all included).
fn shallow_cut(view: &View<'_>, wants: &[Oid], opts: &FetchOpts) -> Result<(HashSet<Oid>, Vec<Oid>)> {
    let mut included: HashSet<Oid> = HashSet::new();
    let mut boundary = Vec::new();
    let mut queue: std::collections::VecDeque<(Oid, u32)> = std::collections::VecDeque::new();
    for w in wants {
        // Peel tags to commits; non-commit wants (blobs, trees) need no cut.
        let mut o = *w;
        for _ in 0..8 {
            match view.load(&o)? {
                Some((Kind::Tag, body)) => match view.refs(Kind::Tag, &body)?.first() {
                    Some(t) => o = *t,
                    None => break,
                },
                _ => break,
            }
        }
        queue.push_back((o, 1));
    }
    while let Some((c, d)) = queue.pop_front() {
        let Some((Kind::Commit, body)) = view.load(&c)? else { continue };
        if !included.insert(c) {
            continue;
        }
        let parents: Vec<Oid> = view.refs(Kind::Commit, &body)?.into_iter().skip(1).collect();
        let mut cut = opts.depth.is_some_and(|n| d >= n);
        let mut kept = Vec::new();
        for p in &parents {
            if cut {
                break;
            }
            if let Some(t) = opts.since
                && let Some((Kind::Commit, pb)) = view.load(p)?
                && commit_time(&pb) < t
            {
                cut = true;
                break;
            }
            kept.push(*p);
        }
        if cut && !parents.is_empty() {
            boundary.push(c);
        } else {
            for p in kept {
                queue.push_back((p, d + 1));
            }
        }
    }
    boundary.sort();
    boundary.dedup();
    Ok((included, boundary))
}

/// Depth (from the wants, wants = 1) of the deepest of the client's shallow commits.
fn shallow_depth(view: &View<'_>, wants: &[Oid], shallow: &HashSet<Oid>) -> Result<u32> {
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut queue: std::collections::VecDeque<(Oid, u32)> = wants.iter().map(|w| (*w, 1)).collect();
    let mut deepest = 0;
    while let Some((c, d)) = queue.pop_front() {
        if !seen.insert(c) {
            continue;
        }
        if shallow.contains(&c) {
            deepest = deepest.max(d);
            continue; // the client has nothing behind it
        }
        if let Some((Kind::Commit, body)) = view.load(&c)? {
            for p in view.refs(Kind::Commit, &body)?.into_iter().skip(1) {
                queue.push_back((p, d + 1));
            }
        }
    }
    Ok(deepest)
}

/// Committer time of a commit body (seconds; 0 if unparsable).
fn commit_time(body: &[u8]) -> u64 {
    String::from_utf8_lossy(body)
        .lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.strip_prefix("committer ").map(str::to_string))
        .and_then(|l| l.rsplit(' ').nth(1).and_then(|t| t.parse().ok()))
        .unwrap_or(0)
}

fn boundary_exclude(view: &View<'_>, common: &[Oid], stop: &HashSet<Oid>) -> Result<HashSet<Oid>> {
    let mut ex: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = common.to_vec();
    while let Some(o) = stack.pop() {
        if !ex.insert(o) {
            continue;
        }
        // The client has no history behind its shallow commits.
        if !stop.contains(&o)
            && let Some((Kind::Commit, body)) = view.load(&o)?
        {
            stack.extend(view.refs(Kind::Commit, &body)?.into_iter().skip(1));
        }
    }
    let commits: HashSet<Oid> = ex.clone();
    for c in common.iter().take(MAX_BOUNDARY) {
        if let Some((Kind::Commit, body)) = view.load(c)?
            && let Some(tree) = view.refs(Kind::Commit, &body)?.first().copied()
        {
            let got = walk(view, &[tree], &ex, |_, _, _| Ok(()))?;
            ex.extend(got);
        }
    }
    debug_assert!(commits.is_subset(&ex));
    Ok(ex)
}

const MAX_BOUNDARY: usize = 16;

/// Objects to send with the path they were reached by (for delta pairing):
/// reachable from `tips`, minus `exclude`. Newest history first.
fn walk_paths(view: &View<'_>, tips: &[Oid], exclude: &HashSet<Oid>) -> Result<Vec<Walked>> {
    walk_paths_with(view, tips, exclude, None, None)
}

/// An object to send: its oid, kind, the path it was reached by (for delta pairing), the stored
/// object and its body length if recorded.
pub(crate) struct Walked {
    pub(crate) oid: Oid,
    pub(crate) kind: Kind,
    pub(crate) path: String,
    pub(crate) hash: Hash,
    pub(crate) len: Option<u64>,
}

/// [`walk_paths`] limited to the `commits` set (shallow) and omitting blobs over `blob_limit`
/// bytes (partial clone). Blobs named directly as tips are always sent.
///
/// The walk goes in breadth-first waves with one batched oid-map lookup per wave: commits (and
/// tags) generation by generation from the tips, then trees level by level, then all blobs.
/// Blob bodies aren't read here. Within a kind, newer history comes first, so each path's
/// newest version precedes its older ones (they become deltas against it).
fn walk_paths_with(
    view: &View<'_>,
    tips: &[Oid],
    exclude: &HashSet<Oid>,
    commits: Option<&HashSet<Oid>>,
    blob_limit: Option<u64>,
) -> Result<Vec<Walked>> {
    let tip_set: HashSet<Oid> = tips.iter().copied().collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut out: Vec<Walked> = Vec::new();
    // Oids of one wave, deduplicated against everything seen; looked up together.
    let take = |wave: Vec<(Oid, String)>, seen: &mut HashSet<Oid>| -> Result<Vec<WaveItem>> {
        let wave: Vec<(Oid, String)> =
            wave.into_iter().filter(|(o, _)| !exclude.contains(o) && seen.insert(*o)).collect();
        let oids: Vec<Oid> = wave.iter().map(|w| w.0).collect();
        let found = view.lookup_many_len(&oids)?;
        wave.into_iter()
            .zip(found)
            .map(|((o, p), f)| {
                let (k, h, len) = f.ok_or_else(|| Error::Corrupt(format!("missing git object {}", hex(&o))))?;
                Ok((o, p, k, h, len))
            })
            .collect()
    };
    let load_all = |items: &[WaveItem]| -> Result<Vec<Vec<u8>>> {
        par_map(items.len(), workers, |j| {
            let (o, _, k, h, _) = &items[j];
            view.load_stored(*k, h)?.ok_or_else(|| Error::Corrupt(format!("missing git object {}", hex(o))))
        })
    };
    let mut trees: Vec<(Oid, String)> = Vec::new();
    let mut blobs: Vec<(Oid, String)> = Vec::new();
    // History found by the waves: object -> (its entry, root tree if a commit, next objects).
    let mut hist: HashMap<Oid, (Walked, Option<Oid>, Vec<Oid>)> = HashMap::new();
    // 1. Commits and tags, generation by generation (batched lookups and loads).
    let mut wave: Vec<(Oid, String)> = tips.iter().map(|o| (*o, String::new())).collect();
    while !wave.is_empty() {
        let items = take(std::mem::take(&mut wave), &mut seen)?;
        let (history, other): (Vec<_>, Vec<_>) =
            items.into_iter().partition(|x| matches!(x.2, Kind::Commit | Kind::Tag));
        for (o, p, k, ..) in other {
            // A tree or blob named directly (a tip or a tag target): walked in its own phase.
            seen.remove(&o);
            match k {
                Kind::Tree => trees.push((o, p)),
                _ => blobs.push((o, p)),
            }
        }
        let bodies = load_all(&history)?;
        for ((o, p, k, h, len), body) in history.into_iter().zip(bodies) {
            let refs = view.refs(k, &body)?;
            let (tree, next): (Option<Oid>, Vec<Oid>) = match k {
                Kind::Commit => (
                    refs.first().copied(),
                    refs.iter().skip(1).filter(|par| commits.is_none_or(|c| c.contains(*par))).copied().collect(),
                ),
                _ => (None, refs),
            };
            wave.extend(next.iter().map(|r| (*r, String::new())));
            hist.insert(o, (Walked { oid: o, kind: k, path: p, hash: h, len }, tree, next));
        }
    }
    // Emit history in depth-first order from the tips (first parent first), as git does: each
    // path's versions then follow one line of history, which keeps "previous version at this
    // path" a good delta base. (Breadth-first generations across many tips interleave eras.)
    let mut stack: Vec<Oid> = tips.iter().rev().copied().collect();
    let mut done: HashSet<Oid> = HashSet::new();
    let mut root_trees: Vec<(Oid, String)> = Vec::new();
    while let Some(o) = stack.pop() {
        if !done.insert(o) {
            continue;
        }
        let Some((w, tree, next)) = hist.remove(&o) else { continue };
        if let Some(t) = tree {
            root_trees.push((t, String::new()));
        }
        stack.extend(next.iter().rev().copied());
        out.push(w);
    }
    // Trees named directly (tips, tag targets) first, then commits' root trees in that order.
    trees.extend(root_trees);
    // 2. Trees, level by level.
    let mut level = trees;
    while !level.is_empty() {
        let items = take(std::mem::take(&mut level), &mut seen)?;
        let bodies = load_all(&items)?;
        for ((oid, path, kind, h, len), body) in items.into_iter().zip(bodies) {
            if kind != Kind::Tree {
                // Recorded under another kind than its tree entry says: send it as what it is.
                seen.remove(&oid);
                blobs.push((oid, path));
                continue;
            }
            let mut i = 0;
            while i < body.len() {
                let Some(sp) = body[i..].iter().position(|&c| c == b' ').map(|x| x + i) else { break };
                let Some(nul) = body[sp..].iter().position(|&c| c == 0).map(|x| x + sp) else { break };
                // Entries hash like the tree itself (SHA-1 or SHA-256 repository).
                let hl = oid.len();
                if nul + 1 + hl > body.len() {
                    break;
                }
                let mode = &body[i..sp];
                let Some(o) = Oid::from_slice(&body[nul + 1..nul + 1 + hl]) else { break };
                if mode != b"160000" {
                    let name = String::from_utf8_lossy(&body[sp + 1..nul]);
                    let p = if path.is_empty() { name.to_string() } else { format!("{path}/{name}") };
                    if mode == b"40000" { level.push((o, p)) } else { blobs.push((o, p)) }
                }
                i = nul + 1 + hl;
            }
            out.push(Walked { oid, kind, path, hash: h, len });
        }
    }
    // 3. Blobs (bodies are read when the pack is written).
    for (oid, path, kind, hash, len) in take(blobs, &mut seen)? {
        if kind == Kind::Tree {
            return Err(Error::Corrupt(format!("tree {} listed as a blob", hex(&oid))));
        }
        if let Some(limit) = blob_limit
            && kind == Kind::Blob
            && !tip_set.contains(&oid)
            && len.is_some_and(|n| n > limit)
        {
            continue; // filtered out (partial clone)
        }
        out.push(Walked { oid, kind, path, hash, len });
    }
    Ok(out)
}

fn pack_entry_header(kind: Kind, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut c = ((kind as u8) << 4) | (size & 15) as u8;
    let mut rest = size >> 4;
    while rest > 0 {
        out.push(c | 0x80);
        c = (rest & 0x7f) as u8;
        rest >>= 7;
    }
    out.push(c);
    out
}

/// Every object reachable from `tips`, skipping `exclude` (and what it reaches).
fn walk(
    view: &View<'_>,
    tips: &[Oid],
    exclude: &HashSet<Oid>,
    mut visit: impl FnMut(Oid, Kind, &[u8]) -> Result<()>,
) -> Result<HashSet<Oid>> {
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = tips.iter().filter(|o| !exclude.contains(*o)).copied().collect();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let (kind, body) =
            view.load(&oid)?.ok_or_else(|| Error::Corrupt(format!("missing git object {}", hex(&oid))))?;
        for child in view.refs(kind, &body)? {
            if !seen.contains(&child) && !exclude.contains(&child) {
                stack.push(child);
            }
        }
        visit(oid, kind, &body)?;
    }
    Ok(seen)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn git_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

fn no_cache(h: &mut HeaderMap) {
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, max-age=0, must-revalidate"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// `/git/{*rest}` entry point.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    method: Method,
    Path(rest): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(git) = st.git.clone() else { return git_error(StatusCode::NOT_FOUND, "git is disabled") };
    let (repo, action) = match rest.rsplit_once('/') {
        Some((r, "git-upload-pack")) => (r, "upload"),
        Some((r, "git-receive-pack")) => (r, "receive"),
        Some((r, "refs")) if r.ends_with("/info") => (&r[..r.len() - 5], "refs"),
        _ => return git_error(StatusCode::NOT_FOUND, "unknown git endpoint"),
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo).to_string();
    if repo.is_empty()
        || repo.starts_with('_')
        || pico2peta_core::validate_path(&repo).is_err()
        || repo.split('/').any(|s| s.starts_with('_'))
    {
        return git_error(StatusCode::BAD_REQUEST, "invalid repository name");
    }
    let service = match action {
        "refs" => match q.get("service").map(String::as_str) {
            Some(s @ ("git-upload-pack" | "git-receive-pack")) => s.to_string(),
            _ => return git_error(StatusCode::FORBIDDEN, "dumb HTTP is not supported; use a smart git client"),
        },
        "upload" => "git-upload-pack".into(),
        _ => "git-receive-pack".into(),
    };
    let need = if service == "git-receive-pack" { Access::Write } else { Access::Read };
    if !st.authz.allows(&p, &ns(&repo), need) {
        st.metrics.denied.inc();
        st.audit.record_nowait(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "git",
            target: ns(&repo),
            outcome: "denied".into(),
            detail: Some(service.clone()),
            remote: c.remote.clone(),
        });
        return git_error(StatusCode::FORBIDDEN, format!("{} may not access {repo}", p.name));
    }
    if let Err(e) = st.admit(&p, &ns(&repo), need == Access::Write) {
        return e.into_response();
    }
    match (method, action) {
        (Method::GET, "refs") => {
            let g = git.clone();
            let r = repo.clone();
            let s2 = service.clone();
            let v2 = service == "git-upload-pack" && wants_v2(&headers);
            match tokio::task::spawn_blocking(move || {
                if v2 { Ok(v2_capabilities(g.fmt_for(&r))) } else { advertisement(&g, &r, &s2) }
            })
            .await
            {
                Ok(Ok(body)) => {
                    let mut resp = Response::new(Body::from(body));
                    if let Ok(v) = HeaderValue::from_str(&format!("application/x-{service}-advertisement")) {
                        resp.headers_mut().insert(header::CONTENT_TYPE, v);
                    }
                    no_cache(resp.headers_mut());
                    resp
                }
                Ok(Err(e)) => git_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                Err(e) => git_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
        (Method::POST, "upload") => upload_pack(&st, git, repo, &headers, body).await,
        (Method::POST, "receive") => {
            if !st.audit.is_healthy() {
                return git_error(StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable; refusing pushes");
            }
            receive_pack(&st, git, &p, &c, repo, &headers, body).await
        }
        _ => git_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// Reads a (possibly gzip-encoded) request body into memory, up to `limit`.
async fn small_body(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
) -> std::result::Result<Vec<u8>, (StatusCode, String)> {
    let too_large = || (StatusCode::PAYLOAD_TOO_LARGE, "request too large".to_string());
    let raw = axum::body::to_bytes(body, limit).await.map_err(|_| too_large())?;
    let gz = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    if !gz {
        return Ok(raw.to_vec());
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&raw[..])
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad gzip body: {e}")))?;
    if out.len() > limit {
        return Err(too_large());
    }
    Ok(out)
}

struct UploadRequest {
    wants: Vec<Oid>,
    haves: Vec<Oid>,
    done: bool,
    sideband: bool,
}

fn parse_upload_request(body: &[u8]) -> Result<UploadRequest> {
    let mut r = body;
    let mut req = UploadRequest { wants: Vec::new(), haves: Vec::new(), done: false, sideband: false };
    let mut first = true;
    loop {
        let line = match read_pkt(&mut r) {
            Ok(Some(l)) => l,
            Ok(None) => {
                if r.is_empty() {
                    break;
                }
                continue;
            }
            Err(_) if r.is_empty() => break,
            Err(e) => return Err(e),
        };
        let line = trim_nl(&line);
        if let Some(rest) = line.strip_prefix(b"want ") {
            let hexlen = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            let oid = parse_oid(&rest[..hexlen]).ok_or_else(|| Error::Parse("bad want".into()))?;
            if first {
                let caps = String::from_utf8_lossy(&rest[hexlen..]).to_string();
                req.sideband = caps.split(' ').any(|c| c == "side-band-64k" || c == "side-band");
                first = false;
            }
            req.wants.push(oid);
        } else if let Some(rest) = line.strip_prefix(b"have ") {
            let hexlen = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            req.haves.push(parse_oid(&rest[..hexlen]).ok_or_else(|| Error::Parse("bad have".into()))?);
        } else if line == b"done" {
            req.done = true;
            break;
        } else if line.starts_with(b"shallow ") || line.starts_with(b"deepen") || line.starts_with(b"filter ") {
            return Err(Error::Unsupported("shallow and partial clones are not supported"));
        }
    }
    Ok(req)
}

/// The client asked for protocol v2 (`Git-Protocol: version=2`).
fn wants_v2(h: &HeaderMap) -> bool {
    h.get("git-protocol").and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(':').any(|p| p.trim() == "version=2"))
}

async fn upload_pack(
    st: &Arc<AppState>,
    git: Arc<GitState>,
    repo: String,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let body = match small_body(headers, body, 64 << 20).await {
        Ok(b) => b,
        Err((status, msg)) => return git_error(status, msg),
    };
    if wants_v2(headers) {
        let req = match parse_v2_request(&body) {
            Ok(r) => r,
            Err(e) => return git_error(StatusCode::BAD_REQUEST, e.to_string()),
        };
        let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(8);
        let metrics = st.metrics.clone();
        tokio::task::spawn_blocking(move || {
            let res = match req.command.as_str() {
                _ if req.object_format.as_deref().is_some_and(|f| f != git.fmt_for(&repo).name()) => {
                    Err(Error::Parse(format!("this repository uses object-format={}", git.fmt_for(&repo).name())))
                }
                "ls-refs" => v2_ls_refs(&git, &repo, &req.args).and_then(|out| send(&tx, out)),
                "fetch" => v2_fetch(&git, &repo, &req.args, &tx, &metrics),
                other => Err(Error::Parse(format!("unknown v2 command {other:?}"))),
            };
            if let Err(e) = res {
                warn!(repo, error = %e, "upload-pack v2 failed");
                let mut out = Vec::new();
                sideband(&mut out, 3, format!("ERR {e}\n").as_bytes(), true);
                let _ = tx.blocking_send(Ok(Bytes::from(out)));
            }
        });
        let mut resp = Response::new(Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|i| (i, rx))
        })));
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-git-upload-pack-result"));
        no_cache(resp.headers_mut());
        return resp;
    }
    let req = match parse_upload_request(&body) {
        Ok(r) => r,
        Err(e) => return git_error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(8);
    let metrics = st.metrics.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = serve_upload(&git, &repo, &req, &tx, &metrics) {
            warn!(repo, error = %e, "upload-pack failed");
            let mut out = Vec::new();
            let msg = format!("ERR {e}\n");
            if req.sideband {
                sideband(&mut out, 3, msg.as_bytes(), true);
            } else {
                out.extend_from_slice(&pkt(msg.as_bytes()));
            }
            let _ = tx.blocking_send(Ok(Bytes::from(out)));
        }
    });
    let mut resp = Response::new(Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|i| (i, rx))
    })));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-git-upload-pack-result"));
    no_cache(resp.headers_mut());
    resp
}

/// Sends a chunk to the client, giving up if it stalls for [`IDLE_TIMEOUT`].
fn send(tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>, data: Vec<u8>) -> Result<()> {
    let deadline = std::time::Instant::now() + IDLE_TIMEOUT;
    let mut item = Ok(Bytes::from(data));
    loop {
        match tx.try_send(item) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(Error::Io(std::io::Error::other("client went away")));
            }
            Err(mpsc::error::TrySendError::Full(i)) => {
                if std::time::Instant::now() > deadline {
                    return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "client stalled")));
                }
                item = i;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// Validates wants (must be tips of `repo`'s refs) and finds the common commits:
/// haves reachable from this repository (never other repos' objects).
fn negotiate(view: &View<'_>, git: &GitState, repo: &str, wants: &[Oid], haves: &[Oid]) -> Result<Vec<Oid>> {
    let refs = list_refs(git, repo)?;
    let tips: HashSet<Oid> = refs.iter().map(|r| r.1).collect();
    // Non-tip wants (a partial clone fetching missing blobs, or a shallow client deepening) are
    // allowed when reachable from this repo's refs: object ids are global across repos.
    let others: Vec<&Oid> = wants.iter().filter(|w| !tips.contains(*w)).collect();
    if !others.is_empty() {
        let tip_list: Vec<Oid> = tips.iter().copied().collect();
        let reachable: HashSet<Oid> =
            walk_paths(view, &tip_list, &HashSet::new())?.into_iter().map(|o| o.oid).collect();
        if let Some(w) = others.into_iter().find(|w| !reachable.contains(*w)) {
            return Err(Error::Parse(format!("not our ref {}", hex(w))));
        }
    }
    let mut common: Vec<Oid> = Vec::new();
    if !haves.is_empty() {
        let mut repo_commits: HashSet<Oid> = HashSet::new();
        let mut stack: Vec<Oid> = tips.iter().copied().collect();
        let haves: HashSet<Oid> = haves.iter().copied().collect();
        while let Some(o) = stack.pop() {
            if !repo_commits.insert(o) {
                continue;
            }
            let Some((kind, body)) = view.load(&o)? else { continue };
            match kind {
                Kind::Commit => {
                    for (i, child) in view.refs(kind, &body)?.into_iter().enumerate() {
                        if i > 0 {
                            stack.push(child); // parents only (index 0 is the tree)
                        }
                    }
                }
                Kind::Tag => stack.extend(view.refs(kind, &body)?),
                _ => {}
            }
            if haves.len() == common.len() {
                break;
            }
            if haves.contains(&o) {
                common.push(o);
            }
        }
        common.sort();
        common.dedup();
    }
    Ok(common)
}

// ---------------------------------------------------------------------------
// Protocol v2 (upload-pack only; pushes use v0/v1, as git itself does)
// ---------------------------------------------------------------------------

/// The v2 capability advertisement (no `# service` line in v2).
fn v2_capabilities(fmt: Fmt) -> Vec<u8> {
    let mut out = Vec::new();
    let of = format!("object-format={}\n", fmt.name());
    for l in ["version 2\n", &format!("{AGENT}\n"), "ls-refs=unborn\n", "fetch=shallow filter\n", &of] {
        out.extend_from_slice(&pkt(l.as_bytes()));
    }
    out.extend_from_slice(FLUSH);
    out
}

struct V2Request {
    command: String,
    args: Vec<Vec<u8>>,
    /// `object-format=` the client asked for (checked against the repository's).
    object_format: Option<String>,
}

fn parse_v2_request(body: &[u8]) -> Result<V2Request> {
    let mut r = body;
    let mut command = None;
    let mut args = Vec::new();
    let mut object_format = None;
    let mut in_args = false;
    loop {
        match read_pkt(&mut r)? {
            None => break,                             // flush ends the request
            Some(l) if l.is_empty() => in_args = true, // delim
            Some(l) => {
                let l = trim_nl(&l).to_vec();
                if in_args {
                    args.push(l);
                } else if let Some(c) = l.strip_prefix(b"command=") {
                    command = Some(String::from_utf8_lossy(c).to_string());
                } else if let Some(f) = l.strip_prefix(b"object-format=") {
                    object_format = Some(String::from_utf8_lossy(f).into_owned());
                }
            }
        }
    }
    Ok(V2Request {
        command: command.ok_or_else(|| Error::Parse("v2 request without command".into()))?,
        args,
        object_format,
    })
}

fn v2_ls_refs(git: &GitState, repo: &str, args: &[Vec<u8>]) -> Result<Vec<u8>> {
    let view = View::for_repo(git, repo)?;
    let refs = list_refs(git, repo)?;
    let has = |a: &[u8]| args.iter().any(|x| x == a);
    let (peel, symrefs, unborn) = (has(b"peel"), has(b"symrefs"), has(b"unborn"));
    let prefixes: Vec<&[u8]> = args.iter().filter_map(|a| a.strip_prefix(b"ref-prefix ")).collect();
    let wanted = |name: &str| prefixes.is_empty() || prefixes.iter().any(|p| name.as_bytes().starts_with(p));
    let mut out = Vec::new();
    match head_target(&refs) {
        Some(h) if wanted("HEAD") => {
            let mut l = format!("{} HEAD", hex(&h.1));
            if symrefs {
                l.push_str(&format!(" symref-target:{}", h.0));
            }
            out.extend_from_slice(&pkt(format!("{l}\n").as_bytes()));
        }
        None if unborn && wanted("HEAD") => {
            out.extend_from_slice(&pkt(b"unborn HEAD symref-target:refs/heads/main\n"));
        }
        _ => {}
    }
    for (name, oid, _) in &refs {
        if !wanted(name) {
            continue;
        }
        let mut l = format!("{} {name}", hex(oid));
        if peel && let Some((Kind::Tag, body)) = view.load(oid)? {
            // Peel through nested tags to the final object.
            let mut target = view.refs(Kind::Tag, &body)?.first().copied();
            for _ in 0..16 {
                match target.map(|t| view.load(&t)).transpose()?.flatten() {
                    Some((Kind::Tag, b)) => target = view.refs(Kind::Tag, &b)?.first().copied(),
                    _ => break,
                }
            }
            if let Some(t) = target {
                l.push_str(&format!(" peeled:{}", hex(&t)));
            }
        }
        out.extend_from_slice(&pkt(format!("{l}\n").as_bytes()));
    }
    out.extend_from_slice(FLUSH);
    Ok(out)
}

fn v2_fetch(
    git: &GitState,
    repo: &str,
    args: &[Vec<u8>],
    tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    metrics: &crate::metrics::Metrics,
) -> Result<()> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut done = false;
    let mut opts = FetchOpts::default();
    let mut relative = false;
    let oid_arg = |o: &[u8], what: &str| {
        // 40 (SHA-1) or 64 (SHA-256) hex digits, up to the first space.
        parse_oid(o.split(|&b| b == b' ').next().unwrap_or_default()).ok_or_else(|| Error::Parse(format!("bad {what}")))
    };
    for a in args {
        if let Some(o) = a.strip_prefix(b"want ") {
            wants.push(oid_arg(o, "want")?);
        } else if let Some(o) = a.strip_prefix(b"have ") {
            haves.push(oid_arg(o, "have")?);
        } else if a == b"done" {
            done = true;
        } else if let Some(o) = a.strip_prefix(b"shallow ") {
            opts.client_shallow.insert(oid_arg(o, "shallow")?);
        } else if let Some(n) = a.strip_prefix(b"deepen ") {
            let n: u32 = std::str::from_utf8(n)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .ok_or_else(|| Error::Parse("bad deepen".into()))?;
            if n == 0 {
                return Err(Error::Parse("deepen 0".into()));
            }
            opts.depth = Some(n.min(1 << 20));
        } else if let Some(t) = a.strip_prefix(b"deepen-since ") {
            opts.since = Some(
                std::str::from_utf8(t)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .ok_or_else(|| Error::Parse("bad deepen-since".into()))?,
            );
        } else if let Some(f) = a.strip_prefix(b"filter ") {
            opts.blob_limit = Some(parse_filter(f)?);
        } else if a == b"deepen-relative" {
            relative = true;
        } else if a.starts_with(b"deepen-not ") || a.starts_with(b"want-ref ") {
            return Err(Error::Unsupported("deepen-not and want-ref are not supported"));
        }
    }
    if wants.is_empty() {
        return Err(Error::Parse("fetch without wants".into()));
    }
    let view = View::for_repo(git, repo)?;
    let common = negotiate(&view, git, repo, &wants, &haves)?;
    if relative && let Some(n) = opts.depth {
        // `--deepen N`: N more commits behind the client's current boundary.
        opts.depth = Some(shallow_depth(&view, &wants, &opts.client_shallow)?.saturating_add(n));
    }
    if !done {
        let mut out = pkt(b"acknowledgments\n");
        if common.is_empty() {
            out.extend_from_slice(&pkt(b"NAK\n"));
        }
        for c in &common {
            out.extend_from_slice(&pkt(format!("ACK {}\n", hex(c)).as_bytes()));
        }
        if common.is_empty() {
            out.extend_from_slice(FLUSH);
            return send(tx, out);
        }
        // We have enough to build a pack: say so and send it right away.
        out.extend_from_slice(&pkt(b"ready\n"));
        out.extend_from_slice(b"0001");
        send(tx, out)?;
    }
    // Shallow: which commits to send, and the client's new boundary.
    let mut commits = None;
    if opts.deepens() || !opts.client_shallow.is_empty() {
        let mut info = pkt(b"shallow-info\n");
        if opts.deepens() {
            let (inc, boundary) = shallow_cut(&view, &wants, &opts)?;
            for b in &boundary {
                info.extend_from_slice(&pkt(format!("shallow {}\n", hex(b)).as_bytes()));
            }
            // Previously shallow commits whose parents are now sent.
            for c in &opts.client_shallow {
                if inc.contains(c) && !boundary.contains(c) {
                    info.extend_from_slice(&pkt(format!("unshallow {}\n", hex(c)).as_bytes()));
                }
            }
            commits = Some(inc);
        }
        info.extend_from_slice(b"0001");
        send(tx, info)?;
    }
    send(tx, pkt(b"packfile\n"))?;
    write_pack(&view, repo, &wants, &common, &opts, commits.as_ref(), true, tx, metrics)?;
    send(tx, FLUSH.to_vec())
}

fn serve_upload(
    git: &GitState,
    repo: &str,
    req: &UploadRequest,
    tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    metrics: &crate::metrics::Metrics,
) -> Result<()> {
    let view = View::for_repo(git, repo)?;
    let common = negotiate(&view, git, repo, &req.wants, &req.haves)?;
    let mut out = Vec::new();
    if let Some(first) = common.first() {
        out.extend_from_slice(&pkt(format!("ACK {}\n", hex(first)).as_bytes()));
    }
    if !req.done {
        if common.is_empty() {
            out.extend_from_slice(&pkt(b"NAK\n"));
        }
        return send(tx, out);
    }
    if common.is_empty() {
        out.extend_from_slice(&pkt(b"NAK\n"));
    }
    send(tx, out)?;
    write_pack(&view, repo, &req.wants, &common, &FetchOpts::default(), None, req.sideband, tx, metrics)?;
    if req.sideband {
        send(tx, FLUSH.to_vec())?;
    }
    Ok(())
}

/// zlib for pack entries. Level 1: served packs are built per request, so speed wins over the
/// last few percent of size.
fn zlib(data: &[u8]) -> Result<Vec<u8>> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::with_capacity(data.len() / 2 + 64), flate2::Compression::new(1));
    z.write_all(data)?;
    Ok(z.finish()?)
}

/// `f(0..n)` on up to `workers` threads, results in order.
fn par_map<T: Send>(n: usize, workers: usize, f: impl Fn(usize) -> Result<T> + Sync) -> Result<Vec<T>> {
    if workers <= 1 || n < 8 {
        return (0..n).map(&f).collect();
    }
    let per = n.div_ceil(workers);
    let parts: Vec<Result<Vec<T>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..n)
            .step_by(per)
            .map(|a| {
                let f = &f;
                sc.spawn(move || (a..(a + per).min(n)).map(f).collect::<Result<Vec<T>>>())
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(Error::Corrupt("pack worker panicked".into()))))
            .collect()
    });
    let mut out = Vec::with_capacity(n);
    for p in parts {
        out.extend(p?);
    }
    Ok(out)
}

/// Streams a pack of everything reachable from `wants` but not from `common`.
#[allow(clippy::too_many_arguments)]
fn write_pack(
    view: &View<'_>,
    repo: &str,
    wants: &[Oid],
    common: &[Oid],
    opts: &FetchOpts,
    commits: Option<&HashSet<Oid>>,
    sideband_on: bool,
    tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    metrics: &crate::metrics::Metrics,
) -> Result<()> {
    // Objects to send: reachable from wants minus reachable from common.
    let exclude =
        if common.is_empty() { HashSet::new() } else { boundary_exclude(view, common, &opts.client_shallow)? };
    // Deepening: the client's shallow commits are excluded (it has them), so start the walk
    // at their parents that are now included as well.
    let mut tips: Vec<Oid> = wants.to_vec();
    if let Some(inc) = commits {
        for c in &opts.client_shallow {
            if let Some((Kind::Commit, body)) = view.load(c)? {
                tips.extend(view.refs(Kind::Commit, &body)?.into_iter().skip(1).filter(|p| inc.contains(p)));
            }
        }
    }
    let objs = walk_paths_with(view, &tips, &exclude, commits, opts.blob_limit)?;
    // Stream the pack. Blobs and trees are deltified against the previously
    // sent version at the same path (newest first, so older versions are deltas).
    let mut h = PackHasher::new(view.fmt);
    let mut buf = Vec::with_capacity(1 << 17);
    let mut header = b"PACK".to_vec();
    header.extend_from_slice(&2u32.to_be_bytes());
    header.extend_from_slice(&(objs.len() as u32).to_be_bytes());
    h.update(&header);
    buf.extend_from_slice(&header);
    let mut offset = header.len() as u64;
    let flush = |buf: &mut Vec<u8>, force: bool| -> Result<()> {
        if buf.len() >= 1 << 16 || (force && !buf.is_empty()) {
            let mut framed = Vec::with_capacity(buf.len() + 64);
            sideband(&mut framed, 1, buf, sideband_on);
            metrics.bytes_out.inc_by(buf.len() as u64);
            buf.clear();
            send(tx, framed)?;
        }
        Ok(())
    };
    // (kind, path) -> the latest object at that path (the next version's delta base).
    let mut bases: HashMap<(Kind, String), SentBase> = HashMap::new();
    let mut base_bytes = 0usize;
    let mut deltas = 0usize;
    let mut offsets: Vec<u64> = Vec::with_capacity(objs.len());
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let mut start = 0;
    while start < objs.len() {
        // A window of objects, bounded by count and by (recorded) body bytes.
        let mut end = start;
        let mut bytes = 0u64;
        while end < objs.len() && end - start < SEND_WINDOW && (end == start || bytes < SEND_WINDOW_BYTES) {
            bytes += objs[end].len.unwrap_or(0);
            end += 1;
        }
        let win = &objs[start..end];
        // 1. Load the bodies (parallel).
        let bodies: Vec<Arc<Vec<u8>>> = par_map(win.len(), workers, |j| {
            let w = &win[j];
            let body = view
                .load_stored(w.kind, &w.hash)?
                .ok_or_else(|| Error::Corrupt(format!("object {} vanished", hex(&w.oid))))?;
            Ok(Arc::new(body))
        })?;
        // 2. Pick delta bases: the previous object at the same path (sequential, cheap). Depth is
        //    predicted as if every delta is taken; a rejected delta only shortens a chain.
        let mut plan: Vec<Option<(usize, Arc<Vec<u8>>)>> = Vec::with_capacity(win.len());
        for (j, Walked { kind, path, .. }) in win.iter().enumerate() {
            let body = &bodies[j];
            let deltable = matches!(kind, Kind::Blob | Kind::Tree) && body.len() >= 64 && body.len() <= MAX_DELTA_INPUT;
            if !deltable || path.is_empty() {
                plan.push(None);
                continue;
            }
            let key = (*kind, path.clone());
            let (base, depth) = match bases.get(&key) {
                Some((bi, bb, bd)) if *bd < MAX_SEND_DEPTH => (Some((*bi, bb.clone())), bd + 1),
                _ => (None, 0),
            };
            plan.push(base);
            let add = body.len();
            let prev = bases.insert(key, (start + j, body.clone(), depth));
            base_bytes = base_bytes + add - prev.map_or(0, |p| p.1.len());
            if base_bytes > SEND_BASE_CACHE {
                // Over budget: stop remembering new bases (existing ones keep working).
                bases.retain(|_, v| v.2 > 0);
                base_bytes = bases.values().map(|v| v.1.len()).sum();
            }
        }
        // 3. Delta-encode and compress (parallel).
        let encoded: Vec<(Option<usize>, Vec<u8>, usize)> = par_map(win.len(), workers, |j| {
            let body = &bodies[j];
            if let Some((bi, bb)) = &plan[j] {
                let d = encode_delta(bb, body);
                if d.len() < body.len() / 2 {
                    return Ok((Some(*bi), zlib(&d)?, d.len()));
                }
            }
            Ok((None, zlib(body)?, body.len()))
        })?;
        drop(plan);
        drop(bodies);
        // 4. Write in order.
        for (j, (base, data, raw)) in encoded.into_iter().enumerate() {
            let eh = match base {
                Some(bi) => {
                    deltas += 1;
                    let mut eh = pack_entry_header(Kind::Commit, raw); // type patched below
                    eh[0] = (eh[0] & 0x8f) | (6 << 4);
                    eh.extend_from_slice(&ofs_encode(offset - offsets[bi]));
                    eh
                }
                None => pack_entry_header(win[j].kind, raw),
            };
            offsets.push(offset);
            h.update(&eh);
            h.update(&data);
            offset += (eh.len() + data.len()) as u64;
            buf.extend_from_slice(&eh);
            buf.extend_from_slice(&data);
            flush(&mut buf, false)?;
        }
        start = end;
    }
    let trailer = h.finish();
    let tl = trailer.len() as u64;
    buf.extend_from_slice(&trailer);
    flush(&mut buf, true)?;
    info!(repo, objects = objs.len(), deltas, bytes = offset + tl, "upload-pack served");
    Ok(())
}

struct Command {
    old: Oid,
    new: Oid,
    gitref: String,
}

/// What a push did, for the report and the audit log.
struct PushResult {
    unpack: std::result::Result<usize, String>,
    statuses: Vec<(String, std::result::Result<(), String>)>,
    sideband: bool,
    /// Refs created or moved: (ref, new oid, core hash of the new object).
    applied: Vec<(String, Oid, Hash)>,
}

async fn receive_pack(
    st: &Arc<AppState>,
    git: Arc<GitState>,
    p: &Principal,
    c: &Caller,
    repo: String,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    if headers.get(header::CONTENT_ENCODING).is_some_and(|v| v != "identity") {
        return git_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "compressed push bodies are not supported");
    }
    // Spool the request to disk (bounded memory).
    let path = git.spool.join(format!("{:016x}{:016x}.push", fastrand::u64(..), fastrand::u64(..)));
    let spool = async {
        let mut f = tokio::fs::File::create(&path).await?;
        let mut stream = body.into_data_stream();
        let mut total = 0u64;
        loop {
            match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
                Err(_) => return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "push idle timeout")),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(std::io::Error::other(e.to_string())),
                Ok(Some(Ok(b))) => {
                    total += b.len() as u64;
                    if total > git.max_push_bytes {
                        return Err(std::io::Error::other("push exceeds max_upload_bytes"));
                    }
                    st.metrics.bytes_in.inc_by(b.len() as u64);
                    f.write_all(&b).await?;
                }
            }
        }
        f.flush().await
    };
    if let Err(e) = spool.await {
        let _ = tokio::fs::remove_file(&path).await;
        return git_error(StatusCode::BAD_REQUEST, format!("receiving push: {e}"));
    }
    let g = git.clone();
    let r = repo.clone();
    let p2 = path.clone();
    let res = tokio::task::spawn_blocking(move || process_push(&g, &r, &p2)).await;
    let _ = tokio::fs::remove_file(&path).await;
    let res = match res {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return git_error(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e) => return git_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // Audit before acknowledging.
    let detail = format!(
        "unpack={} {}",
        match &res.unpack {
            Ok(n) => format!("ok({n})"),
            Err(e) => format!("err({e})"),
        },
        res.statuses
            .iter()
            .map(|(r, s)| format!("{r}:{}", if s.is_ok() { "ok" } else { "ng" }))
            .collect::<Vec<_>>()
            .join(",")
    );
    let ok = res.unpack.is_ok() && res.statuses.iter().all(|s| s.1.is_ok());
    if let Err(e) = st
        .audit
        .record(Event {
            request_id: c.request_id.clone(),
            principal: p.name.clone(),
            action: "git_push",
            target: ns(&repo),
            outcome: if ok { "ok".into() } else { "partial".into() },
            detail: Some(detail),
            remote: c.remote.clone(),
        })
        .await
    {
        st.metrics.audit_failures.inc();
        return git_error(StatusCode::INTERNAL_SERVER_ERROR, format!("push applied but audit write failed: {e}"));
    }
    let items = res
        .applied
        .iter()
        .map(|(gitref, oid, h)| {
            let meta = [("repo", repo.clone()), ("ref", gitref.clone()), ("oid", hex(oid))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            (*h, meta)
        })
        .collect();
    crate::prov_api::record_publishes(st, "git", &p.name, items);
    let mut report = Vec::new();
    match &res.unpack {
        Ok(_) => report.extend_from_slice(&pkt(b"unpack ok\n")),
        Err(e) => report.extend_from_slice(&pkt(format!("unpack {e}\n").as_bytes())),
    }
    for (r, s) in &res.statuses {
        let line = match s {
            Ok(()) => format!("ok {r}\n"),
            Err(why) => format!("ng {r} {why}\n"),
        };
        report.extend_from_slice(&pkt(line.as_bytes()));
    }
    report.extend_from_slice(FLUSH);
    let mut out = Vec::new();
    if res.sideband {
        sideband(&mut out, 1, &report, true);
        out.extend_from_slice(FLUSH);
    } else {
        out = report;
    }
    let mut resp = Response::new(Body::from(out));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-git-receive-pack-result"));
    no_cache(resp.headers_mut());
    resp
}

fn process_push(git: &GitState, repo: &str, path: &std::path::Path) -> Result<PushResult> {
    let mut f = std::fs::File::open(path)?;
    // Commands.
    let mut cmds = Vec::new();
    let mut caps = String::new();
    let pack_off;
    {
        let mut r = BufReader::new(&mut f);
        let mut consumed = 0u64;
        loop {
            let mut len = [0u8; 4];
            r.read_exact(&mut len).map_err(|e| Error::Parse(format!("push commands: {e}")))?;
            consumed += 4;
            let n = usize::from_str_radix(std::str::from_utf8(&len).map_err(|_| Error::Parse("bad pkt".into()))?, 16)
                .map_err(|_| Error::Parse("bad pkt".into()))?;
            if n == 0 {
                break;
            }
            if !(4..=65520).contains(&n) {
                return Err(Error::Parse("bad pkt length".into()));
            }
            let mut line = vec![0u8; n - 4];
            r.read_exact(&mut line)?;
            consumed += (n - 4) as u64;
            let line = trim_nl(&line);
            let (cmd, c) = match line.iter().position(|&b| b == 0) {
                Some(i) => (&line[..i], Some(&line[i + 1..])),
                None => (line, None),
            };
            if let Some(c) = c {
                caps = String::from_utf8_lossy(c).to_string();
            }
            if cmd.starts_with(b"shallow ") || cmd.starts_with(b"push-cert") {
                return Err(Error::Unsupported("shallow pushes and push certificates are not supported"));
            }
            let parts: Vec<&[u8]> = cmd.splitn(3, |&b| b == b' ').collect();
            if parts.len() != 3 {
                return Err(Error::Parse("bad push command".into()));
            }
            cmds.push(Command {
                old: parse_oid(parts[0]).ok_or_else(|| Error::Parse("bad old oid".into()))?,
                new: parse_oid(parts[1]).ok_or_else(|| Error::Parse("bad new oid".into()))?,
                gitref: String::from_utf8(parts[2].to_vec()).map_err(|_| Error::Parse("non-UTF-8 ref name".into()))?,
            });
        }
        pack_off = consumed;
    }
    let capset: HashSet<&str> = caps.split(' ').collect();
    let sideband = capset.contains("side-band-64k");
    let atomic = capset.contains("atomic");
    let fmt = git.fmt_for(repo);
    // The client states its repository's format; it must match this repository's.
    if let Some(of) = capset.iter().find_map(|c| c.strip_prefix("object-format="))
        && of != fmt.name()
    {
        return Err(Error::Parse(format!("this repository uses object-format={}, the push uses {of}", fmt.name())));
    }
    if cmds.iter().any(|c| c.old.len() != fmt.hash_len() || c.new.len() != fmt.hash_len()) {
        return Err(Error::Parse(format!("push commands must use {} object ids", fmt.name())));
    }
    let mut view = View::for_repo(git, repo)?;
    let has_pack = f.metadata()?.len() > pack_off;
    let unpack = if has_pack {
        match index_pack(&mut view, &mut f, pack_off) {
            Ok(ix) => {
                git.oidmap.insert_blocking(ix.new_entries)?;
                Ok(ix.objects)
            }
            Err(e) => Err(e.to_string()),
        }
    } else {
        Ok(0)
    };
    if let Err(e) = &unpack {
        let statuses = cmds.iter().map(|c| (c.gitref.clone(), Err("unpacker error".to_string()))).collect();
        warn!(repo, error = %e, "push rejected");
        return Ok(PushResult { unpack, statuses, sideband, applied: Vec::new() });
    }
    // Validate commands.
    let mut statuses: Vec<(String, std::result::Result<(), String>)> = Vec::new();
    // `None` hash = delete.
    let mut updates: Vec<(Name, Option<Hash>, Observed, usize)> = Vec::new();
    for (i, c) in cmds.iter().enumerate() {
        let check = (|| -> std::result::Result<(Name, Option<Hash>, Observed), String> {
            let name = ref_name(repo, &c.gitref).ok_or("unsupported ref name")?;
            if c.new == fmt.zero() {
                let cur = git.repo.resolve(&name).map_err(|e| e.to_string())?;
                let cur_oid = match &cur {
                    Some(v) => Some(fmt.hash(&git.repo.get(&v.hash, None).map_err(|e| e.to_string())?)),
                    None => None,
                };
                if cur_oid != Some(c.old) && c.old != fmt.zero() {
                    return Err("fetch first (stale old value)".into());
                }
                return Ok((name, None, RefValue::observed(cur)));
            }
            let (kind, h) = view.lookup(&c.new).map_err(|e| e.to_string())?.ok_or("missing new object")?;
            // Not in this push: the client assumes we have it, so make sure it stays.
            if !view.extra.contains_key(&c.new) && !git.repo.ensure_live(&h).map_err(|e| e.to_string())? {
                return Err("missing new object".into());
            }
            if c.gitref.starts_with("refs/heads/") && kind != Kind::Commit {
                return Err("branch must point at a commit".into());
            }
            let cur = git.repo.resolve(&name).map_err(|e| e.to_string())?;
            let cur_oid = match &cur {
                Some(v) => Some(fmt.hash(&git.repo.get(&v.hash, None).map_err(|e| e.to_string())?)),
                None => None,
            };
            let expected = if c.old == fmt.zero() { None } else { Some(c.old) };
            if cur_oid != expected {
                return Err("fetch first (stale old value)".into());
            }
            Ok((name, Some(h), RefValue::observed(cur)))
        })();
        match check {
            Ok((n, h, o)) => {
                updates.push((n, h, o, i));
                statuses.push((c.gitref.clone(), Ok(())));
            }
            Err(e) => statuses.push((c.gitref.clone(), Err(e))),
        }
    }
    let planned: Vec<(usize, Option<Hash>)> = updates.iter().map(|x| (x.3, x.1)).collect();
    if atomic {
        if statuses.iter().any(|s| s.1.is_err()) {
            for s in statuses.iter_mut() {
                if s.1.is_ok() {
                    s.1 = Err("atomic push failed".into());
                }
            }
            return Ok(PushResult { unpack, statuses, sideband, applied: Vec::new() });
        }
        if !updates.is_empty() {
            let u: Vec<(Name, Option<Hash>)> = updates.iter().map(|x| (x.0.clone(), x.1)).collect();
            let rs: Vec<(Name, Observed)> = updates.iter().map(|x| (x.0.clone(), x.2)).collect();
            let why = match git.repo.update_many(&u, &rs) {
                Ok(ReplaceManyOutcome::Ok { .. }) => None,
                Ok(ReplaceManyOutcome::Conflict { .. }) => Some("fetch first (concurrent update)".to_string()),
                Err(Error::Forbidden { .. }) => Some("ref is immutable".to_string()),
                Err(e) => Some(e.to_string()),
            };
            if let Some(why) = why {
                for s in statuses.iter_mut() {
                    s.1 = Err(why.clone());
                }
            }
        }
    } else {
        for (n, h, o, i) in updates {
            let res = match h {
                Some(h) => git.repo.replace(&n, &[(n.clone(), o)], h),
                None => git.repo.delete_ref(&n, &[(n.clone(), o)]),
            };
            let r = match res {
                Ok(ReplaceOutcome::Ok { .. }) => Ok(()),
                Ok(ReplaceOutcome::Conflict { .. }) => Err("fetch first (concurrent update)".to_string()),
                Err(Error::Forbidden { .. }) => Err("ref is immutable".to_string()),
                Err(e) => Err(e.to_string()),
            };
            statuses[i].1 = r;
        }
    }
    let applied = planned
        .into_iter()
        .filter(|(i, _)| statuses.get(*i).is_some_and(|s| s.1.is_ok()))
        .filter_map(|(i, h)| Some((statuses.get(i)?.0.clone(), cmds.get(i)?.new, h?)))
        .collect();
    Ok(PushResult { unpack, statuses, sideband, applied })
}

// ---------------------------------------------------------------------------
// Grouped repack hints
// ---------------------------------------------------------------------------

/// Ordering **hints** for git content (`pico2peta_repo::Grouper`; docs/ORDERING.md). Since API
/// v1.15 the core orders every small live object by content similarity itself; these keys only
/// break ties between equally similar objects, and objects without a key are ordered too.
/// The keys are the Experiment 1 V2 layout:
/// * blobs: group `blob:<ext>` (or `blob:<basename>` without an extension),
///   ordered by path, then the time of the oldest commit that contains them;
/// * trees: group `tree`, ordered by directory path, then time;
/// * commits and tags: groups `commit` / `tag`, ordered by time.
///
/// Every git repository's refs are walked (oldest commits first), so all
/// versions of a file end up adjacent and in history order.
pub struct GitGrouper;

/// Commit or tagger time from a commit/tag body (`committer … <ts> <tz>`).
fn object_time(kind: Kind, body: &[u8]) -> u64 {
    let field: &[u8] = if kind == Kind::Tag { b"tagger " } else { b"committer " };
    for line in body.split(|&c| c == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(field) {
            let mut it = rest.rsplit(|&c| c == b' ');
            let _tz = it.next();
            if let Some(ts) = it.next().and_then(|t| std::str::from_utf8(t).ok()).and_then(|t| t.parse().ok()) {
                return ts;
            }
        }
    }
    0
}

fn blob_group(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rfind('.') {
        Some(i) if i > 0 && i + 1 < base.len() => format!("blob:{}", &base[i + 1..]),
        _ => format!("blob:{base}"),
    }
}

impl pico2peta_repo::Grouper for GitGrouper {
    fn assign(&self, repo: &dyn Repository, live: &HashSet<Hash>) -> Result<HashMap<Hash, pico2peta_repo::GroupKey>> {
        use pico2peta_repo::GroupKey;
        let mut out: HashMap<Hash, GroupKey> = HashMap::new();
        let n = Name::new("git/_oidmap").unwrap_or_else(|_| unreachable!("static name is valid"));
        let Some(rv) = repo.resolve(&n)? else { return Ok(out) };
        let root = crate::segmap::Root::decode(&repo.get(&rv.hash, None)?)?;
        // A SegMap needs an Arc<dyn Repository>; read segments directly instead.
        let lookup = OidLookup { repo, root, cache: HashMap::new() };
        let mut lookup = lookup;
        let load = |l: &mut OidLookup<'_>, oid: &Oid| -> Result<Option<(Hash, Kind, Vec<u8>)>> {
            let Some(h) = l.get(oid)? else { return Ok(None) };
            let bytes = match l.repo.get(&h, None) {
                Ok(b) => b,
                Err(Error::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            let Some((kind, size, hl)) = parse_header(&bytes) else { return Ok(None) };
            if hl + size != bytes.len() {
                return Ok(None);
            }
            Ok(Some((h, kind, bytes[hl..].to_vec())))
        };
        // 1. All commits (and tags) reachable from any git ref.
        let mut tips: Vec<Oid> = Vec::new();
        for (name, v) in repo.list_refs("git")? {
            if name.as_str().starts_with("git/_") || !name.as_str().contains("/refs/") {
                continue;
            }
            let bytes = repo.get(&v.hash, None)?;
            // Either object format; the id that isn't in the oid map is skipped below.
            tips.push(oid_of(&bytes));
            tips.push(Fmt::Sha256.hash(&bytes));
        }
        let mut commits: Vec<(u64, Oid, Hash, Vec<u8>)> = Vec::new();
        let mut seen: HashSet<Oid> = HashSet::new();
        let mut stack = tips;
        while let Some(o) = stack.pop() {
            if !seen.insert(o) {
                continue;
            }
            let Some((h, kind, body)) = load(&mut lookup, &o)? else { continue };
            match kind {
                Kind::Tag => {
                    let t = object_time(kind, &body);
                    if live.contains(&h) {
                        out.insert(h, GroupKey { group: "tag".into(), order: format!("{t:020}") });
                    }
                    stack.extend(refs_any(kind, &body));
                }
                Kind::Commit => {
                    let t = object_time(kind, &body);
                    let refs = refs_any(kind, &body);
                    stack.extend(refs.iter().skip(1).copied());
                    commits.push((t, o, h, body));
                }
                _ => {}
            }
        }
        commits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        // 2. Trees and blobs, first seen from the oldest commit.
        let mut seen_tree: HashSet<Oid> = HashSet::new();
        for (t, _, h, body) in commits {
            if live.contains(&h) {
                out.insert(h, GroupKey { group: "commit".into(), order: format!("{t:020}") });
            }
            let Some(tree) = refs_any(Kind::Commit, &body).first().copied() else { continue };
            let mut stack: Vec<(Oid, String)> = vec![(tree, String::new())];
            while let Some((toid, dir)) = stack.pop() {
                if !seen_tree.insert(toid) {
                    continue;
                }
                let Some((th, kind, tbody)) = load(&mut lookup, &toid)? else { continue };
                if kind != Kind::Tree {
                    continue;
                }
                if live.contains(&th) {
                    out.insert(th, GroupKey { group: "tree".into(), order: format!("{dir}\0{t:020}") });
                }
                let mut i = 0;
                while i < tbody.len() {
                    let Some(sp) = tbody[i..].iter().position(|&c| c == b' ').map(|x| x + i) else { break };
                    let Some(nul) = tbody[sp..].iter().position(|&c| c == 0).map(|x| x + sp) else { break };
                    let hl = toid.len();
                    if nul + 1 + hl > tbody.len() {
                        break;
                    }
                    let mode = &tbody[i..sp];
                    let name = String::from_utf8_lossy(&tbody[sp + 1..nul]).to_string();
                    let Some(oid) = Oid::from_slice(&tbody[nul + 1..nul + 1 + hl]) else { break };
                    i = nul + 1 + hl;
                    let path = if dir.is_empty() { name } else { format!("{dir}/{name}") };
                    match mode {
                        b"40000" => stack.push((oid, path)),
                        b"160000" => {}
                        _ => {
                            if let Some(bh) = lookup.get(&oid)?
                                && live.contains(&bh)
                            {
                                out.entry(bh).or_insert_with(|| GroupKey {
                                    group: blob_group(&path),
                                    order: format!("{path}\0{t:020}"),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Oid → BLAKE3 lookups against an oid-map root, reading segments directly.
struct OidLookup<'a> {
    repo: &'a dyn Repository,
    root: crate::segmap::Root,
    cache: HashMap<Hash, Arc<Vec<(Key, Value)>>>,
}

impl OidLookup<'_> {
    fn get(&mut self, oid: &Oid) -> Result<Option<Hash>> {
        let k = key_of(oid);
        for (seg, _) in self.root.segments.iter().rev() {
            let entries = match self.cache.get(seg) {
                Some(e) => e.clone(),
                None => {
                    let e = Arc::new(crate::segmap::decode_segment(&self.repo.get(seg, None)?)?);
                    self.cache.insert(*seg, e.clone());
                    e
                }
            };
            if let Ok(i) = entries.binary_search_by(|(x, _)| x.cmp(&k)) {
                return Ok(Some(entries[i].1.hash));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Fuzzing hooks (see `crate::fuzzing`)
// ---------------------------------------------------------------------------

pub(crate) fn fuzz_push(repo: Arc<pico2peta_repo::LocalRepo>, spool: &std::path::Path, data: &[u8]) {
    let Ok(st) = GitState::new(repo, spool.join("spool"), 1 << 30) else { return };
    let p = spool.join("req");
    if std::fs::write(&p, data).is_err() {
        return;
    }
    if let Ok(res) = process_push(&st, "fuzz/repo", &p) {
        // A successful unpack must leave every referenced object resolvable.
        if res.unpack.is_ok() {
            for (r, s) in &res.statuses {
                if s.is_ok() {
                    let n = ref_name("fuzz/repo", r).unwrap_or_else(|| panic!("accepted bad ref {r}"));
                    // An accepted update (not a delete) must point at a stored object.
                    if let Some(v) = st.repo.resolve(&n).ok().flatten() {
                        assert!(st.repo.contains(&v.hash).unwrap_or(false), "ok ref {r} points at a missing object");
                    }
                }
            }
        }
    }
}

/// A receive-pack request creating `refs/heads/main` → `objects[tip]`, with
/// every object stored whole (codes 1..=4) in a valid pack.
pub(crate) fn fuzz_make_push(objects: &[(u8, Vec<u8>)], tip: usize) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    let mut tip_oid = Fmt::Sha1.zero();
    for (i, (code, body)) in objects.iter().enumerate() {
        let kind = Kind::from_code(*code).unwrap_or(Kind::Blob);
        if i == tip {
            tip_oid = oid_of(&canonical(kind, body));
        }
        pack.extend_from_slice(&pack_entry_header(kind, body.len()));
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let _ = z.write_all(body);
        pack.extend_from_slice(&z.finish().unwrap_or_default());
    }
    let trailer: [u8; 20] = Sha1::digest(&pack).into();
    pack.extend_from_slice(&trailer);
    let mut out =
        pkt(format!("{} {} refs/heads/main\0report-status\n", hex(&Fmt::Sha1.zero()), hex(&tip_oid)).as_bytes());
    out.extend_from_slice(FLUSH);
    out.extend_from_slice(&pack);
    out
}

pub(crate) fn fuzz_parsers(data: &[u8]) {
    let _ = parse_header(data);
    for k in [Kind::Commit, Kind::Tree, Kind::Tag, Kind::Blob] {
        let _ = refs_of(k, data);
    }
    let _ = parse_upload_request(data);
    let _ = read_entry_header(&mut &data[..], 1 << 40, 20);
    let _ = read_entry_header(&mut &data[..], 1 << 40, 32);
    if data.len() > 2 {
        let split = data[0] as usize % data.len();
        let _ = apply_delta(&data[..split], &data[split..]);
    }
    let mut r = data;
    while let Ok(Some(_)) = read_pkt(&mut r) {}
    if data.len() > 2 {
        let split = data[1] as usize % data.len();
        let (b, t) = data.split_at(split);
        let back = apply_delta(b, &encode_delta(b, t)).unwrap_or_else(|e| panic!("encoded delta does not apply: {e}"));
        assert!(back == t, "delta roundtrip mismatch");
    }
}

// ---------------------------------------------------------------------------
// GC
// ---------------------------------------------------------------------------

/// GC tracer for git objects (see the module docs).
pub struct GitTracer {
    oidmap: SegMap,
    root: Root,
    /// Git LFS index (sha256 → content), written by the HF front.
    lfs: SegMap,
    lfs_root: Root,
    /// sha256 → Xet file record (for Xet downloads of files uploaded through Xet).
    xetsha: SegMap,
    xetsha_root: Root,
}

impl GitTracer {
    /// Snapshots the oid map; objects pushed later are protected by the GC lease.
    pub fn new(repo: Arc<dyn Repository>) -> Result<GitTracer> {
        let n = Name::new("git/_oidmap").unwrap_or_else(|_| unreachable!("static name is valid"));
        let oidmap = SegMap::new(repo.clone(), n, Arc::new(SegCache::default()));
        let root = oidmap.root_blocking()?.1;
        let n = Name::new("git/_lfs").unwrap_or_else(|_| unreachable!("static name is valid"));
        let lfs = SegMap::new(repo.clone(), n, Arc::new(SegCache::default()));
        let lfs_root = lfs.root_blocking()?.1;
        let n = Name::new("git/_xetsha").unwrap_or_else(|_| unreachable!("static name is valid"));
        let xetsha = SegMap::new(repo, n, Arc::new(SegCache::default()));
        let xetsha_root = xetsha.root_blocking()?.1;
        Ok(GitTracer { oidmap, root, lfs, lfs_root, xetsha, xetsha_root })
    }

    /// Children of a canonical git object, or `None` if `h` is not one.
    pub fn children_of(&self, h: &Hash, store: &dyn ObjectStore) -> Result<Option<Vec<Hash>>> {
        let size = store.size(h)?;
        let head = store.get(h, Some(0..size.min(32)))?;
        if let Some(c) = crate::xet::record_child(h, &head, size, store)? {
            return Ok(Some(vec![c])); // Xet xorb record -> its content
        }
        let Some((kind, body_len, hl)) = parse_header(&head) else { return Ok(None) };
        if hl as u64 + body_len as u64 != size {
            return Ok(None);
        }
        if kind == Kind::Blob {
            // A Git LFS pointer keeps its content alive (LFS objects have no other referrer).
            const LFS: &[u8] = b"version https://git-lfs";
            let seen = &head[hl..];
            let n = seen.len().min(LFS.len());
            if body_len <= 512 && body_len >= LFS.len() && seen[..n] == LFS[..n] {
                let bytes = store.get(h, None)?;
                if let Some((sha, _)) = crate::hf::parse_lfs_pointer(&bytes[hl..])
                    && let Some(k) = crate::hf::sha_key(&sha)
                {
                    let mut kids = Vec::new();
                    if let Some(v) = self.lfs.get_in(&self.lfs_root, &k)? {
                        kids.push(v.hash);
                    }
                    // The Xet file record (it references the same content).
                    if let Some(v) = self.xetsha.get_in(&self.xetsha_root, &k)? {
                        kids.push(v.hash);
                    }
                    return Ok(Some(kids));
                }
            }
            return Ok(Some(Vec::new()));
        }
        let bytes = store.get(h, None)?;
        // The tracer sees core objects, not repositories: accept either object format.
        let keys: Vec<Key> = refs_any(kind, &bytes[hl..]).iter().map(key_of).collect();
        Ok(Some(self.oidmap.get_many(&self.root, &keys)?.into_iter().flatten().map(|v| v.hash).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_and_oids() {
        let c = canonical(Kind::Blob, b"hello\n");
        assert_eq!(&c[..7], b"blob 6\0");
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(hex(&oid_of(&c)), "ce013625030ba8dba906f756967f9e9ca394464a");
        assert_eq!(parse_header(&c), Some((Kind::Blob, 6, 7)));
        for bad in [&b"blob 06\0"[..], b"blob \0", b"blobx 1\0", b"blob 1", b"commit 99999999999999999999999\0"] {
            assert_eq!(parse_header(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn delta_apply() {
        let base = b"0123456789abcdef";
        // src=16, dst=9: copy 4 bytes at offset 2, insert "XYZ", copy 2 at 14.
        let d = [16u8, 9, 0x91, 2, 4, 3, b'X', b'Y', b'Z', 0x91, 14, 2];
        assert_eq!(apply_delta(base, &d).unwrap(), b"2345XYZef");
        assert!(apply_delta(base, &[15, 1, 1, b'a']).is_err(), "base size mismatch");
        assert!(apply_delta(base, &[16, 4, 0x91, 15, 4]).is_err(), "copy out of range");
        assert!(apply_delta(base, &[16, 1, 0]).is_err(), "opcode 0");
        assert!(apply_delta(base, &[16, 2, 1, b'a']).is_err(), "short result");
    }

    #[test]
    fn tree_and_commit_refs() {
        let mut tree = b"100644 a.txt\0".to_vec();
        tree.extend_from_slice(&[1u8; 20]);
        tree.extend_from_slice(b"160000 sub\0");
        tree.extend_from_slice(&[2u8; 20]);
        tree.extend_from_slice(b"40000 dir\0");
        tree.extend_from_slice(&[3u8; 20]);
        assert_eq!(refs_of(Kind::Tree, &tree).unwrap(), vec![[1u8; 20], [3u8; 20]]);
        assert!(refs_of(Kind::Tree, &tree[..tree.len() - 1]).is_err());
        let commit = format!("tree {}\nparent {}\nauthor x\n\nmsg\nparent zzz\n", "aa".repeat(20), "bb".repeat(20));
        assert_eq!(refs_of(Kind::Commit, commit.as_bytes()).unwrap(), vec![[0xaa; 20], [0xbb; 20]]);
        assert!(refs_of(Kind::Commit, b"author x\n\nmsg").is_err());
    }

    #[test]
    fn delta_roundtrip() {
        let base: Vec<u8> = (0..5000u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut target = base.clone();
        target.splice(100..100, b"inserted text".iter().copied());
        target.drain(3000..3100);
        target.extend_from_slice(b"tail");
        let d = encode_delta(&base, &target);
        assert!(d.len() < 100, "delta should be small: {}", d.len());
        assert_eq!(apply_delta(&base, &d).unwrap(), target);
        for (b, t) in [(&b""[..], &b"x"[..]), (b"short", b"shorter text here, longer than 16"), (&base[..], b"")] {
            assert_eq!(apply_delta(b, &encode_delta(b, t)).unwrap(), t);
        }
        // Copies longer than 0xffffff split correctly.
        let big = vec![7u8; 0x100_0100];
        assert_eq!(apply_delta(&big, &encode_delta(&big, &big)).unwrap(), big);
        for off in [1u64, 127, 128, 129, 16_511, 16_512, 1 << 20, u32::MAX as u64] {
            let enc = ofs_encode(off);
            let mut hdr = vec![0x60u8]; // OFS_DELTA, size 0
            hdr.extend_from_slice(&enc);
            let (_, _, base, _) = read_entry_header(&mut &hdr[..], off + 10, 20).unwrap();
            assert!(matches!(base, Base::Ofs(b) if b == 10), "{off}");
        }
    }

    #[test]
    fn pack_headers() {
        assert_eq!(pack_entry_header(Kind::Blob, 5), vec![0x35]);
        let h = pack_entry_header(Kind::Commit, 300);
        let (code, size, _, n) = read_entry_header(&mut &h[..], 100, 20).unwrap();
        assert_eq!((code, size, n as usize), (1, 300, h.len()));
    }

    #[test]
    fn upload_request_parsing() {
        let mut b = pkt(format!("want {} side-band-64k agent=git/2\n", "ab".repeat(20)).as_bytes());
        b.extend_from_slice(FLUSH);
        b.extend_from_slice(&pkt(format!("have {}\n", "cd".repeat(20)).as_bytes()));
        b.extend_from_slice(&pkt(b"done\n"));
        let r = parse_upload_request(&b).unwrap();
        assert_eq!(r.wants, vec![[0xab; 20]]);
        assert_eq!(r.haves, vec![[0xcd; 20]]);
        assert!(r.done && r.sideband);
        let mut s = pkt(format!("want {}\n", "ab".repeat(20)).as_bytes());
        s.extend_from_slice(&pkt(b"deepen 1\n"));
        assert!(parse_upload_request(&s).is_err());
    }
}
