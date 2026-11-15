//! Name layer: refs, OCC `replace`, per-namespace rules.
//!
//! # Storage
//! A single append-only journal `<dir>/journal`:
//! ```text
//! "P2PREFJ1"                                   8-byte versioned magic
//! { len: u32 | crc32(payload): u32 | payload } records
//! payload = 1 | name_len u16 | name | hash[32] | version u64      (ref set)
//!         | 2 | prefix_len u16 | prefix | rule u8                  (namespace rule)
//! ```
//! # Concurrency and the commit point
//! Every handle (in-process or in another process) keeps an in-memory copy
//! and the journal offset it has applied. Mutations take an **exclusive
//! `flock`** on the journal. Under that lock the handle catches up on
//! records other handles appended (truncating a torn tail left by a crash),
//! validates, appends one checksummed record, fsyncs, and unlocks. So
//! validation and commit are atomic across processes, and an `Ok` is durable.
//! Reads take a shared lock, which means they never observe a half-written
//! record from a live writer.

use pico2peta_core::fault::{FaultHandle, Faults};
use pico2peta_core::{
    Error, Hash, Mobility, Name, Observed, RefValue, ReplaceOutcome, Result, Rule, governing, is_under, mobility,
    validate_path,
};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAGIC: &[u8; 8] = b"P2PREFJ1";
const MAX_RECORD: u32 = 4096;

/// Pure in-memory ref state, plus the decision logic shared by every handle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefState {
    /// Current refs.
    pub refs: BTreeMap<Name, RefValue>,
    /// Namespace rules by prefix.
    pub rules: BTreeMap<String, Rule>,
}

/// The effect of a mutation that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// Point `name` at `value`.
    Set(Name, RefValue),
    /// Set the rule for a prefix.
    Rule(String, Rule),
}

impl RefState {
    /// Observed state of `name`.
    pub fn observe(&self, name: &Name) -> Observed {
        RefValue::observed(self.refs.get(name).copied())
    }

    /// Decides a `replace` against this state (pure). Order: input
    /// validation → read-set validation (`Conflict`) → rule (`Forbidden`).
    /// Object existence is checked by the caller beforehand.
    pub fn decide_replace(
        &self,
        name: &Name,
        read_set: &[(Name, Observed)],
        new: Hash,
    ) -> Result<std::result::Result<Mutation, ReplaceOutcome>> {
        if !read_set.iter().any(|(n, _)| n == name) {
            return Err(Error::TargetNotInReadSet(name.clone()));
        }
        let mut stale: Vec<(Name, Observed)> =
            read_set.iter().filter(|(n, o)| self.observe(n) != *o).map(|(n, _)| (n.clone(), self.observe(n))).collect();
        if !stale.is_empty() {
            stale.sort();
            stale.dedup();
            return Ok(Err(ReplaceOutcome::Conflict { stale }));
        }
        let (rule, prefix) = governing(self.rules.iter(), Rule::Mutable, name);
        let current = self.refs.get(name);
        match mobility(rule, &prefix, name) {
            Mobility::Mutable => {}
            Mobility::CreateOnce if current.is_none() => {}
            Mobility::CreateOnce => {
                return Err(Error::Forbidden { name: name.clone(), rule, why: "name is immutable once published" });
            }
            Mobility::Forbidden(why) => return Err(Error::Forbidden { name: name.clone(), rule, why }),
        }
        let version = current.map_or(1, |v| v.version + 1);
        Ok(Ok(Mutation::Set(name.clone(), RefValue { hash: new, version })))
    }

    /// Decides a `set_rule` (pure). Returns `None` when it's a no-op.
    pub fn decide_set_rule(&self, prefix: &str, rule: Rule) -> Result<Option<Mutation>> {
        validate_path(prefix)?;
        if self.rules.get(prefix) == Some(&rule) {
            return Ok(None);
        }
        if self.refs.keys().any(|n| is_under(n.as_str(), prefix)) {
            return Err(Error::RuleChangeOnNonEmpty(prefix.to_string()));
        }
        Ok(Some(Mutation::Rule(prefix.to_string(), rule)))
    }

    /// Applies a mutation.
    pub fn apply(&mut self, m: Mutation) {
        match m {
            Mutation::Set(n, v) => {
                self.refs.insert(n, v);
            }
            Mutation::Rule(p, r) => {
                self.rules.insert(p, r);
            }
        }
    }

    /// Refs under `prefix` ("" = all), sorted.
    pub fn list(&self, prefix: &str) -> Vec<(Name, RefValue)> {
        self.refs.iter().filter(|(n, _)| is_under(n.as_str(), prefix)).map(|(n, v)| (n.clone(), *v)).collect()
    }
}

fn encode(m: &Mutation) -> Vec<u8> {
    let mut p = Vec::new();
    match m {
        Mutation::Set(n, v) => {
            p.push(1);
            p.extend_from_slice(&(n.as_str().len() as u16).to_le_bytes());
            p.extend_from_slice(n.as_str().as_bytes());
            p.extend_from_slice(v.hash.as_bytes());
            p.extend_from_slice(&v.version.to_le_bytes());
        }
        Mutation::Rule(prefix, r) => {
            p.push(2);
            p.extend_from_slice(&(prefix.len() as u16).to_le_bytes());
            p.extend_from_slice(prefix.as_bytes());
            p.push(r.code());
        }
    }
    let mut rec = Vec::with_capacity(8 + p.len());
    rec.extend_from_slice(&(p.len() as u32).to_le_bytes());
    rec.extend_from_slice(&crc32fast::hash(&p).to_le_bytes());
    rec.extend_from_slice(&p);
    rec
}

/// Decodes a payload (already CRC-checked).
pub fn decode_payload(p: &[u8]) -> Result<Mutation> {
    let bad = |w: &str| Error::Corrupt(format!("ref journal: {w}"));
    let (&tag, rest) = p.split_first().ok_or_else(|| bad("empty record"))?;
    if rest.len() < 2 {
        return Err(bad("short record"));
    }
    let n = u16::from_le_bytes([rest[0], rest[1]]) as usize;
    let rest = &rest[2..];
    let s = rest.get(..n).ok_or_else(|| bad("short name"))?;
    let s = std::str::from_utf8(s).map_err(|_| bad("non-utf8 name"))?;
    let rest = &rest[n..];
    match tag {
        1 => {
            if rest.len() != 40 {
                return Err(bad("bad set record length"));
            }
            let name = Name::new(s)?;
            let mut h = [0u8; 32];
            h.copy_from_slice(&rest[..32]);
            let mut v = [0u8; 8];
            v.copy_from_slice(&rest[32..40]);
            Ok(Mutation::Set(name, RefValue { hash: Hash(h), version: u64::from_le_bytes(v) }))
        }
        2 => {
            if rest.len() != 1 {
                return Err(bad("bad rule record length"));
            }
            validate_path(s)?;
            let r = Rule::from_code(rest[0]).ok_or_else(|| bad("unknown rule"))?;
            Ok(Mutation::Rule(s.to_string(), r))
        }
        _ => Err(bad("unknown record tag")),
    }
}

/// Parses as many complete, valid records as possible from `buf`.
/// Returns the mutations and the number of bytes consumed.
pub fn parse_records(buf: &[u8]) -> (Vec<Mutation>, usize, Option<Error>) {
    let mut out = Vec::new();
    let mut off = 0;
    while buf.len() - off >= 8 {
        let len = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        let crc = u32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
        if len > MAX_RECORD || buf.len() - off - 8 < len as usize {
            break;
        }
        let p = &buf[off + 8..off + 8 + len as usize];
        if crc32fast::hash(p) != crc {
            break;
        }
        match decode_payload(p) {
            Ok(m) => out.push(m),
            // A CRC-valid but undecodable record is real corruption (or a newer format).
            Err(e) => return (out, off, Some(e)),
        }
        off += 8 + len as usize;
    }
    (out, off, None)
}

struct Inner {
    file: File,
    state: RefState,
    /// Journal offset up to which `state` is applied.
    applied: u64,
}

/// A handle on a ref journal. Thread-safe. Open one per process (or more).
pub struct RefStore {
    path: PathBuf,
    faults: FaultHandle,
    inner: Mutex<Inner>,
}

fn poisoned<T>(_: T) -> Error {
    Error::Corrupt("internal lock poisoned".into())
}

/// Releases a flock on drop. Holds a `dup` of the locked file, which shares
/// the open file description and therefore the lock.
struct FlockGuard(File);
impl FlockGuard {
    fn exclusive(f: &File) -> Result<Self> {
        let d = f.try_clone()?;
        d.lock()?;
        Ok(FlockGuard(d))
    }
    fn shared(f: &File) -> Result<Self> {
        let d = f.try_clone()?;
        d.lock_shared()?;
        Ok(FlockGuard(d))
    }
}
impl Drop for FlockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl RefStore {
    /// Opens or creates the journal in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_faults(dir, Faults::disarmed())
    }

    /// Like [`RefStore::open`], with fault injection.
    pub fn open_with_faults(dir: impl AsRef<Path>, faults: FaultHandle) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join("journal");
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        {
            let _g = FlockGuard::exclusive(&file)?;
            let len = file.metadata()?.len();
            if len < MAGIC.len() as u64 {
                // New (or torn during creation). Initialize under the exclusive lock.
                file.set_len(0)?;
                file.write_all_at(MAGIC, 0)?;
                file.sync_all()?;
                pico2peta_core::backend::fsync_dir(dir)?;
            } else {
                let mut m = [0u8; 8];
                file.read_exact_at(&mut m, 0)?;
                if &m != MAGIC {
                    return Err(Error::Corrupt(format!("{} is not a pico2peta ref journal (v1)", path.display())));
                }
            }
        }
        let s = RefStore {
            path,
            faults,
            inner: Mutex::new(Inner { file, state: RefState::default(), applied: MAGIC.len() as u64 }),
        };
        {
            let mut g = s.inner.lock().map_err(poisoned)?;
            let _l = FlockGuard::shared(&g.file)?;
            Self::catch_up(&mut g, false)?;
        }
        Ok(s)
    }

    /// Applies records appended since `applied`. With `exclusive`, a torn tail is truncated.
    fn catch_up(g: &mut Inner, exclusive: bool) -> Result<()> {
        let len = g.file.metadata()?.len();
        if len < g.applied {
            return Err(Error::Corrupt("ref journal shrank below applied offset".into()));
        }
        if len == g.applied {
            return Ok(());
        }
        let mut buf = vec![0u8; (len - g.applied) as usize];
        g.file.read_exact_at(&mut buf, g.applied)?;
        let (muts, used, err) = parse_records(&buf);
        for m in muts {
            g.state.apply(m);
        }
        g.applied += used as u64;
        if let Some(e) = err {
            return Err(e);
        }
        if exclusive && g.applied < len {
            // Torn tail from a crashed writer: no live writer exists (we hold the lock).
            g.file.set_len(g.applied)?;
            g.file.sync_all()?;
        }
        Ok(())
    }

    fn with_shared<T>(&self, f: impl FnOnce(&RefState) -> T) -> Result<T> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        let _l = FlockGuard::shared(&g.file)?;
        Self::catch_up(&mut g, false)?;
        Ok(f(&g.state))
    }

    /// Runs `decide` under the exclusive lock and commits its mutation durably.
    fn commit<T>(&self, decide: impl FnOnce(&RefState) -> Result<(Option<Mutation>, T)>) -> Result<T> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        let _l = FlockGuard::exclusive(&g.file)?;
        Self::catch_up(&mut g, true)?;
        let (m, out) = decide(&g.state)?;
        if let Some(m) = m {
            let rec = encode(&m);
            if let Err(e) = self.faults.hit("refs.append") {
                // Simulated torn write.
                let _ = g.file.write_all_at(&rec[..rec.len() / 2], g.applied);
                return Err(e.into());
            }
            g.file.write_all_at(&rec, g.applied)?;
            self.faults.hit("refs.fsync")?;
            g.file.sync_data()?;
            g.applied += rec.len() as u64;
            g.state.apply(m);
        }
        Ok(out)
    }

    /// Current value of `name`.
    pub fn resolve(&self, name: &Name) -> Result<Option<RefValue>> {
        self.with_shared(|s| s.refs.get(name).copied())
    }

    /// OCC replace. `exists` reports whether `new` is a stored object.
    pub fn replace(
        &self,
        name: &Name,
        read_set: &[(Name, Observed)],
        new: Hash,
        exists: &dyn Fn(&Hash) -> Result<bool>,
    ) -> Result<ReplaceOutcome> {
        if !read_set.iter().any(|(n, _)| n == name) {
            return Err(Error::TargetNotInReadSet(name.clone()));
        }
        if !exists(&new)? {
            return Err(Error::MissingObject(new));
        }
        self.commit(|s| match s.decide_replace(name, read_set, new)? {
            Ok(m) => {
                let version = match &m {
                    Mutation::Set(_, v) => v.version,
                    Mutation::Rule(..) => 0,
                };
                Ok((Some(m), ReplaceOutcome::Ok { version }))
            }
            Err(conflict) => Ok((None, conflict)),
        })
    }

    /// Sets a namespace rule (see [`RefState::decide_set_rule`]).
    pub fn set_rule(&self, prefix: &str, rule: Rule) -> Result<()> {
        self.commit(|s| Ok((s.decide_set_rule(prefix, rule)?, ())))
    }

    /// Refs under `prefix`.
    pub fn list(&self, prefix: &str) -> Result<Vec<(Name, RefValue)>> {
        self.with_shared(|s| s.list(prefix))
    }

    /// Configured rules.
    pub fn rules(&self) -> Result<Vec<(String, Rule)>> {
        self.with_shared(|s| s.rules.iter().map(|(p, r)| (p.clone(), *r)).collect())
    }

    /// A snapshot of the whole state (after catching up).
    pub fn snapshot(&self) -> Result<RefState> {
        self.with_shared(|s| s.clone())
    }

    /// Journal path (for diagnostics and tests).
    pub fn journal_path(&self) -> &Path {
        &self.path
    }
}
