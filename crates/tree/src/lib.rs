//! Directory snapshots as standard objects (docs/ARCHITECTURE.md, "Trees").
//!
//! A tree object is a canonical, sorted list of entries `(name, mode, hash)`:
//! one path segment as raw bytes, a mode (regular file, executable file,
//! symlink, subtree) and the hash of the blob or subtree. Encoding,
//! decoding and the canonical-form check are the h5i kernel
//! `o5s_kernel::tree`, proved in `verify/core/proofs/Tree.lean`: the decoder
//! accepts exactly the canonical encodings, encode/decode round-trip, and
//! the tracer returns exactly the entry hashes. This crate is the shell:
//! the [`Tree`] type, the GC front ([`TreeFront`]), and the filesystem side
//! ([`put_dir`], [`checkout`], [`diff`]).
//!
//! Entries are not names, so they need no name encoding: a tree is one
//! object, and only the tree's own hash is ever put under a name.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use o5s_core::{Error, Hash, ObjectStore, Repository, Result};
use o5s_kernel::tree as k;
use o5s_repo::front::Front;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

pub use o5s_kernel::tree::MAX_TREE;

/// The 8 bytes every tree object starts with.
pub const MAGIC: &[u8; 8] = b"O5STREE1";

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    /// A regular file; the hash is its contents.
    Regular,
    /// An executable file; the hash is its contents.
    Executable,
    /// A symbolic link; the hash is a blob of the target's bytes.
    Symlink,
    /// A directory; the hash is a tree.
    Subtree,
}

impl Mode {
    fn code(self) -> u8 {
        match self {
            Mode::Regular => k::REGULAR,
            Mode::Executable => k::EXECUTABLE,
            Mode::Symlink => k::SYMLINK,
            Mode::Subtree => k::SUBTREE,
        }
    }

    fn from_code(c: u8) -> Result<Mode> {
        match c {
            k::REGULAR => Ok(Mode::Regular),
            k::EXECUTABLE => Ok(Mode::Executable),
            k::SYMLINK => Ok(Mode::Symlink),
            k::SUBTREE => Ok(Mode::Subtree),
            _ => Err(Error::Corrupt(format!("tree entry mode {c}"))),
        }
    }

    /// Whether the entry is a file of either kind.
    pub fn is_file(self) -> bool {
        matches!(self, Mode::Regular | Mode::Executable)
    }

    /// One-letter tag for listings: `f`, `x`, `l` or `d`.
    pub fn tag(self) -> char {
        match self {
            Mode::Regular => 'f',
            Mode::Executable => 'x',
            Mode::Symlink => 'l',
            Mode::Subtree => 'd',
        }
    }
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// One path segment, raw bytes (no `/`, no NUL, not `.` or `..`).
    pub name: Vec<u8>,
    /// What it is.
    pub mode: Mode,
    /// The blob or subtree.
    pub hash: Hash,
}

/// A directory snapshot: entries strictly sorted by name bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// The entries, sorted.
    pub entries: Vec<Entry>,
}

fn tree_err(e: k::TreeError) -> Error {
    Error::Parse(format!("not a canonical tree: {e:?}"))
}

impl Tree {
    /// A tree of these entries, sorted by name. Fails if names repeat or are
    /// invalid, or if the encoding would be over [`MAX_TREE`] bytes.
    pub fn new(mut entries: Vec<Entry>) -> Result<Tree> {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let t = Tree { entries };
        t.encode()?;
        Ok(t)
    }

    /// The canonical encoding (the bytes whose hash is the tree's identity).
    pub fn encode(&self) -> Result<Vec<u8>> {
        let es: Vec<k::Entry> = self
            .entries
            .iter()
            .map(|e| k::Entry { mode: e.mode.code(), name: e.name.clone(), hash: e.hash.0 })
            .collect();
        k::encode(&es).map_err(tree_err)
    }

    /// Decodes canonical tree bytes; anything else is an error.
    pub fn decode(bytes: &[u8]) -> Result<Tree> {
        let es = k::decode(bytes).map_err(tree_err)?;
        let entries = es
            .into_iter()
            .map(|e| Ok(Entry { name: e.name, mode: Mode::from_code(e.mode)?, hash: Hash(e.hash) }))
            .collect::<Result<Vec<_>>>()?;
        Ok(Tree { entries })
    }

    /// The entry named `name`.
    pub fn get(&self, name: &[u8]) -> Option<&Entry> {
        self.entries.binary_search_by(|e| e.name.as_slice().cmp(name)).ok().map(|i| &self.entries[i])
    }
}

/// Whether `bytes` are a canonical tree.
pub fn is_tree(bytes: &[u8]) -> bool {
    k::decode(bytes).is_ok()
}

/// Stores a tree after checking that every entry's object is stored and
/// live (so the tree never references a missing or condemned object: GC
/// treats a missing child as corruption). Subtrees must be trees.
pub fn put_tree(repo: &dyn ObjectStore, tree: &Tree) -> Result<Hash> {
    for e in &tree.entries {
        if !repo.ensure_live(&e.hash)? {
            return Err(Error::NotFound(e.hash));
        }
    }
    repo.put(&tree.encode()?)
}

/// Reads tree `hash`. An object that is not a canonical tree is an error.
pub fn get_tree(repo: &dyn ObjectStore, hash: &Hash) -> Result<Tree> {
    let size = repo.size(hash)?;
    if size > MAX_TREE as u64 {
        return Err(Error::Parse(format!("{hash} is not a tree ({size} bytes)")));
    }
    Tree::decode(&repo.get(hash, None)?).map_err(|e| Error::Parse(format!("{hash}: {e}")))
}

/// The GC front: a tree keeps every entry's object alive. Objects that are
/// not canonical trees (including blobs that merely start with the magic)
/// have no children. It claims no names.
#[derive(Default)]
pub struct TreeFront;

impl TreeFront {
    /// The front's id.
    pub const ID: &'static str = "tree";
}

/// The children of `hash` if it is a tree (see [`TreeFront`]).
pub fn tree_children(hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
    let size = store.size(hash)?;
    if size < MAGIC.len() as u64 || size > MAX_TREE as u64 {
        return Ok(Vec::new());
    }
    if store.get(hash, Some(0..MAGIC.len() as u64))? != MAGIC {
        return Ok(Vec::new());
    }
    match k::children(&store.get(hash, None)?) {
        Ok(hs) => Ok(hs.into_iter().map(Hash).collect()),
        Err(_) => Ok(Vec::new()),
    }
}

impl Front for TreeFront {
    fn id(&self) -> &str {
        Self::ID
    }

    fn children(&self, hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
        tree_children(hash, store)
    }
}

/// Options for [`put_dir`].
#[derive(Clone, Debug, Default)]
pub struct PutDirOpts {
    /// Entry names skipped at any depth (e.g. `.git`).
    pub exclude: HashSet<Vec<u8>>,
}

/// What [`put_dir`] stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PutDirStats {
    /// Files (regular and executable).
    pub files: u64,
    /// Symlinks.
    pub symlinks: u64,
    /// Directories, the root included.
    pub dirs: u64,
    /// File bytes read.
    pub bytes: u64,
}

#[cfg(unix)]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(s).to_vec()
}

#[cfg(unix)]
fn bytes_os(b: &[u8]) -> &std::ffi::OsStr {
    std::os::unix::ffi::OsStrExt::from_bytes(b)
}

fn io_at(path: &Path, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// Snapshots directory `dir` (not following symlinks) into trees and blobs
/// and returns the root tree's hash. Everything is written through `repo`'s
/// one writer and flushed once at the end, so the returned hash is durable.
/// Fails on special files (devices, FIFOs, sockets).
#[cfg(unix)]
pub fn put_dir(repo: &dyn Repository, dir: &Path, opts: &PutDirOpts) -> Result<(Hash, PutDirStats)> {
    let mut stats = PutDirStats::default();
    let h = put_dir_rec(repo, dir, opts, &mut stats)?;
    repo.flush()?;
    Ok((h, stats))
}

#[cfg(unix)]
fn put_dir_rec(repo: &dyn Repository, dir: &Path, opts: &PutDirOpts, stats: &mut PutDirStats) -> Result<Hash> {
    use std::os::unix::fs::PermissionsExt;
    stats.dirs += 1;
    let mut names: Vec<Vec<u8>> = Vec::new();
    for ent in std::fs::read_dir(dir).map_err(|e| io_at(dir, e))? {
        let name = os_bytes(&ent.map_err(|e| io_at(dir, e))?.file_name());
        if !opts.exclude.contains(&name) {
            names.push(name);
        }
    }
    names.sort();
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        let path = dir.join(bytes_os(&name));
        let md = std::fs::symlink_metadata(&path).map_err(|e| io_at(&path, e))?;
        let ft = md.file_type();
        let (mode, hash) = if ft.is_dir() {
            (Mode::Subtree, put_dir_rec(repo, &path, opts, stats)?)
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| io_at(&path, e))?;
            stats.symlinks += 1;
            (Mode::Symlink, repo.put(&os_bytes(target.as_os_str()))?)
        } else if ft.is_file() {
            let f = std::fs::File::open(&path).map_err(|e| io_at(&path, e))?;
            let mut src = std::io::BufReader::with_capacity(1 << 20, f);
            stats.files += 1;
            stats.bytes += md.len();
            let mode = if md.permissions().mode() & 0o100 != 0 { Mode::Executable } else { Mode::Regular };
            (mode, repo.put_stream(&mut src).map_err(|e| in_path(&path, e))?)
        } else {
            return Err(Error::Parse(format!("{}: unsupported file type (not a file, symlink or directory)", path.display())));
        };
        entries.push(Entry { name, mode, hash });
    }
    // Sorted by name already; `Tree::new` still checks every rule.
    let t = Tree::new(entries).map_err(|e| in_path(dir, e))?;
    repo.put(&t.encode()?)
}

fn in_path(path: &Path, e: Error) -> Error {
    match e {
        Error::Io(io) => io_at(path, io),
        Error::Parse(m) => Error::Parse(format!("{}: {m}", path.display())),
        e => e,
    }
}

/// Writes tree `hash` out as directory `dir`, which must not exist or be an
/// empty directory. Every object is read verified. Files are created new
/// (never through an existing path), so a tree can't write outside `dir`:
/// names are single segments, and symlinks are created, never followed.
/// Executables get mode 0755 and other files 0644, both masked by the umask.
#[cfg(unix)]
pub fn checkout(repo: &dyn ObjectStore, hash: &Hash, dir: &Path) -> Result<PutDirStats> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let md = std::fs::symlink_metadata(dir).map_err(|e| io_at(dir, e))?;
            if !md.is_dir() || std::fs::read_dir(dir).map_err(|e| io_at(dir, e))?.next().is_some() {
                return Err(Error::Parse(format!("{}: exists and is not an empty directory", dir.display())));
            }
        }
        Err(e) => return Err(io_at(dir, e)),
    }
    let mut stats = PutDirStats::default();
    let mut stack: Vec<(Hash, PathBuf, usize)> = vec![(*hash, dir.to_path_buf(), 0)];
    while let Some((h, at, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(Error::Parse(format!("{}: tree nested deeper than {MAX_DEPTH}", at.display())));
        }
        stats.dirs += 1;
        let t = get_tree(repo, &h)?;
        for e in t.entries {
            let path = at.join(bytes_os(&e.name));
            match e.mode {
                Mode::Subtree => {
                    std::fs::create_dir(&path).map_err(|err| io_at(&path, err))?;
                    stack.push((e.hash, path, depth + 1));
                }
                Mode::Symlink => {
                    let target = repo.get(&e.hash, None)?;
                    std::os::unix::fs::symlink(bytes_os(&target), &path).map_err(|err| io_at(&path, err))?;
                    stats.symlinks += 1;
                }
                Mode::Regular | Mode::Executable => {
                    let perm = if e.mode == Mode::Executable { 0o755 } else { 0o644 };
                    let f = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(perm)
                        .open(&path)
                        .map_err(|err| io_at(&path, err))?;
                    let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
                    let mut src = repo.open(&e.hash, None)?;
                    stats.bytes += std::io::copy(&mut src, &mut w).map_err(|err| io_at(&path, err))?;
                    w.flush().map_err(|err| io_at(&path, err))?;
                    stats.files += 1;
                }
            }
        }
    }
    Ok(stats)
}

/// Deepest nesting [`checkout`] and [`diff`] follow (a crafted tree could be
/// arbitrarily deep; no filesystem path is anywhere near this).
pub const MAX_DEPTH: usize = 4096;

/// How a path differs between two trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Only in the new tree.
    Added,
    /// Only in the old tree.
    Deleted,
    /// Same kind of entry, different contents.
    Modified,
    /// A file's mode changed (regular, executable, symlink), contents maybe too.
    TypeChanged,
}

impl Status {
    /// `A`, `D`, `M` or `T`.
    pub fn letter(self) -> char {
        match self {
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Modified => 'M',
            Status::TypeChanged => 'T',
        }
    }
}

/// One difference: a path (segments joined by `/`) that is not a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The path, raw bytes.
    pub path: Vec<u8>,
    /// What changed.
    pub status: Status,
    /// The old entry's mode and hash, if any.
    pub old: Option<(Mode, Hash)>,
    /// The new entry's mode and hash, if any.
    pub new: Option<(Mode, Hash)>,
}

fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = prefix.to_vec();
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// The leaf-level differences from tree `a` to tree `b`, sorted by path.
/// Subtrees with equal hashes are skipped without reading them; a directory
/// that appears or disappears is listed file by file.
pub fn diff(repo: &dyn ObjectStore, a: &Hash, b: &Hash) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    // (path prefix, old tree, new tree, depth); a `None` side is empty.
    let mut stack: Vec<(Vec<u8>, Option<Hash>, Option<Hash>, usize)> = vec![(Vec::new(), Some(*a), Some(*b), 0)];
    while let Some((prefix, ta, tb, depth)) = stack.pop() {
        if ta == tb {
            continue;
        }
        if depth > MAX_DEPTH {
            return Err(Error::Parse(format!("tree nested deeper than {MAX_DEPTH}")));
        }
        let ea = match &ta {
            Some(h) => get_tree(repo, h)?.entries,
            None => Vec::new(),
        };
        let eb = match &tb {
            Some(h) => get_tree(repo, h)?.entries,
            None => Vec::new(),
        };
        let (mut i, mut j) = (0, 0);
        while i < ea.len() || j < eb.len() {
            let ord = match (ea.get(i), eb.get(j)) {
                (Some(x), Some(y)) => x.name.cmp(&y.name),
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            let (x, y) = match ord {
                std::cmp::Ordering::Less => {
                    i += 1;
                    (Some(&ea[i - 1]), None)
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                    (None, Some(&eb[j - 1]))
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                    (Some(&ea[i - 1]), Some(&eb[j - 1]))
                }
            };
            let name = x.or(y).map(|e| e.name.as_slice()).unwrap_or_default();
            let path = join(&prefix, name);
            let sub = |e: Option<&Entry>| e.filter(|e| e.mode == Mode::Subtree).map(|e| e.hash);
            let leaf = |e: Option<&Entry>| e.filter(|e| e.mode != Mode::Subtree).map(|e| (e.mode, e.hash));
            // Directories on either side are compared recursively; a file
            // replacing a directory (or the reverse) is a delete plus an add.
            if sub(x).is_some() || sub(y).is_some() {
                stack.push((path.clone(), sub(x), sub(y), depth + 1));
            }
            match (leaf(x), leaf(y)) {
                (None, None) => {}
                (Some(o), None) => out.push(Change { path, status: Status::Deleted, old: Some(o), new: None }),
                (None, Some(n)) => out.push(Change { path, status: Status::Added, old: None, new: Some(n) }),
                (Some(o), Some(n)) if o == n => {}
                (Some(o), Some(n)) => {
                    let status = if o.0 == n.0 { Status::Modified } else { Status::TypeChanged };
                    out.push(Change { path, status, old: Some(o), new: Some(n) });
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then((a.status as u8).cmp(&(b.status as u8))));
    Ok(out)
}
