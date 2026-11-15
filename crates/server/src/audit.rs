//! Append-only, hash-chained, fsync'd audit log (JSON lines).
//!
//! Every record carries `seq` (1, 2, …) and `prev`, the hex BLAKE3 of the
//! previous line's bytes (all zeros for the first record), so truncation,
//! deletion or editing of earlier records is detectable with [`verify`].
//!
//! A dedicated writer thread does group commit: it drains every queued record,
//! writes them, calls `fdatasync` once, and then acknowledges each waiter.
//! Mutating API calls await this acknowledgement before replying, so an acked
//! operation is never missing from the audit log.
//!
//! Crash recovery: a torn final line (no trailing `\n`) is truncated at open.
//! Complete lines are never rewritten.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::SystemTime;
use tokio::sync::oneshot;
use tracing::{error, warn};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub seq: u64,
    /// RFC 3339 UTC with milliseconds.
    pub ts: String,
    pub prev: String,
    pub request_id: String,
    pub principal: String,
    /// e.g. `put`, `get`, `replace`, `set_rule`, `auth`.
    pub action: String,
    /// Name, hash or prefix acted on.
    pub target: String,
    /// `ok`, `conflict`, `denied`, `error`, …
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// Fields supplied by callers; `seq`, `ts` and `prev` are filled by the writer.
#[derive(Clone, Debug, Default)]
pub struct Event {
    pub request_id: String,
    pub principal: String,
    pub action: &'static str,
    pub target: String,
    pub outcome: String,
    pub detail: Option<String>,
    pub remote: Option<String>,
}

enum Cmd {
    Write(Event, Option<oneshot::Sender<std::io::Result<()>>>),
    Stop(oneshot::Sender<()>),
}

/// Handle to the audit writer. Cheap to clone.
#[derive(Clone)]
pub struct AuditLog {
    tx: mpsc::Sender<Cmd>,
    path: PathBuf,
    healthy: Arc<AtomicBool>,
}

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Bound on a single record's `detail`, so records stay small.
const MAX_DETAIL: usize = 2048;

impl AuditLog {
    /// Opens (or creates) the log, recovers its tail and starts the writer thread.
    pub fn open(path: &Path) -> std::io::Result<AuditLog> {
        Self::open_with(path, None)
    }

    /// Like [`AuditLog::open`], rotating the file once it reaches `max_bytes`:
    /// the full file is renamed to `<name>.<last seq, 12 digits>` (fsync'd) and a
    /// new file continues the same hash chain.
    pub fn open_with(path: &Path, max_bytes: Option<u64>) -> std::io::Result<AuditLog> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let existed = path.exists();
        let mut f = OpenOptions::new().read(true).append(true).create(true).open(path)?;
        if !existed {
            f.sync_all()?;
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                File::open(dir)?.sync_all()?;
            }
        }
        let (mut seq, mut prev) = recover_tail(&mut f, path)?;
        if seq == 0
            && let Some((_, last)) = rotated_files(path)?.pop()
        {
            // The current file is new or empty: continue the chain of the newest rotated file.
            let mut r = OpenOptions::new().read(true).write(true).open(&last)?;
            (seq, prev) = recover_tail(&mut r, &last)?;
        }
        let (tx, rx) = mpsc::channel();
        let healthy = Arc::new(AtomicBool::new(true));
        let h = healthy.clone();
        let rot = max_bytes.map(|m| (path.to_path_buf(), m.max(4096)));
        std::thread::Builder::new().name("p2p-audit".into()).spawn(move || writer(f, rx, seq, prev, h, rot))?;
        Ok(AuditLog { tx, path: path.to_path_buf(), healthy })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// False once a write has failed (the writer then rejects everything).
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Appends `ev` and waits until it is durable.
    pub async fn record(&self, ev: Event) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::Write(ev, Some(tx))).map_err(|_| std::io::Error::other("audit writer stopped"))?;
        rx.await.map_err(|_| std::io::Error::other("audit writer stopped"))?
    }

    /// Appends `ev` without waiting (reads, denials).
    pub fn record_nowait(&self, ev: Event) {
        if self.tx.send(Cmd::Write(ev, None)).is_err() {
            error!("audit writer stopped; dropping audit record");
        }
    }

    /// Flushes everything queued so far and stops the writer.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Stop(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

/// Rotated files of the log at `path`, oldest first: `(last seq, path)`.
pub fn rotated_files(path: &Path) -> std::io::Result<Vec<(u64, PathBuf)>> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Ok(Vec::new()) };
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for e in rd {
        let e = e?;
        let fname = e.file_name();
        let Some(f) = fname.to_str() else { continue };
        if let Some(suffix) = f.strip_prefix(name).and_then(|r| r.strip_prefix('.'))
            && suffix.len() == 12
            && let Ok(n) = suffix.parse::<u64>()
        {
            out.push((n, e.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// Renames the full log away and opens a fresh one (both fsync'd).
fn rotate(f: &mut File, path: &Path, seq: u64) -> std::io::Result<()> {
    f.sync_all()?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| std::io::Error::other("bad audit path"))?;
    let target = path.with_file_name(format!("{name}.{seq:012}"));
    if target.exists() {
        return Err(std::io::Error::other(format!("{} already exists", target.display())));
    }
    std::fs::rename(path, &target)?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    *f = OpenOptions::new().read(true).append(true).create_new(true).open(path)?;
    f.sync_all()?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Finds the last complete record. Truncates a torn trailing line.
fn recover_tail(f: &mut File, path: &Path) -> std::io::Result<(u64, String)> {
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok((0, ZERO.to_string()));
    }
    // Records are < 8 KiB, so the last 1 MiB holds the last complete line.
    let window = len.min(1 << 20);
    f.seek(SeekFrom::Start(len - window))?;
    let mut tail = Vec::with_capacity(window as usize);
    f.read_to_end(&mut tail)?;
    let Some(last_nl) = tail.iter().rposition(|&b| b == b'\n') else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: no complete record in the last {window} bytes; refusing to touch it", path.display()),
        ));
    };
    if last_nl + 1 != tail.len() {
        let keep = len - window + last_nl as u64 + 1;
        warn!(path = %path.display(), dropped = len - keep, "audit log: truncating torn final record");
        f.set_len(keep)?;
        f.sync_all()?;
    }
    let body = &tail[..last_nl];
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let line = &body[start..];
    let rec: AuditRecord = serde_json::from_slice(line).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: bad last record: {e}", path.display()))
    })?;
    Ok((rec.seq, blake3::hash(line).to_hex().to_string()))
}

fn writer(
    mut f: File,
    rx: mpsc::Receiver<Cmd>,
    mut seq: u64,
    mut prev: String,
    healthy: Arc<AtomicBool>,
    rotation: Option<(PathBuf, u64)>,
) {
    let mut failed: Option<String> = None;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(c) = rx.try_recv() {
            batch.push(c);
            if batch.len() >= 4096 {
                break;
            }
        }
        let mut buf = Vec::new();
        let mut waiters = Vec::new();
        let mut stop = None;
        for c in batch {
            match c {
                Cmd::Write(ev, w) => {
                    seq += 1;
                    let mut detail = ev.detail;
                    if let Some(d) = &mut detail
                        && d.len() > MAX_DETAIL
                    {
                        let mut cut = MAX_DETAIL;
                        while !d.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        d.truncate(cut);
                    }
                    let rec = AuditRecord {
                        seq,
                        ts: rfc3339_now(),
                        prev: prev.clone(),
                        request_id: ev.request_id,
                        principal: ev.principal,
                        action: ev.action.to_string(),
                        target: ev.target,
                        outcome: ev.outcome,
                        detail,
                        remote: ev.remote,
                    };
                    // Serializing plain strings/ints cannot fail.
                    let line = serde_json::to_vec(&rec).unwrap_or_default();
                    prev = blake3::hash(&line).to_hex().to_string();
                    buf.extend_from_slice(&line);
                    buf.push(b'\n');
                    if let Some(w) = w {
                        waiters.push(w);
                    }
                }
                Cmd::Stop(s) => stop = Some(s),
            }
        }
        let res = match &failed {
            Some(e) => Err(std::io::Error::other(e.clone())),
            None => f.write_all(&buf).and_then(|_| f.sync_data()).and_then(|_| match &rotation {
                Some((path, max)) if f.metadata()?.len() >= *max => rotate(&mut f, path, seq),
                _ => Ok(()),
            }),
        };
        if let Err(e) = &res
            && failed.is_none()
        {
            // After a failed write the file position/contents are uncertain, so
            // stop accepting records instead of producing a broken chain.
            error!(error = %e, "audit log write failed; all further audited operations will fail");
            failed = Some(e.to_string());
            healthy.store(false, Ordering::Release);
        }
        for w in waiters {
            let _ = w.send(match &res {
                Ok(()) => Ok(()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            });
        }
        if let Some(s) = stop {
            let _ = s.send(());
            return;
        }
    }
}

/// Verifies the hash chain of the log at `path`, including its rotated files
/// (oldest first). Returns the number of records checked. The chain may start
/// at `seq > 1` when older rotated files were archived away; everything from
/// there on must link up exactly.
pub fn verify(path: &Path) -> Result<u64, String> {
    Ok(verify_all(path)?.0)
}

/// Like [`verify`], also returning the first sequence number seen.
pub fn verify_all(path: &Path) -> Result<(u64, u64), String> {
    let mut files: Vec<PathBuf> = rotated_files(path).map_err(|e| e.to_string())?.into_iter().map(|x| x.1).collect();
    if path.exists() {
        files.push(path.to_path_buf());
    }
    let mut prev: Option<String> = None;
    let mut next_seq: Option<u64> = None;
    let mut first_seq = 0;
    let mut n = 0u64;
    for file in &files {
        let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        for (i, line) in data.split(|&b| b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let at = || format!("{}: line {}", file.display(), i + 1);
            let rec: AuditRecord = serde_json::from_slice(line).map_err(|e| format!("{}: {e}", at()))?;
            match (&prev, next_seq) {
                (Some(p), Some(s)) => {
                    if rec.prev != *p {
                        return Err(format!("{}: chain broken (prev mismatch)", at()));
                    }
                    if rec.seq != s {
                        return Err(format!("{}: seq {} expected {s}", at(), rec.seq));
                    }
                }
                _ => {
                    // First record overall.
                    if rec.seq == 1 && rec.prev != ZERO {
                        return Err(format!("{}: first record must chain from zero", at()));
                    }
                    first_seq = rec.seq;
                }
            }
            prev = Some(blake3::hash(line).to_hex().to_string());
            next_seq = Some(rec.seq + 1);
            n += 1;
        }
    }
    Ok((n, first_seq))
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn rfc3339_now() -> String {
    rfc3339(SystemTime::now())
}

pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(i: usize) -> Event {
        Event {
            request_id: format!("r{i}"),
            principal: "p".into(),
            action: "put",
            target: "t".into(),
            outcome: "ok".into(),
            ..Default::default()
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn chain_and_reopen() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/audit.jsonl");
        rt().block_on(async {
            let a = AuditLog::open(&p).unwrap();
            for i in 0..10 {
                a.record(ev(i)).await.unwrap();
            }
            a.shutdown().await;
        });
        assert_eq!(verify(&p).unwrap(), 10);
        rt().block_on(async {
            let a = AuditLog::open(&p).unwrap();
            a.record(ev(10)).await.unwrap();
            a.shutdown().await;
        });
        assert_eq!(verify(&p).unwrap(), 11);
    }

    #[test]
    fn concurrent_group_commit() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).build().unwrap().block_on(async {
            let a = AuditLog::open(&p).unwrap();
            let hs: Vec<_> = (0..200)
                .map(|i| {
                    let a = a.clone();
                    tokio::spawn(async move { a.record(ev(i)).await.unwrap() })
                })
                .collect();
            for h in hs {
                h.await.unwrap();
            }
            a.shutdown().await;
        });
        assert_eq!(verify(&p).unwrap(), 200);
    }

    #[test]
    fn torn_tail_truncated_and_tamper_detected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        rt().block_on(async {
            let a = AuditLog::open(&p).unwrap();
            for i in 0..3 {
                a.record(ev(i)).await.unwrap();
            }
            a.shutdown().await;
        });
        // Simulate a crash mid-append.
        OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"seq\":4,\"ts\":\"20").unwrap();
        rt().block_on(async {
            let a = AuditLog::open(&p).unwrap();
            a.record(ev(3)).await.unwrap();
            a.shutdown().await;
        });
        assert_eq!(verify(&p).unwrap(), 4);

        // Edit an earlier record: the chain breaks at the next line.
        let s = std::fs::read_to_string(&p).unwrap().replacen("\"r1\"", "\"rX\"", 1);
        std::fs::write(&p, s).unwrap();
        assert!(verify(&p).unwrap_err().contains("line 3"));
    }

    #[test]
    fn rotation_keeps_the_chain() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        rt().block_on(async {
            let a = AuditLog::open_with(&p, Some(4096)).unwrap();
            for i in 0..100 {
                a.record(ev(i)).await.unwrap();
            }
            a.shutdown().await;
        });
        let rotated = rotated_files(&p).unwrap();
        assert!(rotated.len() >= 2, "{rotated:?}");
        assert_eq!(verify(&p).unwrap(), 100);
        // Reopen (possibly right after a rotation) and continue the chain.
        rt().block_on(async {
            let a = AuditLog::open_with(&p, Some(4096)).unwrap();
            a.record(ev(100)).await.unwrap();
            a.shutdown().await;
        });
        assert_eq!(verify_all(&p).unwrap(), (101, 1));
        // Archiving the oldest file is allowed; the chain then starts later.
        std::fs::remove_file(&rotated[0].1).unwrap();
        let (n, first) = verify_all(&p).unwrap();
        assert!(first > 1 && n < 101);
        // Removing a file between two remaining ones breaks the chain.
        assert!(rotated.len() >= 3, "{rotated:?}");
        std::fs::remove_file(&rotated[2].1).unwrap();
        assert!(verify(&p).unwrap_err().contains("chain broken"));
    }

    #[test]
    fn timestamps() {
        use std::time::Duration;
        assert_eq!(
            rfc3339(SystemTime::UNIX_EPOCH + Duration::from_millis(1_369_353_600_123)),
            "2013-05-24T00:00:00.123Z"
        );
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00.000Z");
    }
}
