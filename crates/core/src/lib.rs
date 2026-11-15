//! Core types shared by every pico2peta layer: content hashes, names, read sets,
//! namespace rules, errors, and the `ObjectStore` trait.
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
    pub const LEN: usize = 32;

    pub fn of(data: &[u8]) -> Hash {
        Hash(*blake3::hash(data).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("object not found: {0}")]
    NotFound(Hash),
    #[error("backend key not found: {0}")]
    KeyNotFound(String),
    #[error("backend error (transient={transient}): {msg}")]
    Backend { transient: bool, msg: String },
    #[error("invalid range {start}..{end} for object of length {len}")]
    InvalidRange { start: u64, end: u64, len: u64 },
    #[error("invalid name {0:?}: {1}")]
    InvalidName(String, &'static str),
    #[error("replace target {0} is not in the read set")]
    TargetNotInReadSet(Name),
    #[error("object {0} does not exist; refusing to point a name at it")]
    MissingObject(Hash),
    #[error("forbidden by namespace rule {rule:?} for {name}: {why}")]
    Forbidden { name: Name, rule: Rule, why: &'static str },
    #[error("cannot set rule on {0}: names already exist under it")]
    RuleChangeOnNonEmpty(String),
    #[error("corrupt data: {0}")]
    Corrupt(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("repository is locked by another process")]
    Locked,
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

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
    fn contains(&self, hash: &Hash) -> Result<bool>;
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
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.objects.read().map(|m| m.len()).unwrap_or(0)
    }
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
    pub fn new(s: impl Into<String>) -> Result<Name> {
        let s = s.into();
        validate_path(&s)?;
        Ok(Name(s))
    }
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
    pub fn as_str(&self) -> &'static str {
        match self {
            Rule::Mutable => "mutable",
            Rule::ImmutableAfterPublish => "immutable",
            Rule::HeadMutableReleaseImmutable => "head-release",
        }
    }
    pub fn code(&self) -> u8 {
        match self {
            Rule::Mutable => 0,
            Rule::ImmutableAfterPublish => 1,
            Rule::HeadMutableReleaseImmutable => 2,
        }
    }
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
    Absent,
    /// Present at this version (versions start at 1 and increase by 1 per change).
    Version(u64),
}

/// The current value of a ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefValue {
    pub hash: Hash,
    pub version: u64,
}

impl RefValue {
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
    /// Committed; the name is now at this version.
    Ok { version: u64 },
    /// Some read-set entries no longer match. Lists `(name, current)`.
    Conflict { stale: Vec<(Name, Observed)> },
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
