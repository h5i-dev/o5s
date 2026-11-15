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
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use pico2peta_core::fault::{FaultHandle, Faults};
use pico2peta_core::{
    Error, Hash, Mobility, Name, Observed, RefValue, ReplaceManyOutcome, ReplaceOutcome, Result, Rule, governing,
    is_under, mobility, validate_path,
};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAGIC: &[u8; 8] = b"P2PREFJ1";
const MAX_RECORD: u32 = 4 << 20;
/// Maximum number of names in one atomic `replace_many`.
pub const MAX_BATCH: usize = 10_000;

/// Pure in-memory ref state, plus the decision logic shared by every handle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefState {
    /// Current refs.
    pub refs: BTreeMap<Name, RefValue>,
    /// Namespace rules by prefix.
    pub rules: BTreeMap<String, Rule>,
    /// Last version of deleted names, so a re-created name continues its
    /// version sequence instead of restarting at 1.
    pub tombstones: BTreeMap<Name, u64>,
}

/// The effect of a mutation that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// Point `name` at `value`.
    Set(Name, RefValue),
    /// Set the rule for a prefix.
    Rule(String, Rule),
    /// Journal tombstone: this journal is closed, and state continues in
    /// generation `n` (snapshot + journal). Written only by compaction.
    Rotated(u64),
    /// Several `Set`s committed atomically (one record, one commit point).
    Batch(Vec<(Name, RefValue)>),
    /// Remove a name, remembering its last version (tombstone).
    Delete(Name, u64),
    /// Several `Set`/`Delete`s committed atomically (API v1.6 `update_many`).
    Txn(Vec<Mutation>),
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
        let version = match current {
            Some(v) => v.version + 1,
            None => self.tombstones.get(name).map_or(1, |t| t + 1),
        };
        Ok(Ok(Mutation::Set(name.clone(), RefValue { hash: new, version })))
    }

    /// Decides an atomic multi-name update (pure), where `None` deletes.
    /// Validation is as in [`RefState::decide_replace_many`]; each entry then follows
    /// the single-name rules (`decide_replace` / `decide_delete`) against the
    /// pre-state. Returns the mutation and the per-entry versions (deletes:
    /// the deleted version, 0 if absent).
    pub fn decide_update_many(
        &self,
        updates: &[(Name, Option<Hash>)],
        read_set: &[(Name, Observed)],
    ) -> Result<std::result::Result<(Mutation, Vec<u64>), ReplaceManyOutcome>> {
        if updates.is_empty() || updates.len() > MAX_BATCH {
            return Err(Error::Parse(format!("update_many takes 1..={MAX_BATCH} updates")));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (n, _) in updates {
            if !seen.insert(n) {
                return Err(Error::Parse(format!("duplicate target {n} in update_many")));
            }
            if !read_set.iter().any(|(r, _)| r == n) {
                return Err(Error::TargetNotInReadSet(n.clone()));
            }
        }
        let mut stale: Vec<(Name, Observed)> =
            read_set.iter().filter(|(n, o)| self.observe(n) != *o).map(|(n, _)| (n.clone(), self.observe(n))).collect();
        if !stale.is_empty() {
            stale.sort();
            stale.dedup();
            return Ok(Err(ReplaceManyOutcome::Conflict { stale }));
        }
        let mut ms = Vec::with_capacity(updates.len());
        let mut versions = Vec::with_capacity(updates.len());
        for (name, new) in updates {
            match new {
                Some(h) => match self.decide_replace(name, read_set, *h)? {
                    Ok(Mutation::Set(n, v)) => {
                        versions.push(v.version);
                        ms.push(Mutation::Set(n, v));
                    }
                    _ => return Err(Error::Corrupt("unexpected decision in update_many".into())),
                },
                None => match self.decide_delete(name, read_set)? {
                    Ok(Some(Mutation::Delete(n, v))) => {
                        versions.push(v);
                        ms.push(Mutation::Delete(n, v));
                    }
                    Ok(None) => versions.push(0),
                    _ => return Err(Error::Corrupt("unexpected decision in update_many".into())),
                },
            }
        }
        Ok(Ok((Mutation::Txn(ms), versions)))
    }

    /// Decides an OCC delete (pure). Deleting an absent name is a no-op
    /// (`Ok(None)`). Create-once and forbidden names can't be deleted.
    pub fn decide_delete(
        &self,
        name: &Name,
        read_set: &[(Name, Observed)],
    ) -> Result<std::result::Result<Option<Mutation>, ReplaceOutcome>> {
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
        let Some(cur) = self.refs.get(name) else { return Ok(Ok(None)) };
        let (rule, prefix) = governing(self.rules.iter(), Rule::Mutable, name);
        match mobility(rule, &prefix, name) {
            Mobility::Mutable => Ok(Ok(Some(Mutation::Delete(name.clone(), cur.version)))),
            Mobility::CreateOnce => {
                Err(Error::Forbidden { name: name.clone(), rule, why: "immutable names can't be deleted" })
            }
            Mobility::Forbidden(why) => Err(Error::Forbidden { name: name.clone(), rule, why }),
        }
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
                self.tombstones.remove(&n);
                self.refs.insert(n, v);
            }
            Mutation::Delete(n, v) => {
                self.refs.remove(&n);
                self.tombstones.insert(n, v);
            }
            Mutation::Txn(ms) => {
                for m in ms {
                    self.apply(m);
                }
            }
            Mutation::Rule(p, r) => {
                self.rules.insert(p, r);
            }
            Mutation::Rotated(_) => {}
            Mutation::Batch(v) => {
                for (n, val) in v {
                    self.tombstones.remove(&n);
                    self.refs.insert(n, val);
                }
            }
        }
    }

    /// Decides an atomic multi-name replace (pure). Same order as
    /// [`RefState::decide_replace`]: input, then read set, then rules (each name).
    pub fn decide_replace_many(
        &self,
        updates: &[(Name, Hash)],
        read_set: &[(Name, Observed)],
    ) -> Result<std::result::Result<Mutation, ReplaceManyOutcome>> {
        if updates.is_empty() || updates.len() > MAX_BATCH {
            return Err(Error::Parse(format!("replace_many takes 1..={MAX_BATCH} updates")));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (n, _) in updates {
            if !seen.insert(n) {
                return Err(Error::Parse(format!("duplicate target {n} in replace_many")));
            }
            if !read_set.iter().any(|(r, _)| r == n) {
                return Err(Error::TargetNotInReadSet(n.clone()));
            }
        }
        let mut stale: Vec<(Name, Observed)> =
            read_set.iter().filter(|(n, o)| self.observe(n) != *o).map(|(n, _)| (n.clone(), self.observe(n))).collect();
        if !stale.is_empty() {
            stale.sort();
            stale.dedup();
            return Ok(Err(ReplaceManyOutcome::Conflict { stale }));
        }
        let mut sets = Vec::with_capacity(updates.len());
        for (name, new) in updates {
            match self.decide_replace(name, read_set, *new)? {
                Ok(Mutation::Set(n, v)) => sets.push((n, v)),
                Ok(_) => return Err(Error::Corrupt("unexpected mutation".into())),
                Err(_) => return Err(Error::Corrupt("read set changed during decision".into())),
            }
        }
        Ok(Ok(Mutation::Batch(sets)))
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
        Mutation::Rotated(g) => {
            p.push(3);
            p.extend_from_slice(&g.to_le_bytes());
        }
        Mutation::Delete(n, ver) => {
            p.push(5);
            p.extend_from_slice(&(n.as_str().len() as u16).to_le_bytes());
            p.extend_from_slice(n.as_str().as_bytes());
            p.extend_from_slice(&ver.to_le_bytes());
        }
        Mutation::Txn(ms) => {
            p.push(6);
            p.extend_from_slice(&(ms.len() as u32).to_le_bytes());
            for m in ms {
                let inner = encode(m);
                // Reuse the record framing: len u32 | crc u32 | payload.
                p.extend_from_slice(&inner);
            }
        }
        Mutation::Batch(v) => {
            p.push(4);
            p.extend_from_slice(&(v.len() as u32).to_le_bytes());
            for (n, val) in v {
                p.extend_from_slice(&(n.as_str().len() as u16).to_le_bytes());
                p.extend_from_slice(n.as_str().as_bytes());
                p.extend_from_slice(val.hash.as_bytes());
                p.extend_from_slice(&val.version.to_le_bytes());
            }
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
    if tag == 6 {
        let cnt: [u8; 4] = rest.get(..4).and_then(|b| b.try_into().ok()).ok_or_else(|| bad("short txn"))?;
        let cnt = u32::from_le_bytes(cnt) as usize;
        if cnt > MAX_BATCH {
            return Err(bad("bad txn size"));
        }
        let (inner, used, err) = parse_records(&rest[4..]);
        if err.is_some() || used != rest.len() - 4 || inner.len() != cnt {
            return Err(bad("bad txn body"));
        }
        if inner.iter().any(|m| !matches!(m, Mutation::Set(..) | Mutation::Delete(..))) {
            return Err(bad("txn may only contain set/delete"));
        }
        return Ok(Mutation::Txn(inner));
    }
    if tag == 4 {
        let cnt: [u8; 4] = rest.get(..4).and_then(|b| b.try_into().ok()).ok_or_else(|| bad("short batch"))?;
        let cnt = u32::from_le_bytes(cnt) as usize;
        if cnt == 0 || cnt > MAX_BATCH {
            return Err(bad("bad batch size"));
        }
        let mut rest = &rest[4..];
        let mut v = Vec::with_capacity(cnt);
        for _ in 0..cnt {
            let l = rest.get(..2).ok_or_else(|| bad("short batch entry"))?;
            let l = u16::from_le_bytes([l[0], l[1]]) as usize;
            let e = rest.get(2..2 + l + 40).ok_or_else(|| bad("short batch entry"))?;
            let name = Name::new(std::str::from_utf8(&e[..l]).map_err(|_| bad("non-utf8 name"))?)?;
            let mut h = [0u8; 32];
            h.copy_from_slice(&e[l..l + 32]);
            let mut ver = [0u8; 8];
            ver.copy_from_slice(&e[l + 32..l + 40]);
            v.push((name, RefValue { hash: Hash(h), version: u64::from_le_bytes(ver) }));
            rest = &rest[2 + l + 40..];
        }
        if !rest.is_empty() {
            return Err(bad("trailing bytes in batch"));
        }
        return Ok(Mutation::Batch(v));
    }
    if tag == 3 {
        let g: [u8; 8] = rest.try_into().map_err(|_| bad("bad rotation record"))?;
        return Ok(Mutation::Rotated(u64::from_le_bytes(g)));
    }
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
        5 => {
            let v: [u8; 8] = rest.try_into().map_err(|_| bad("bad delete record length"))?;
            Ok(Mutation::Delete(Name::new(s)?, u64::from_le_bytes(v)))
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
    /// Journal generation of `file` (0 = `journal`, n = `journal.<n>`).
    generation: u64,
    state: RefState,
    /// Journal offset up to which `state` is applied.
    applied: u64,
}

/// A handle on a ref store directory. Thread-safe. Any number of handles,
/// in any number of processes, may share a directory.
///
/// # Compaction
/// When the journal exceeds the compaction threshold, the committer (holding
/// the exclusive lock) writes `snapshot.<n+1>` (the full state) and an empty
/// `journal.<n+1>`, then appends a `Rotated(n+1)` tombstone to the old journal.
/// The tombstone is the commit point. Last, it updates `CURRENT`. A handle
/// that catches up on an old journal and reaches the tombstone switches to the
/// new generation, so no write is ever made to a rotated journal.
struct Journal {
    dir: PathBuf,
    faults: FaultHandle,
    compact_threshold: std::sync::atomic::AtomicU64,
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

const SNAP_MAGIC: &[u8; 8] = b"P2PREFS1";
/// Default journal size that triggers compaction.
pub const DEFAULT_COMPACT_THRESHOLD: u64 = 64 << 20;

fn journal_name(g: u64) -> String {
    if g == 0 { "journal".into() } else { format!("journal.{g}") }
}

/// Writes `bytes` to `dir/name` atomically (temp file, fsync, rename, dir fsync).
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(name))?;
    pico2peta_core::backend::fsync_dir(dir)
}

fn encode_snapshot(st: &RefState) -> Vec<u8> {
    let mut v = SNAP_MAGIC.to_vec();
    for (p, r) in &st.rules {
        v.extend_from_slice(&encode(&Mutation::Rule(p.clone(), *r)));
    }
    for (n, val) in &st.refs {
        v.extend_from_slice(&encode(&Mutation::Set(n.clone(), *val)));
    }
    for (n, ver) in &st.tombstones {
        v.extend_from_slice(&encode(&Mutation::Delete(n.clone(), *ver)));
    }
    let crc = crc32fast::hash(&v);
    v.extend_from_slice(&crc.to_le_bytes());
    v
}

fn decode_snapshot(b: &[u8]) -> Result<RefState> {
    let bad = || Error::Corrupt("ref snapshot is corrupt".into());
    if b.len() < 12 || &b[..8] != SNAP_MAGIC {
        return Err(bad());
    }
    let (body, crc) = b.split_at(b.len() - 4);
    if crc32fast::hash(body).to_le_bytes() != crc {
        return Err(bad());
    }
    let (muts, used, err) = parse_records(&body[8..]);
    if err.is_some() || used != body.len() - 8 {
        return Err(bad());
    }
    let mut st = RefState::default();
    for m in muts {
        st.apply(m);
    }
    Ok(st)
}

fn read_current(dir: &Path) -> Result<u64> {
    match std::fs::read_to_string(dir.join("CURRENT")) {
        Ok(s) => s.trim().parse().map_err(|_| Error::Corrupt("bad refs/CURRENT".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

impl Journal {
    /// Opens or creates the ref store in `dir`.
    fn open_with_faults(dir: impl AsRef<Path>, faults: FaultHandle) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let mut last_err = None;
        // Retry: a concurrent compaction may delete the generation we just read from CURRENT.
        for _ in 0..8 {
            match Self::load_generation(&dir, read_current(&dir)?) {
                Ok(inner) => {
                    let s = Journal {
                        dir,
                        faults,
                        compact_threshold: std::sync::atomic::AtomicU64::new(DEFAULT_COMPACT_THRESHOLD),
                        inner: Mutex::new(inner),
                    };
                    s.with_shared(|_| ())?;
                    return Ok(s);
                }
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => last_err = Some(Error::Io(e)),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or(Error::Corrupt("could not open ref store".into())))
    }

    /// Opens generation `g`: its snapshot (none for 0) and its journal.
    fn load_generation(dir: &Path, g: u64) -> Result<Inner> {
        let path = dir.join(journal_name(g));
        let state = if g == 0 {
            RefState::default()
        } else {
            decode_snapshot(&std::fs::read(dir.join(format!("snapshot.{g}")))?)?
        };
        let file = if g == 0 {
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?
        } else {
            OpenOptions::new().read(true).write(true).open(&path)?
        };
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
        Ok(Inner { file, generation: g, state, applied: MAGIC.len() as u64 })
    }

    /// Switches `g` to generation `next` after reading a tombstone. Falls back
    /// to `CURRENT` if `next` was itself compacted away already.
    fn switch(&self, g: &mut Inner, next: u64) -> Result<()> {
        let fresh = match Self::load_generation(&self.dir, next) {
            Ok(i) => i,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Self::load_generation(&self.dir, read_current(&self.dir)?)?
            }
            Err(e) => return Err(e),
        };
        if fresh.generation <= g.generation {
            return Err(Error::Corrupt("ref journal rotation went backwards".into()));
        }
        *g = fresh;
        Ok(())
    }

    /// Applies records appended since `applied`. With `exclusive`, a torn tail
    /// is truncated. Returns `true` if it switched generation; the caller
    /// must then re-lock the new journal and call again.
    fn catch_up(&self, g: &mut Inner, exclusive: bool) -> Result<bool> {
        let len = g.file.metadata()?.len();
        if len < g.applied {
            return Err(Error::Corrupt("ref journal shrank below applied offset".into()));
        }
        if len == g.applied {
            return Ok(false);
        }
        let mut buf = vec![0u8; (len - g.applied) as usize];
        g.file.read_exact_at(&mut buf, g.applied)?;
        let (muts, used, err) = parse_records(&buf);
        for m in muts {
            if let Mutation::Rotated(next) = m {
                self.switch(g, next)?;
                return Ok(true);
            }
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
        Ok(false)
    }

    fn with_shared<T>(&self, f: impl FnOnce(&RefState) -> T) -> Result<T> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        loop {
            let _l = FlockGuard::shared(&g.file)?;
            if !self.catch_up(&mut g, false)? {
                return Ok(f(&g.state));
            }
        }
    }

    /// Runs `decide` under the exclusive lock and commits its mutation durably.
    fn commit<T>(&self, decide: impl FnOnce(&RefState) -> Result<(Option<Mutation>, T)>) -> Result<T> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        let _l = loop {
            let l = FlockGuard::exclusive(&g.file)?;
            if !self.catch_up(&mut g, true)? {
                break l;
            }
        };
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
            if g.applied > self.compact_threshold.load(std::sync::atomic::Ordering::Relaxed) {
                // The commit itself is done. A failed compaction is retried on a later commit.
                let _ = self.rotate_locked(&mut g);
            }
        }
        Ok(out)
    }

    /// Compacts while holding the exclusive lock on `g.file`.
    fn rotate_locked(&self, g: &mut Inner) -> Result<()> {
        let next = g.generation + 1;
        self.faults.hit("refs.rotate.snapshot")?;
        write_atomic(&self.dir, &format!("snapshot.{next}"), &encode_snapshot(&g.state))?;
        self.faults.hit("refs.rotate.journal")?;
        write_atomic(&self.dir, &journal_name(next), MAGIC)?;
        let rec = encode(&Mutation::Rotated(next));
        if let Err(e) = self.faults.hit("refs.rotate.tombstone") {
            let _ = g.file.write_all_at(&rec[..rec.len() / 2], g.applied);
            return Err(e.into());
        }
        g.file.write_all_at(&rec, g.applied)?;
        g.file.sync_data()?;
        // Committed. From here on, every handle (including this one) moves to `next`.
        let old = g.generation;
        let fresh = Self::load_generation(&self.dir, next)?;
        *g = Inner { state: std::mem::take(&mut g.state), ..fresh };
        self.faults.hit("refs.rotate.current")?;
        write_atomic(&self.dir, "CURRENT", format!("{next}\n").as_bytes())?;
        // Old generations are only needed by handles that haven't caught up.
        // Those hold open descriptors, and a missing snapshot makes them fall
        // back to CURRENT.
        for gen_ in old.saturating_sub(1)..=old {
            let _ = std::fs::remove_file(self.dir.join(journal_name(gen_)));
            if gen_ > 0 {
                let _ = std::fs::remove_file(self.dir.join(format!("snapshot.{gen_}")));
            }
        }
        Ok(())
    }

    /// Sets the journal size (bytes) that triggers compaction.
    pub fn set_compact_threshold(&self, bytes: u64) {
        self.compact_threshold.store(bytes.max(64), std::sync::atomic::Ordering::Relaxed);
    }

    /// Forces a compaction now.
    pub fn compact(&self) -> Result<()> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        let _l = loop {
            let l = FlockGuard::exclusive(&g.file)?;
            if !self.catch_up(&mut g, true)? {
                break l;
            }
        };
        self.rotate_locked(&mut g)
    }

    /// Current journal generation (diagnostics).
    pub fn generation(&self) -> Result<u64> {
        Ok(self.inner.lock().map_err(poisoned)?.generation)
    }
}

/// The storage engine behind a [`RefStore`]: a consistent read, and a
/// linearizable commit of a decision. A decision may be evaluated more than
/// once (engines with optimistic commit retry it against a newer state).
trait Engine: Send + Sync {
    fn read(&self, f: &mut dyn FnMut(&RefState)) -> Result<()>;
    fn commit(&self, decide: &mut dyn FnMut(&RefState) -> Result<Option<Mutation>>) -> Result<()>;
    fn compact(&self) -> Result<()>;
    fn generation(&self) -> Result<u64>;
    fn set_compact_threshold(&self, bytes: u64);
}

impl Engine for Journal {
    fn read(&self, f: &mut dyn FnMut(&RefState)) -> Result<()> {
        self.with_shared(|s| f(s))
    }
    fn commit(&self, decide: &mut dyn FnMut(&RefState) -> Result<Option<Mutation>>) -> Result<()> {
        Journal::commit(self, |s| Ok((decide(s)?, ())))
    }
    fn compact(&self) -> Result<()> {
        Journal::compact(self)
    }
    fn generation(&self) -> Result<u64> {
        Journal::generation(self)
    }
    fn set_compact_threshold(&self, bytes: u64) {
        Journal::set_compact_threshold(self, bytes)
    }
}

/// A handle on a ref store. Thread-safe, and any number of handles may share
/// one store:
/// * [`RefStore::open`]: a local journal directory. Commits are serialized by an exclusive
///   `flock`, so all writers must be on one host.
/// * [`RefStore::open_backend`]: a create-only log on any [`Backend`], e.g. S3. Commits are
///   linearized by atomic conditional create, so writers may be on many hosts.
pub struct RefStore {
    engine: Box<dyn Engine>,
}

impl RefStore {
    /// Opens or creates a journal-backed ref store in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_faults(dir, Faults::disarmed())
    }

    /// Like [`RefStore::open`], with fault injection.
    pub fn open_with_faults(dir: impl AsRef<Path>, faults: FaultHandle) -> Result<Self> {
        Ok(RefStore { engine: Box::new(Journal::open_with_faults(dir, faults)?) })
    }

    /// Opens (creating if needed) a ref store stored as a create-only log under
    /// `prefix` on `backend` (see [`log`]).
    pub fn open_backend(backend: std::sync::Arc<dyn pico2peta_core::backend::Backend>, prefix: &str) -> Result<Self> {
        Self::open_backend_with_faults(backend, prefix, Faults::disarmed())
    }

    /// Like [`RefStore::open_backend`], with fault injection.
    pub fn open_backend_with_faults(
        backend: std::sync::Arc<dyn pico2peta_core::backend::Backend>,
        prefix: &str,
        faults: FaultHandle,
    ) -> Result<Self> {
        Ok(RefStore { engine: Box::new(log::LogEngine::open(backend, prefix, faults)?) })
    }

    fn read<T>(&self, f: impl FnOnce(&RefState) -> T) -> Result<T> {
        let mut f = Some(f);
        let mut out = None;
        self.engine.read(&mut |s| {
            if let Some(f) = f.take() {
                out = Some(f(s));
            }
        })?;
        out.ok_or_else(|| Error::Corrupt("ref engine did not read".into()))
    }

    fn commit<T>(&self, mut decide: impl FnMut(&RefState) -> Result<(Option<Mutation>, T)>) -> Result<T> {
        let mut out = None;
        self.engine.commit(&mut |s| {
            let (m, t) = decide(s)?;
            out = Some(t);
            Ok(m)
        })?;
        out.ok_or_else(|| Error::Corrupt("ref engine did not decide".into()))
    }

    /// Sets the size that triggers compaction (journal engine: bytes; log engine: entries).
    pub fn set_compact_threshold(&self, n: u64) {
        self.engine.set_compact_threshold(n)
    }

    /// Forces a compaction now.
    pub fn compact(&self) -> Result<()> {
        self.engine.compact()
    }

    /// Current journal generation, or log snapshot sequence (diagnostics).
    pub fn generation(&self) -> Result<u64> {
        self.engine.generation()
    }

    /// Current value of `name`.
    pub fn resolve(&self, name: &Name) -> Result<Option<RefValue>> {
        self.read(|s| s.refs.get(name).copied())
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
                    Mutation::Rule(..)
                    | Mutation::Rotated(_)
                    | Mutation::Batch(_)
                    | Mutation::Delete(..)
                    | Mutation::Txn(_) => 0,
                };
                Ok((Some(m), ReplaceOutcome::Ok { version }))
            }
            Err(conflict) => Ok((None, conflict)),
        })
    }

    /// Atomic multi-name OCC replace: either every update commits (in one
    /// journal record) or none does.
    pub fn replace_many(
        &self,
        updates: &[(Name, Hash)],
        read_set: &[(Name, Observed)],
        exists: &dyn Fn(&Hash) -> Result<bool>,
    ) -> Result<ReplaceManyOutcome> {
        for (n, h) in updates {
            if !read_set.iter().any(|(r, _)| r == n) {
                return Err(Error::TargetNotInReadSet(n.clone()));
            }
            if !exists(h)? {
                return Err(Error::MissingObject(*h));
            }
        }
        self.commit(|s| match s.decide_replace_many(updates, read_set)? {
            Ok(m) => {
                let versions = match &m {
                    Mutation::Batch(v) => v.iter().map(|(_, val)| val.version).collect(),
                    _ => Vec::new(),
                };
                Ok((Some(m), ReplaceManyOutcome::Ok { versions }))
            }
            Err(c) => Ok((None, c)),
        })
    }

    /// Atomic multi-name update where `None` deletes (API v1.6).
    pub fn update_many(
        &self,
        updates: &[(Name, Option<Hash>)],
        read_set: &[(Name, Observed)],
        exists: &dyn Fn(&Hash) -> Result<bool>,
    ) -> Result<ReplaceManyOutcome> {
        for (n, h) in updates {
            if !read_set.iter().any(|(r, _)| r == n) {
                return Err(Error::TargetNotInReadSet(n.clone()));
            }
            if let Some(h) = h
                && !exists(h)?
            {
                return Err(Error::MissingObject(*h));
            }
        }
        self.commit(|s| match s.decide_update_many(updates, read_set)? {
            Ok((m, versions)) => Ok((Some(m), ReplaceManyOutcome::Ok { versions })),
            Err(c) => Ok((None, c)),
        })
    }

    /// OCC delete (see [`RefState::decide_delete`]). `Ok { version }` carries
    /// the deleted version (0 if the name was already absent).
    pub fn delete(&self, name: &Name, read_set: &[(Name, Observed)]) -> Result<ReplaceOutcome> {
        self.commit(|s| match s.decide_delete(name, read_set)? {
            Ok(Some(m)) => {
                let version = match &m {
                    Mutation::Delete(_, v) => *v,
                    _ => 0,
                };
                Ok((Some(m), ReplaceOutcome::Ok { version }))
            }
            Ok(None) => Ok((None, ReplaceOutcome::Ok { version: 0 })),
            Err(c) => Ok((None, c)),
        })
    }

    /// Sets a namespace rule (see [`RefState::decide_set_rule`]).
    pub fn set_rule(&self, prefix: &str, rule: Rule) -> Result<()> {
        self.commit(|s| Ok((s.decide_set_rule(prefix, rule)?, ())))
    }

    /// Refs under `prefix`.
    pub fn list(&self, prefix: &str) -> Result<Vec<(Name, RefValue)>> {
        self.read(|s| s.list(prefix))
    }

    /// Configured rules.
    pub fn rules(&self) -> Result<Vec<(String, Rule)>> {
        self.read(|s| s.rules.iter().map(|(p, r)| (p.clone(), *r)).collect())
    }

    /// A snapshot of the whole state (after catching up).
    pub fn snapshot(&self) -> Result<RefState> {
        self.read(|s| s.clone())
    }
}

pub mod log;
