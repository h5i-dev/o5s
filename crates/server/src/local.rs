//! The local single-process store: an append-only log of committed kernel
//! write sets. Each command is decided by the verified kernel, appended and
//! synced, and only then applied; a failed append changes nothing. Startup
//! replays the log through the kernel's `commit`. A torn final record from a
//! crash is truncated; corruption anywhere else refuses to start.
use crate::backend::Failure;
use fs2::FileExt;
use o5s_platform::{Actor, Command, Database, Effect, Refusal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const LOG: &str = "registry.log";

/// A reply remembered under a client's idempotency key.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Remembered {
    tenant: u64,
    user: u64,
    key: String,
    fingerprint: String,
    reply: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct Record {
    effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idempotency: Option<Remembered>,
}

pub struct Store {
    pub db: Database,
    path: PathBuf,
    log: File,
    /// Bytes of complete records; a failed append is truncated back to it.
    length: u64,
    /// An append failed and could not be undone: refuse writes until restart.
    poisoned: bool,
    fail_next: bool,
    idempotency: HashMap<(u64, u64, String), Remembered>,
    // Held for the process lifetime: no second writer can open the same store.
    _lock: File,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(command: &Command) -> String {
    hex(&Sha256::digest(
        serde_json::to_vec(command).unwrap_or_default(),
    ))
}

/// Visible ASCII, 1 to 128 bytes.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_graphic())
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("store.lock"))?;
        lock.try_lock_exclusive()?;
        let path = root.join(LOG);
        let created = !path.exists();
        if created && root.join("state.json").exists() {
            return Err(format!(
                "{} holds a pre-kernel state.json; import it with `o5s-server import-json {}` first",
                root.display(),
                root.display()
            )
            .into());
        }
        let mut log = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        if created {
            log.sync_all()?;
            File::open(root)?.sync_all()?;
        }
        let mut store = Self {
            db: Database::default(),
            path,
            log: log.try_clone()?,
            length: 0,
            poisoned: false,
            fail_next: false,
            idempotency: HashMap::new(),
            _lock: lock,
        };
        store.replay(&mut log)?;
        Ok(store)
    }

    fn replay(&mut self, log: &mut File) -> Result<(), Box<dyn std::error::Error>> {
        log.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(&*log);
        let mut offset = 0u64;
        let mut line = Vec::new();
        let mut number = 0usize;
        let mut torn = None;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            number += 1;
            if torn.is_some() {
                return Err(format!(
                    "{}: corrupt record before line {number}",
                    self.path.display()
                )
                .into());
            }
            match decode(&line) {
                Some(record) => {
                    self.db
                        .commit(&record.effects)
                        .map_err(|e| format!("{}: line {number}: {e}", self.path.display()))?;
                    if let Some(r) = record.idempotency {
                        self.idempotency
                            .insert((r.tenant, r.user, r.key.clone()), r);
                    }
                    offset += read as u64;
                }
                None => torn = Some(offset),
            }
        }
        if let Some(end) = torn {
            // Only the last record may be incomplete: a crash during append.
            self.log.set_len(end)?;
            self.log.sync_all()?;
            eprintln!(
                "{}: truncated an incomplete final record",
                self.path.display()
            );
        }
        self.length = torn.unwrap_or(offset);
        Ok(())
    }

    /// Decide and commit; refusals and storage failures change nothing.
    pub fn execute(
        &mut self,
        actor: &Actor,
        command: Command,
    ) -> Result<serde_json::Value, Refusal> {
        self.execute_keyed(actor, command, None)
            .map_err(|e| match e {
                Failure::Refused(r) => r,
                Failure::Conflict(m) | Failure::Unavailable(m) => Refusal(m),
            })
    }

    /// With a key, a repeated command returns the first reply instead of
    /// running again; the key and reply commit with the command's writes.
    pub fn execute_keyed(
        &mut self,
        actor: &Actor,
        command: Command,
        key: Option<&str>,
    ) -> Result<serde_json::Value, Failure> {
        if self.poisoned {
            return Err(Failure::Unavailable(
                "storage unavailable: an earlier write could not be undone; restart the server"
                    .into(),
            ));
        }
        let remembered = match key {
            Some(key) => {
                if !valid_key(key) {
                    return Err(Failure::Refused(Refusal("invalid idempotency key".into())));
                }
                let print = fingerprint(&command);
                if let Some(r) = self
                    .idempotency
                    .get(&(actor.tenant, actor.user, key.to_string()))
                {
                    if r.fingerprint != print {
                        return Err(Failure::Conflict(
                            "idempotency key reused for a different command".into(),
                        ));
                    }
                    return Ok(r.reply.clone());
                }
                Some(print)
            }
            None => None,
        };
        let decision = self.db.decide(actor, command).map_err(Failure::Refused)?;
        if decision.is_noop() && key.is_none() {
            return Ok(decision.reply);
        }
        let record = Record {
            effects: decision.effects,
            idempotency: remembered.map(|fingerprint| Remembered {
                tenant: actor.tenant,
                user: actor.user,
                key: key.unwrap_or_default().to_string(),
                fingerprint,
                reply: decision.reply.clone(),
            }),
        };
        self.append(&record)
            .map_err(|e| Failure::Unavailable(format!("storage unavailable: {e}")))?;
        self.db
            .commit(&record.effects)
            .map_err(|e| Failure::Unavailable(format!("committed but not applied: {e}")))?;
        if let Some(r) = record.idempotency {
            self.idempotency
                .insert((r.tenant, r.user, r.key.clone()), r);
        }
        Ok(decision.reply)
    }

    fn append(&mut self, record: &Record) -> std::io::Result<()> {
        let line = encode(record)?;
        let written = if std::mem::take(&mut self.fail_next) {
            // Simulate a failure after a partial write.
            self.log
                .write_all(&line[..line.len() / 2])
                .and_then(|_| Err(std::io::Error::other("injected storage failure")))
        } else {
            self.log.write_all(&line).and_then(|_| self.log.sync_data())
        };
        match written {
            Ok(()) => {
                self.length += line.len() as u64;
                Ok(())
            }
            Err(error) => {
                if self
                    .log
                    .set_len(self.length)
                    .and_then(|_| self.log.sync_data())
                    .is_err()
                {
                    self.poisoned = true;
                }
                Err(error)
            }
        }
    }

    /// Make the next append fail after writing part of its record.
    #[doc(hidden)]
    pub fn inject_append_failure(&mut self) {
        self.fail_next = true;
    }

    pub fn log_length(&self) -> u64 {
        self.length
    }

    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn log_path(&self) -> &Path {
        &self.path
    }
}

/// What a log holds, from a read-only replay.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub records: usize,
    /// Bytes of complete, verified records.
    pub bytes: u64,
    /// Bytes after them: an incomplete final record.
    pub torn: u64,
    pub tenants: usize,
    pub repositories: usize,
    pub patches: usize,
}

/// Replay a log without locking it or changing it. Corruption before the last
/// record is an error.
pub fn verify(path: &Path) -> Result<Summary, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut db = Database::default();
    let mut offset = 0usize;
    let mut records = 0usize;
    while offset < bytes.len() {
        let end = match bytes[offset..].iter().position(|b| *b == b'\n') {
            Some(i) => offset + i + 1,
            None => break,
        };
        match decode(&bytes[offset..end]) {
            Some(record) => db
                .commit(&record.effects)
                .map_err(|e| format!("record {}: {e}", records + 1))?,
            None if end == bytes.len() => break,
            None => {
                return Err(format!(
                    "{}: corrupt record {}",
                    path.display(),
                    records + 1
                ));
            }
        }
        records += 1;
        offset = end;
    }
    Ok(Summary {
        records,
        bytes: offset as u64,
        torn: (bytes.len() - offset) as u64,
        tenants: db.registry.len(),
        repositories: db.views.len(),
        patches: db.views.values().map(|v| v.patches.len()).sum(),
    })
}

/// Copy the complete records of a log, which may be in use, to a new file
/// `out`, sync it, and verify the copy.
pub fn backup(path: &Path, out: &Path) -> Result<Summary, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    file.write_all(&bytes[..complete])
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    let summary = verify(out)?;
    if summary.torn != 0 {
        return Err("backup ends in an incomplete record".into());
    }
    Ok(summary)
}

fn encode(record: &Record) -> std::io::Result<Vec<u8>> {
    let json = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let mut line = Vec::with_capacity(json.len() + 66);
    line.extend_from_slice(hex(&Sha256::digest(&json)).as_bytes());
    line.push(b' ');
    line.extend_from_slice(&json);
    line.push(b'\n');
    Ok(line)
}

/// Write a new log holding `effects`, one record each, and sync it.
pub fn write_log(path: &Path, effects: Vec<Effect>) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    for effect in effects {
        file.write_all(&encode(&Record {
            effects: vec![effect],
            idempotency: None,
        })?)?;
    }
    file.sync_all()
}

fn decode(line: &[u8]) -> Option<Record> {
    let line = line.strip_suffix(b"\n")?;
    let (digest, json) = (line.get(..64)?, line.get(65..)?);
    if line.get(64) != Some(&b' ') || hex(&Sha256::digest(json)).as_bytes() != digest {
        return None;
    }
    serde_json::from_slice(json).ok()
}
