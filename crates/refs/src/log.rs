//! A ref store as a **create-only log on any [`Backend`]** (for example S3), so
//! several hosts can write one repository's names.
//!
//! ```text
//! <prefix>log/<seq:020>          one journal record per commit (create-only)
//! <prefix>snap/<seq:020>-<ts>    full state after entries < seq (O5SREFS1); ts = unix seconds
//! ```
//! **Commit point:** after catching up to the end of the log at `seq`, a writer
//! decides against that state and calls `put_if_absent(log/<seq>)`. The backend
//! guarantees a single winner per key (S3 `If-None-Match: *`). A loser catches
//! up and re-decides, so commits are linearized exactly as with the local
//! journal's flock, and the same pure decision logic (`RefState::decide_*`) applies.
//!
//! **Compaction:** every `threshold` commits, a snapshot is written. Log
//! entries (and older snapshots) are deleted only behind a snapshot that is at
//! least [`TRIM_GRACE`] old. A writer that pauses so long (between catching
//! up and its create) that its key could have been trimmed and re-created
//! detects this with a post-commit check, and fails the commit instead of
//! acknowledging a write that nobody would read.

use crate::{Error, Mutation, RefState, Result, decode_snapshot, encode_checked, encode_snapshot, parse_records};
use o5s_core::backend::{Backend, PutOutcome, PutSource};
use o5s_core::fault::FaultHandle;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Log entry framing: `"O5SLOGE1" | nonce[16] | journal record`. The nonce is
/// unique per commit *attempt*, so two writers that make the same decision
/// (same name, same observed version, same target, hence byte-identical
/// records) still produce different entries, and at most one of them can
/// recognise a stored entry as its own.
const ENTRY_MAGIC: &[u8; 8] = b"O5SLOGE1";

fn entry_record(b: &[u8]) -> &[u8] {
    if b.len() >= 24 && &b[..8] == ENTRY_MAGIC { &b[24..] } else { b }
}

fn attempt_nonce() -> [u8; 16] {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    h.update(&format!("{:?}", SystemTime::now()).into_bytes());
    h.update(&format!("{:?}", std::thread::current().id()).into_bytes());
    let mut n = [0u8; 16];
    n.copy_from_slice(&h.finalize().as_bytes()[..16]);
    n
}

/// Log entries are trimmed only behind snapshots at least this old.
pub const TRIM_GRACE: Duration = Duration::from_secs(3600);
/// Commits whose catch-up → create gap exceeds this re-check for trimming.
const SLOW_COMMIT: Duration = Duration::from_secs(60);
/// Default commits between snapshots.
pub const DEFAULT_SNAPSHOT_EVERY: u64 = 10_000;

struct LogState {
    state: RefState,
    /// Next log sequence number to read or write.
    next: u64,
    /// Sequence of the snapshot the state was last (re)based on.
    snap: u64,
    /// When the snapshot list was last checked for a trimmed gap.
    last_list: Option<Instant>,
    /// Key of the snapshot the state is based on (probed to detect trimming).
    snap_key: Option<String>,
}

/// A handle checks for trimmed gaps at most this often. Trimming only happens
/// behind snapshots older than TRIM_GRACE, so a handle that has caught up within
/// this interval can't be behind a trimmed region.
const LIST_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) struct LogEngine {
    feed: crate::Feed,
    backend: Arc<dyn Backend>,
    prefix: String,
    faults: FaultHandle,
    threshold: AtomicU64,
    /// Trim grace in seconds (TRIM_GRACE; tests shorten it).
    grace_secs: AtomicU64,
    inner: Mutex<LogState>,
}

fn poisoned<T>(_: T) -> Error {
    Error::Corrupt("internal lock poisoned".into())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl LogEngine {
    pub(crate) fn open(backend: Arc<dyn Backend>, prefix: &str, faults: FaultHandle) -> Result<Self> {
        if !backend.atomic_create() {
            return Err(Error::Unsupported(
                "refs on this backend need atomic conditional create (S3: enable conditional writes)",
            ));
        }
        let e = LogEngine {
            feed: crate::Feed::default(),
            backend,
            prefix: prefix.to_string(),
            faults,
            threshold: AtomicU64::new(DEFAULT_SNAPSHOT_EVERY),
            grace_secs: AtomicU64::new(TRIM_GRACE.as_secs()),
            inner: Mutex::new(LogState {
                state: RefState::default(),
                next: 0,
                snap: 0,
                last_list: None,
                snap_key: None,
            }),
        };
        {
            let mut g = e.inner.lock().map_err(poisoned)?;
            e.rebase_on_latest_snapshot(&mut g, 0)?;
            e.catch_up(&mut g)?;
        }
        Ok(e)
    }

    fn log_key(&self, seq: u64) -> String {
        format!("{}log/{seq:020}", self.prefix)
    }

    /// `(seq, ts, key)` of every snapshot, sorted by seq.
    fn snapshots(&self) -> Result<Vec<(u64, u64, String)>> {
        let mut v = Vec::new();
        for m in self.backend.list(&format!("{}snap/", self.prefix))? {
            let Some(stem) = m.key.rsplit('/').next() else { continue };
            let Some((seq, ts)) = stem.split_once('-') else { continue };
            if let (Ok(seq), Ok(ts)) = (seq.parse(), ts.parse()) {
                v.push((seq, ts, m.key.clone()));
            }
        }
        v.sort();
        Ok(v)
    }

    /// Loads the newest snapshot with seq > `min` (if any) into `g`.
    fn rebase_on_latest_snapshot(&self, g: &mut LogState, min: u64) -> Result<bool> {
        for _ in 0..8 {
            let snaps = self.snapshots()?;
            let Some((seq, _, key)) = snaps.into_iter().rev().find(|s| s.0 > min || (min == 0 && s.0 == 0)) else {
                return Ok(false);
            };
            if seq <= g.next && min != 0 {
                return Ok(false);
            }
            let mut b = Vec::new();
            match self.backend.open(&key) {
                Ok(mut r) => {
                    r.read_to_end(&mut b)?;
                }
                Err(Error::KeyNotFound(_)) => continue, // trimmed concurrently: a newer one exists; list again
                Err(e) => return Err(e),
            }
            g.state = decode_snapshot(&b)?;
            g.next = seq;
            g.snap = seq;
            g.snap_key = Some(key);
            self.feed.emit(&crate::Foreign::Reset);
            return Ok(true);
        }
        Err(Error::Backend { transient: true, msg: "ref log: snapshots keep disappearing; retry".into() })
    }

    /// True if the entry (or snapshot) our position was derived from has been
    /// trimmed, which means `g.next` may sit inside a trimmed region.
    fn base_trimmed(&self, g: &LogState) -> Result<bool> {
        if g.next > g.snap && g.next > 0 {
            return Ok(self.backend.head(&self.log_key(g.next - 1))?.is_none());
        }
        match &g.snap_key {
            Some(k) => Ok(self.backend.head(k)?.is_none()),
            None => Ok(false), // empty log, never snapshotted: nothing can be trimmed
        }
    }

    /// Applies log entries from `next` until the first missing one. If entries
    /// were trimmed below a newer snapshot, rebases on that snapshot.
    fn catch_up(&self, g: &mut LogState) -> Result<()> {
        loop {
            let mut b = Vec::new();
            match self.backend.open(&self.log_key(g.next)) {
                Ok(mut r) => {
                    r.read_to_end(&mut b)?;
                }
                Err(Error::KeyNotFound(_)) => {
                    // The end of the log, or (rarely) a trimmed gap behind a newer snapshot.
                    // One HEAD tells them apart: if what we read last is gone, we were trimmed.
                    let trimmed = self.base_trimmed(g)?;
                    if trimmed || g.last_list.is_none_or(|t| t.elapsed() > LIST_INTERVAL) {
                        g.last_list = Some(Instant::now());
                        if self.rebase_on_latest_snapshot(g, g.next)? {
                            continue;
                        }
                        if trimmed {
                            return Err(Error::Corrupt(
                                "ref log trimmed past this handle with no newer snapshot".into(),
                            ));
                        }
                    }
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
            let rec = entry_record(&b);
            let (muts, used, err) = parse_records(rec);
            if let Some(e) = err {
                return Err(e);
            }
            if muts.len() != 1 || used != rec.len() {
                return Err(Error::Corrupt(format!("bad ref log entry {}", g.next)));
            }
            self.feed.applied(&muts);
            for m in muts {
                g.state.apply(m);
            }
            g.next += 1;
        }
    }

    fn maybe_snapshot(&self, g: &mut LogState) -> Result<()> {
        if g.next - g.snap < self.threshold.load(Ordering::Relaxed) {
            return Ok(());
        }
        let key = format!("{}snap/{:020}-{}", self.prefix, g.next, now_secs());
        self.faults.hit("refs.log.snapshot")?;
        self.backend.put_if_absent(&key, PutSource::Bytes(&encode_snapshot(&g.state)))?;
        g.snap = g.next;
        g.snap_key = Some(key.clone());
        // Trim: behind the newest snapshot that is older than the grace period,
        // delete older snapshots and log entries below it.
        let snaps = self.snapshots()?;
        let cutoff = now_secs().saturating_sub(self.grace_secs.load(Ordering::Relaxed));
        // Keep at least the two newest snapshots whatever their age (margin against clock skew).
        let candidates = &snaps[..snaps.len().saturating_sub(2)];
        if let Some(&(keep, _, _)) = candidates.iter().rev().find(|s| s.1 <= cutoff) {
            for (seq, _, k) in &snaps {
                if *seq < keep {
                    let _ = self.backend.delete(k);
                }
            }
            for m in self.backend.list(&format!("{}log/", self.prefix))? {
                let seq = m.key.rsplit('/').next().and_then(|s| s.parse::<u64>().ok());
                if seq.is_some_and(|s| s < keep) {
                    let _ = self.backend.delete(&m.key);
                }
            }
        }
        Ok(())
    }
}

impl crate::Engine for LogEngine {
    fn feed(&self) -> &crate::Feed {
        &self.feed
    }

    fn read(&self, f: &mut dyn FnMut(&RefState)) -> Result<()> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        self.catch_up(&mut g)?;
        f(&g.state);
        Ok(())
    }

    fn commit(&self, decide: &mut dyn FnMut(&RefState) -> Result<Option<Mutation>>) -> Result<()> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        for _ in 0..10_000 {
            self.catch_up(&mut g)?;
            let caught_up = Instant::now();
            let Some(m) = decide(&g.state)? else { return Ok(()) };
            // One nonce per attempt: the backend may retry this exact body internally
            // (and a lost response then shows up as AlreadyExists with our own bytes).
            let mut rec = ENTRY_MAGIC.to_vec();
            rec.extend_from_slice(&attempt_nonce());
            rec.extend_from_slice(&encode_checked(&m)?);
            self.faults.hit("refs.log.put")?;
            match self.backend.put_if_absent(&self.log_key(g.next), PutSource::Bytes(&rec))? {
                PutOutcome::Created => {
                    // Did our slot fall into a trimmed region (a long pause, or clock skew)?
                    // If what we based this commit on is gone and a snapshot past our slot
                    // exists, nobody will read our entry: fail instead of acknowledging.
                    if self.base_trimmed(&g)? && self.snapshots()?.iter().any(|s| s.0 > g.next) {
                        let _ = self.backend.delete(&self.log_key(g.next));
                        return Err(Error::Backend {
                            transient: true,
                            msg: "ref commit landed in a compacted region; retry".into(),
                        });
                    }
                    if caught_up.elapsed() > SLOW_COMMIT {
                        // Could our key have been trimmed and re-created? If a snapshot past
                        // it exists, nobody will ever read our entry: fail, don't acknowledge.
                        if self.snapshots()?.iter().any(|s| s.0 > g.next) {
                            return Err(Error::Backend {
                                transient: true,
                                msg: "ref commit raced with log compaction after a long pause; retry".into(),
                            });
                        }
                    }
                    g.state.apply(m);
                    g.next += 1;
                    // The commit is durable. A failed snapshot is retried later.
                    let _ = self.maybe_snapshot(&mut g);
                    return Ok(());
                }
                PutOutcome::AlreadyExists => {
                    // Either another writer won this slot, or *our* attempt landed and only
                    // its response was lost (the backend's retry then sees 412). The entry
                    // carries this attempt's nonce: only a byte-identical entry (same nonce)
                    // is ours. Another writer making the *same* decision has a different
                    // nonce, so we conflict and re-decide. Two CAS operations on one
                    // observed version can never both succeed.
                    let mut stored = Vec::new();
                    match self.backend.open(&self.log_key(g.next)) {
                        Ok(mut r) => {
                            r.read_to_end(&mut stored)?;
                        }
                        Err(Error::KeyNotFound(_)) => continue, // trimmed meanwhile: re-check from scratch
                        Err(e) => return Err(e),
                    }
                    if stored == rec {
                        g.state.apply(m);
                        g.next += 1;
                        let _ = self.maybe_snapshot(&mut g);
                        return Ok(());
                    }
                    continue;
                }
            }
        }
        Err(Error::Backend { transient: true, msg: "ref log: too much contention".into() })
    }

    fn compact(&self) -> Result<()> {
        let mut g = self.inner.lock().map_err(poisoned)?;
        self.catch_up(&mut g)?;
        let t = self.threshold.swap(0, Ordering::Relaxed);
        let r = self.maybe_snapshot(&mut g);
        self.threshold.store(t, Ordering::Relaxed);
        r
    }

    fn generation(&self) -> Result<u64> {
        Ok(self.inner.lock().map_err(poisoned)?.snap)
    }

    fn set_compact_threshold(&self, n: u64) {
        self.threshold.store(n.max(1), Ordering::Relaxed);
    }

    fn set_trim_grace(&self, d: Duration) {
        self.grace_secs.store(d.as_secs(), Ordering::Relaxed);
    }
}
