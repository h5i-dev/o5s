//! Core types shared by every pico2peta layer: content hashes, names, read sets,
//! namespace rules, errors, and the `ObjectStore` trait.
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::HashMap;
pub mod backend;
pub mod fault;

use std::fmt;
use std::io::Read;
use std::ops::Range;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Hash
// ---------------------------------------------------------------------------

/// BLAKE3-256 content hash. The identity of every object.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Length of a hash in bytes.
    pub const LEN: usize = 32;

    /// BLAKE3-256 of `data`.
    pub fn of(data: &[u8]) -> Hash {
        Hash(*blake3::hash(data).as_bytes())
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex (64 characters).
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 15) as usize] as char);
        }
        s
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", &self.to_hex()[..12])
    }
}

impl FromStr for Hash {
    type Err = Error;
    fn from_str(s: &str) -> Result<Hash> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return Err(Error::Parse(format!("hash must be 64 hex chars, got {}", b.len())));
        }
        let nib = |c: u8| -> Result<u8> {
            match c {
                b'0'..=b'9' => Ok(c - b'0'),
                b'a'..=b'f' => Ok(c - b'a' + 10),
                b'A'..=b'F' => Ok(c - b'A' + 10),
                _ => Err(Error::Parse(format!("bad hex char {:?}", c as char))),
            }
        };
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
        }
        Ok(Hash(out))
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every error a pico2peta API can return. `Error::code()` gives a stable
/// machine-readable string (used on the wire).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No object with this hash is stored.
    #[error("object not found: {0}")]
    NotFound(Hash),
    /// A backend blob (pack, index, GC record) doesn't exist.
    #[error("backend key not found: {0}")]
    KeyNotFound(String),
    /// A backend operation failed after the backend's own retries.
    #[error("backend error (transient={transient}): {msg}")]
    Backend {
        /// Whether retrying later might succeed (throttling, unavailability).
        transient: bool,
        /// Human-readable cause.
        msg: String,
    },
    /// A byte range outside `0..=len` was requested.
    #[error("invalid range {start}..{end} for object of length {len}")]
    InvalidRange {
        /// Requested start offset.
        start: u64,
        /// Requested end offset (exclusive).
        end: u64,
        /// Object length.
        len: u64,
    },
    /// A ref name or namespace prefix violates the name grammar (name, reason).
    #[error("invalid name {0:?}: {1}")]
    InvalidName(String, &'static str),
    /// `replace`/`delete_ref` target isn't in its own read set (no blind writes).
    #[error("replace target {0} is not in the read set")]
    TargetNotInReadSet(Name),
    /// A ref would point at an object that isn't stored.
    #[error("object {0} does not exist; refusing to point a name at it")]
    MissingObject(Hash),
    /// The namespace rule forbids this change.
    #[error("forbidden by namespace rule {rule:?} for {name}: {why}")]
    Forbidden {
        /// The name that may not change.
        name: Name,
        /// The rule governing it.
        rule: Rule,
        /// Why the rule forbids it.
        why: &'static str,
    },
    /// A rule can't be set or changed on a prefix that already has refs.
    #[error("cannot set rule on {0}: names already exist under it")]
    RuleChangeOnNonEmpty(String),
    /// Stored data failed a checksum, hash or format check.
    #[error("corrupt data: {0}")]
    Corrupt(String),
    /// Invalid input (arguments, configuration, text formats).
    #[error("parse error: {0}")]
    Parse(String),
    /// The operation isn't supported by this implementation.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// Another process holds a lock that is required exclusively.
    #[error("repository is locked by another process")]
    Locked,
    /// Authentication or authorization failed (API v1.1).
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// An operating-system I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type of all pico2peta APIs.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable machine-readable code (used by the server / client mapping).
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::KeyNotFound(_) => "key_not_found",
            Error::Backend { .. } => "backend",
            Error::InvalidRange { .. } => "invalid_range",
            Error::InvalidName(..) => "invalid_name",
            Error::TargetNotInReadSet(_) => "target_not_in_read_set",
            Error::MissingObject(_) => "missing_object",
            Error::Forbidden { .. } => "forbidden",
            Error::RuleChangeOnNonEmpty(_) => "rule_change_on_non_empty",
            Error::Corrupt(_) => "corrupt",
            Error::Parse(_) => "parse",
            Error::Unsupported(_) => "unsupported",
            Error::Locked => "locked",
            Error::Unauthorized(_) => "unauthorized",
            Error::Io(_) => "io",
        }
    }
}

/// Validates `range` against an object of length `len`. Shared by every
/// implementation (and the model) so that range semantics agree exactly.
pub fn check_range(range: Option<Range<u64>>, len: u64) -> Result<Range<u64>> {
    match range {
        None => Ok(0..len),
        Some(r) if r.start <= r.end && r.end <= len => Ok(r),
        Some(r) => Err(Error::InvalidRange { start: r.start, end: r.end, len }),
    }
}

// ---------------------------------------------------------------------------
// Object store
// ---------------------------------------------------------------------------

/// The object layer API (v1). All methods take `&self`; implementations
/// synchronize internally and are safe to share across threads.
pub trait ObjectStore: Send + Sync {
    /// Stores `data` (idempotently) and returns its content hash.
    fn put(&self, data: &[u8]) -> Result<Hash>;
    /// Streams an object of unknown length in with bounded memory.
    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash>;
    /// Streams an object in, stored as a byte-wise XOR delta against `base`
    /// (API v1.7, the ZipLLM-style storage for versions of model weights). The
    /// identity is still the content hash of the full object. Reads, ranges
    /// and GC are unchanged for callers (GC keeps `base` alive). Worth it when
    /// the new object is a small perturbation of `base` at the same offsets.
    /// Default: ignores `base` and stores normally.
    fn put_delta(&self, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
        let _ = base;
        self.put_stream(src)
    }
    /// Like `put_delta`, with an explicit alignment (API v1.8): each
    /// `(dst, src, len)` says that object bytes `[dst, dst+len)` correspond to
    /// base bytes `[src, src+len)` (sorted by `dst`, non-overlapping). Uncovered
    /// bytes are stored as-is. Used for formats whose regions move between
    /// versions (reordered or resized tensors). Default: `put_delta`.
    fn put_delta_mapped(&self, base: &Hash, segments: &[(u64, u64, u64)], src: &mut dyn Read) -> Result<Hash> {
        let _ = segments;
        self.put_delta(base, src)
    }
    /// `put_stream` with a storage hint (API v1.12): the content is
    /// fixed-width numeric data with `width`-byte elements (2 = bf16/fp16,
    /// 4 = fp32), so the store may byte-group it before compressing. The hint
    /// only affects encoding: identity and reads are unchanged, and a wrong
    /// hint costs only compression ratio. Default: ignores the hint.
    fn put_stream_grouped(&self, src: &mut dyn Read, width: u8) -> Result<Hash> {
        let _ = width;
        self.put_stream(src)
    }
    /// `put_delta_mapped` with the same hint for the delta (API v1.12).
    /// Default: ignores the hint.
    fn put_delta_mapped_grouped(
        &self,
        base: &Hash,
        segments: &[(u64, u64, u64)],
        src: &mut dyn Read,
        width: u8,
    ) -> Result<Hash> {
        let _ = width;
        self.put_delta_mapped(base, segments, src)
    }
    /// Returns the bytes of `hash` in `range` (whole object if `None`).
    /// Buffers the result, so use [`ObjectStore::open`] for large objects.
    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>>;
    /// Streams the bytes of `hash` in `range` with bounded memory. The
    /// reader is independent of `&self`'s lifetime. Content is verified as it
    /// streams, and corruption shows up as an `io::Error` wrapping
    /// `Error::Corrupt`.
    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>>;
    /// Length of the object, or `NotFound`.
    fn size(&self, hash: &Hash) -> Result<u64>;
    /// Whether `hash` is stored (any copy, including one GC has condemned; see `ensure_live`).
    fn contains(&self, hash: &Hash) -> Result<bool>;
    /// Like `contains`, but also guarantees that the object (and everything
    /// it is physically made of) is in **live**, non-condemned storage,
    /// rewriting it there if GC has condemned the only copy. Use this, not
    /// `contains`, before making something reference an object without
    /// re-putting it (API v1.3). The rewrite is durable after the next `flush`.
    /// Default: `contains` (for stores without GC).
    fn ensure_live(&self, hash: &Hash) -> Result<bool> {
        self.contains(hash)
    }
    /// Makes every previous `put` durable **and visible to other processes
    /// and handles** that open the same repository.
    fn flush(&self) -> Result<()>;
}

/// The full repository API (v1): objects plus names. A local repo
/// (`pico2peta-repo`) and the network client (`pico2peta-client`) both
/// implement it, and the CLI and server are written against it.
pub trait Repository: ObjectStore {
    /// Current value of `name`, or `None`.
    fn resolve(&self, name: &Name) -> Result<Option<RefValue>>;
    /// OCC compare-and-swap. `Ok(ReplaceOutcome::Conflict)` is a normal
    /// outcome, not an error. Errors are input or rule violations
    /// (`TargetNotInReadSet`, `MissingObject`, `Forbidden`) or I/O. Check
    /// order: input validation, then the read set (`Conflict`), then the
    /// rule (`Forbidden`). On `Ok`, the new value is durable.
    fn replace(&self, name: &Name, read_set: &[(Name, Observed)], new: Hash) -> Result<ReplaceOutcome>;
    /// Atomic multi-name OCC replace (API v1.2): every `(name, new)` commits
    /// together or nothing does. Every target must be in `read_set`. Rules
    /// are checked per name. Default: single-name updates delegate to
    /// `replace`, and larger batches are `Unsupported`.
    fn replace_many(&self, updates: &[(Name, Hash)], read_set: &[(Name, Observed)]) -> Result<ReplaceManyOutcome> {
        match updates {
            [(n, h)] => Ok(match self.replace(n, read_set, *h)? {
                ReplaceOutcome::Ok { version } => ReplaceManyOutcome::Ok { versions: vec![version] },
                ReplaceOutcome::Conflict { stale } => ReplaceManyOutcome::Conflict { stale },
            }),
            _ => Err(Error::Unsupported("replace_many is not supported by this repository")),
        }
    }
    /// Atomic multi-name update where `None` deletes (API v1.6), e.g. for
    /// `git push --atomic` with deletions. Same validation as `replace_many`.
    /// Each entry follows the single-name rules. Default: if every entry is
    /// `Some`, delegates to `replace_many`; otherwise `Unsupported`.
    fn update_many(
        &self,
        updates: &[(Name, Option<Hash>)],
        read_set: &[(Name, Observed)],
    ) -> Result<ReplaceManyOutcome> {
        let sets: Option<Vec<(Name, Hash)>> = updates.iter().map(|(n, h)| h.map(|h| (n.clone(), h))).collect();
        match sets {
            Some(sets) => self.replace_many(&sets, read_set),
            None => Err(Error::Unsupported("update_many with deletes is not supported by this repository")),
        }
    }
    /// OCC delete of a ref (API v1.5). It obeys namespace rules: create-once
    /// names can't be deleted. Re-creating a deleted name continues its
    /// version sequence. Deleting an absent name is `Ok { version: 0 }`.
    fn delete_ref(&self, name: &Name, read_set: &[(Name, Observed)]) -> Result<ReplaceOutcome> {
        let _ = (name, read_set);
        Err(Error::Unsupported("delete_ref is not supported by this repository"))
    }
    /// All refs under `prefix` ("" = all), sorted by name.
    fn list_refs(&self, prefix: &str) -> Result<Vec<(Name, RefValue)>>;
    /// Sets the rule for namespace `prefix`. Fails with
    /// `RuleChangeOnNonEmpty` if any ref already exists under it.
    fn set_rule(&self, prefix: &str, rule: Rule) -> Result<()>;
    /// All configured `(prefix, rule)` pairs, sorted by prefix.
    fn rules(&self) -> Result<Vec<(String, Rule)>>;
}

/// Trivial in-memory object store, used in tests and as a reference.
#[derive(Default, Debug)]
pub struct MemStore {
    objects: std::sync::RwLock<HashMap<Hash, std::sync::Arc<Vec<u8>>>>,
}

impl MemStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of stored objects.
    pub fn len(&self) -> usize {
        self.objects.read().map(|m| m.len()).unwrap_or(0)
    }
    /// True if nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn obj(&self, hash: &Hash) -> Result<std::sync::Arc<Vec<u8>>> {
        let m = self.objects.read().map_err(|_| Error::Corrupt("poisoned lock".into()))?;
        m.get(hash).cloned().ok_or(Error::NotFound(*hash))
    }
}

impl ObjectStore for MemStore {
    fn put(&self, data: &[u8]) -> Result<Hash> {
        let h = Hash::of(data);
        let mut m = self.objects.write().map_err(|_| Error::Corrupt("poisoned lock".into()))?;
        m.entry(h).or_insert_with(|| std::sync::Arc::new(data.to_vec()));
        Ok(h)
    }
    fn put_stream(&self, src: &mut dyn Read) -> Result<Hash> {
        let mut v = Vec::new();
        src.read_to_end(&mut v)?;
        self.put(&v)
    }
    fn get(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Vec<u8>> {
        let d = self.obj(hash)?;
        let r = check_range(range, d.len() as u64)?;
        Ok(d[r.start as usize..r.end as usize].to_vec())
    }
    fn open(&self, hash: &Hash, range: Option<Range<u64>>) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::io::Cursor::new(self.get(hash, range)?)))
    }
    fn size(&self, hash: &Hash) -> Result<u64> {
        Ok(self.obj(hash)?.len() as u64)
    }
    fn contains(&self, hash: &Hash) -> Result<bool> {
        Ok(self.obj(hash).is_ok())
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Names, rules, read sets
// ---------------------------------------------------------------------------

/// A validated ref name: `/`-separated non-empty segments of
/// `[A-Za-z0-9._+-]`, no `.`/`..` segments, at most 1024 bytes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    /// Validates and wraps a name (see the type docs for the grammar).
    pub fn new(s: impl Into<String>) -> Result<Name> {
        let s = s.into();
        validate_path(&s)?;
        Ok(Name(s))
    }
    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// True if `self` is `prefix` or lies under `prefix/`.
    pub fn is_under(&self, prefix: &str) -> bool {
        is_under(&self.0, prefix)
    }
}

/// True if path `s` equals `prefix` or lies under `prefix/` (segment-aligned).
/// The empty prefix matches everything.
pub fn is_under(s: &str, prefix: &str) -> bool {
    prefix.is_empty() || (s.starts_with(prefix) && (s.len() == prefix.len() || s.as_bytes()[prefix.len()] == b'/'))
}

/// Validates a name or namespace prefix (same grammar).
pub fn validate_path(s: &str) -> Result<()> {
    let bad = |why| Err(Error::InvalidName(s.to_string(), why));
    if s.is_empty() {
        return bad("empty");
    }
    if s.len() > 1024 {
        return bad("longer than 1024 bytes");
    }
    for seg in s.split('/') {
        if seg.is_empty() {
            return bad("empty segment");
        }
        if seg == "." || seg == ".." {
            return bad("'.' or '..' segment");
        }
        if !seg.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+')) {
            return bad("segments may only contain [A-Za-z0-9._+-]");
        }
    }
    Ok(())
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
impl FromStr for Name {
    type Err = Error;
    fn from_str(s: &str) -> Result<Name> {
        Name::new(s)
    }
}

/// Per-namespace name update rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// Any validated replace succeeds (source code branches).
    Mutable,
    /// A name may be created once and never moved (published packages).
    ImmutableAfterPublish,
    /// `<ns>/heads/...` mutable, `<ns>/releases/...` create-once, anything
    /// else under `<ns>` forbidden (model weights).
    HeadMutableReleaseImmutable,
}

impl Rule {
    /// Canonical spelling (`mutable`, `immutable`, `head-release`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Rule::Mutable => "mutable",
            Rule::ImmutableAfterPublish => "immutable",
            Rule::HeadMutableReleaseImmutable => "head-release",
        }
    }
    /// Stable one-byte code used in on-disk formats.
    pub fn code(&self) -> u8 {
        match self {
            Rule::Mutable => 0,
            Rule::ImmutableAfterPublish => 1,
            Rule::HeadMutableReleaseImmutable => 2,
        }
    }
    /// Inverse of [`Rule::code`].
    pub fn from_code(c: u8) -> Option<Rule> {
        Some(match c {
            0 => Rule::Mutable,
            1 => Rule::ImmutableAfterPublish,
            2 => Rule::HeadMutableReleaseImmutable,
            _ => return None,
        })
    }
}

impl FromStr for Rule {
    type Err = Error;
    fn from_str(s: &str) -> Result<Rule> {
        Ok(match s {
            "mutable" => Rule::Mutable,
            "immutable" | "immutable-after-publish" => Rule::ImmutableAfterPublish,
            "head-release" | "head-mutable-release-immutable" => Rule::HeadMutableReleaseImmutable,
            _ => return Err(Error::Parse(format!("unknown rule {s:?} (mutable|immutable|head-release)"))),
        })
    }
}

/// What the effective rule says about a single name move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mobility {
    /// May move freely.
    Mutable,
    /// May only be created (from `Absent`).
    CreateOnce,
    /// May never be written.
    Forbidden(&'static str),
}

/// Pure decision: given the rule of the namespace `prefix` that governs
/// `name`, how may `name` move? Shared by implementation, model and (in
/// spirit) the Lean development.
pub fn mobility(rule: Rule, prefix: &str, name: &Name) -> Mobility {
    match rule {
        Rule::Mutable => Mobility::Mutable,
        Rule::ImmutableAfterPublish => Mobility::CreateOnce,
        Rule::HeadMutableReleaseImmutable => {
            let rest =
                if prefix.is_empty() { name.as_str() } else { name.as_str().get(prefix.len() + 1..).unwrap_or("") };
            if rest.strip_prefix("heads/").is_some_and(|r| !r.is_empty()) {
                return Mobility::Mutable;
            }
            if rest.strip_prefix("releases/").is_some_and(|r| !r.is_empty()) {
                return Mobility::CreateOnce;
            }
            Mobility::Forbidden("only heads/* and releases/* may be written in a head-release namespace")
        }
    }
}

/// The state of one name as observed by a reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Observed {
    /// The name did not exist.
    Absent,
    /// Present at this version (versions start at 1 and increase by 1 per change).
    Version(u64),
}

/// The current value of a ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefValue {
    /// Object the name points at.
    pub hash: Hash,
    /// Version, starting at 1 and incremented on every change (continuing across deletes).
    pub version: u64,
}

impl RefValue {
    /// What a reader observed, given a `resolve` result.
    pub fn observed(v: Option<RefValue>) -> Observed {
        match v {
            None => Observed::Absent,
            Some(r) => Observed::Version(r.version),
        }
    }
}

/// The OCC read set: every name the writer's decision depended on, with the
/// state it observed.
pub type ReadSet = Vec<(Name, Observed)>;

/// Result of a `replace` that passed input validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplaceOutcome {
    /// Committed; the name is now at `version`.
    Ok {
        /// The new version.
        version: u64,
    },
    /// Some read-set entries no longer match. Nothing changed.
    Conflict {
        /// `(name, current state)` for each stale entry.
        stale: Vec<(Name, Observed)>,
    },
}

/// Result of a `replace_many` that passed input validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplaceManyOutcome {
    /// All updates committed atomically.
    Ok {
        /// New versions, in update order.
        versions: Vec<u64>,
    },
    /// Some read-set entries no longer match; nothing was changed.
    Conflict {
        /// `(name, current state)` for each stale entry.
        stale: Vec<(Name, Observed)>,
    },
}

/// Finds the governing namespace for `name`: the longest prefix in `rules`
/// that `name` lies under; `(default, "")` if none.
pub fn governing<'a, I>(rules: I, default: Rule, name: &Name) -> (Rule, String)
where
    I: IntoIterator<Item = (&'a String, &'a Rule)>,
{
    let mut best: Option<(&String, Rule)> = None;
    for (p, r) in rules {
        if name.is_under(p) && best.is_none_or(|(bp, _)| p.len() > bp.len()) {
            best = Some((p, *r));
        }
    }
    match best {
        Some((p, r)) => (r, p.clone()),
        None => (default, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_hex_roundtrip() {
        let h = Hash::of(b"hello");
        assert_eq!(h.to_string().parse::<Hash>().unwrap(), h);
        assert!("zz".parse::<Hash>().is_err());
    }

    #[test]
    fn names() {
        assert!(Name::new("a/b-c/1.0").is_ok());
        for bad in ["", "/a", "a/", "a//b", "a/../b", "a b", "a/./b"] {
            assert!(Name::new(bad).is_err(), "{bad}");
        }
        let n = Name::new("pkg/foo").unwrap();
        assert!(n.is_under("pkg"));
        assert!(n.is_under("pkg/foo"));
        assert!(!n.is_under("pk"));
        assert!(!n.is_under("pkg/foo/bar"));
    }

    #[test]
    fn mobility_head_release() {
        let r = Rule::HeadMutableReleaseImmutable;
        let n = |s| Name::new(s).unwrap();
        assert_eq!(mobility(r, "m", &n("m/heads/main")), Mobility::Mutable);
        assert_eq!(mobility(r, "m", &n("m/releases/v1")), Mobility::CreateOnce);
        assert!(matches!(mobility(r, "m", &n("m/other")), Mobility::Forbidden(_)));
        assert!(matches!(mobility(r, "m", &n("m")), Mobility::Forbidden(_)));
        assert!(matches!(mobility(r, "m", &n("m/heads")), Mobility::Forbidden(_)));
    }

    #[test]
    fn memstore_ranges() {
        let s = MemStore::new();
        let h = s.put(b"abcdef").unwrap();
        assert_eq!(s.get(&h, Some(1..3)).unwrap(), b"bc");
        assert_eq!(s.get(&h, Some(6..6)).unwrap(), b"");
        assert!(s.get(&h, Some(3..7)).is_err());
        assert!(matches!(s.get(&Hash::of(b"x"), None), Err(Error::NotFound(_))));
    }
}
