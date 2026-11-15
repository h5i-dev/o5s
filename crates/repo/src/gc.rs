//! Garbage collection and repack that are safe against concurrent writers.
//!
//! # Protocol
//! A GC cycle has three phases, each separated by at least the grace period
//! `G`. Every invocation of [`LocalRepo::gc`] advances whatever is due, so it
//! can run from cron.
//!
//! * **Plan** (`gc/<ts>-<id>.plan`): records the live sealed packs that exist
//!   now. Only packs that are at least `G` old are ever repacked, so writers
//!   get a lease of `G` between `put` and the `replace` that references it.
//! * **Sweep** (`G` after the plan): marks the live set from the current refs
//!   (plus manifest chunks and a pluggable [`Tracer`]). For each planned pack
//!   that is mostly garbage or small, copies its live records verbatim into
//!   new packs, then writes `gc/<ts>-<id>.condemned` naming those packs.
//!   Condemned packs stay readable.
//! * **Delete** (`G` after the condemnation): marks again, *rescues* any live
//!   object still served only from a condemned pack (a writer may have
//!   referenced it since), then deletes each condemned pack's `.idx` followed
//!   by its `.pack`, and finally the GC records.
//!
//! Writers re-list at least every [`pico2peta_pack::store::DEDUP_REFRESH_INTERVAL`]
//! before trusting a dedup hit, and they ignore copies in condemned packs. A
//! writer whose `put` deduplicated against a pack being collected has
//! therefore either learned about the condemnation (and rewritten the object)
//! or referenced it early enough for the delete phase's mark to rescue it.
//! Readers whose pack disappears re-list and follow the rescued copy.
//!
//! **Assumption (operation lease):** a single `put` → `replace` sequence
//! finishes within `G`. [`GcOptions::grace`] defaults to 1 hour and must be at
//! least 10 minutes unless `allow_short_grace` is set (tests only).

use crate::LocalRepo;
use pico2peta_core::backend::PutSource;
use pico2peta_core::{Error, Hash, ObjectStore, Repository, Result};
use pico2peta_pack::{GC_PREFIX, read_name_list};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Finds the objects an object references, beyond manifest chunks (which
/// GC always follows). Future structured objects (directories, snapshots)
/// plug in here.
pub trait Tracer: Send + Sync {
    /// Hashes directly referenced by `hash`.
    fn children(&self, hash: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>>;
}

/// Treats every object as an opaque leaf. Only ref targets (and their
/// chunks) are kept alive.
pub struct OpaqueObjects;

impl Tracer for OpaqueObjects {
    fn children(&self, _: &Hash, _: &dyn ObjectStore) -> Result<Vec<Hash>> {
        Ok(Vec::new())
    }
}

/// GC tuning.
#[derive(Clone, Debug)]
pub struct GcOptions {
    /// Minimum time between phases. See the module docs for why.
    pub grace: Duration,
    /// Permit `grace` under 10 minutes. Only safe when no writer is active.
    pub allow_short_grace: bool,
    /// Repack a planned pack if at least this fraction of its bytes is garbage...
    pub min_garbage_ratio: f64,
    /// ...or if it is smaller than this many bytes (consolidates small packs).
    pub small_pack_bytes: u64,
    /// Only report what would happen.
    pub dry_run: bool,
}

impl Default for GcOptions {
    fn default() -> Self {
        GcOptions {
            grace: Duration::from_secs(3600),
            allow_short_grace: false,
            min_garbage_ratio: 0.2,
            small_pack_bytes: 16 << 20,
            dry_run: false,
        }
    }
}

/// What one invocation did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GcReport {
    /// A new plan was written (its key).
    pub planned: Option<String>,
    /// Plans swept in this run.
    pub swept: Vec<String>,
    /// Packs condemned in this run.
    pub condemned_packs: usize,
    /// Records copied into new packs (repack plus rescue).
    pub copied_records: usize,
    /// Of those, rescued in the delete phase.
    pub rescued_records: usize,
    /// Packs deleted in this run.
    pub deleted_packs: usize,
    /// Bytes deleted (pack plus index).
    pub deleted_bytes: u64,
    /// Size of the live set (objects plus chunks).
    pub live_hashes: usize,
    /// Refs whose target is missing (corruption). GC continues but reports them.
    pub dangling_refs: Vec<String>,
}

#[derive(Clone, Debug)]
struct GcRecord {
    key: String,
    id: String,
    ts: u64,
    packs: Vec<String>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn parse_key(key: &str, suffix: &str) -> Option<(u64, String)> {
    let stem = key.strip_prefix(GC_PREFIX)?.strip_suffix(suffix)?;
    let (ts, id) = stem.split_once('-')?;
    Some((ts.parse().ok()?, id.to_string()))
}

impl LocalRepo {
    fn gc_records(&self, suffix: &str) -> Result<Vec<GcRecord>> {
        let b = self.packs.backend();
        let mut out = Vec::new();
        for m in b.list(GC_PREFIX)? {
            let Some((ts, id)) = parse_key(&m.key, suffix) else { continue };
            match read_name_list(b.as_ref(), &m.key) {
                Ok(packs) => out.push(GcRecord { key: m.key, id, ts, packs }),
                Err(Error::KeyNotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        out.sort_by_key(|r| (r.ts, r.id.clone()));
        Ok(out)
    }

    fn write_record(&self, kind: &str, id: &str, packs: &[String]) -> Result<String> {
        let key = format!("{GC_PREFIX}{:012}-{id}.{kind}", now_secs());
        let mut body = packs.join("\n");
        body.push('\n');
        self.packs.backend().put_if_absent(&key, PutSource::Bytes(body.as_bytes()))?;
        Ok(key)
    }

    /// Live set: ref targets, manifest chunks, and tracer children.
    fn mark(&self, tracer: &dyn Tracer, report: &mut GcReport) -> Result<HashSet<Hash>> {
        let mut live = HashSet::new();
        let mut queue: VecDeque<Hash> = VecDeque::new();
        report.dangling_refs.clear();
        for (name, v) in self.list_refs("")? {
            if !self.packs.contains(&v.hash)? {
                report.dangling_refs.push(name.to_string());
                continue;
            }
            queue.push_back(v.hash);
        }
        while let Some(h) = queue.pop_front() {
            if !live.insert(h) {
                continue;
            }
            if let Some(chunks) = self.packs.chunks(&h)? {
                queue.extend(chunks);
            }
            for c in tracer.children(&h, self)? {
                if !self.packs.contains(&c)? {
                    return Err(Error::Corrupt(format!("{h} references missing object {c}")));
                }
                queue.push_back(c);
            }
        }
        report.live_hashes = live.len();
        Ok(live)
    }

    /// Runs whichever GC phases are due. See the module docs.
    pub fn gc(&self, opts: &GcOptions, tracer: &dyn Tracer) -> Result<GcReport> {
        if opts.grace < Duration::from_secs(600) && !opts.allow_short_grace {
            return Err(Error::Parse("GC grace period must be at least 10 minutes".into()));
        }
        let mut report = GcReport::default();
        let now = now_secs();
        let due = |ts: u64| ts.saturating_add(opts.grace.as_secs()) <= now;
        // Our own writes must be visible before we reason about packs.
        self.packs.flush()?;
        self.packs.refresh()?;

        let plans = self.gc_records("plan")?;
        let condemned = self.gc_records("condemned")?;
        let condemned_ids: HashSet<&str> = condemned.iter().map(|r| r.id.as_str()).collect();

        // --- Delete phase -------------------------------------------------
        for rec in condemned.iter().filter(|r| due(r.ts)) {
            let live = self.mark(tracer, &mut report)?;
            let doomed: HashSet<&String> = rec.packs.iter().collect();
            let mut rescued = 0;
            for h in &live {
                if self.packs.serving_pack(h)?.is_some_and(|p| doomed.contains(&p)) {
                    if !opts.dry_run {
                        self.packs.copy_record(h)?;
                    }
                    rescued += 1;
                }
            }
            if opts.dry_run {
                report.rescued_records += rescued;
                continue;
            }
            self.packs.flush()?;
            self.packs.refresh()?;
            for h in &live {
                if self.packs.serving_pack(h)?.is_some_and(|p| doomed.contains(&p)) {
                    return Err(Error::Corrupt(format!(
                        "live object {h} still only in a condemned pack; aborting delete"
                    )));
                }
            }
            report.rescued_records += rescued;
            report.copied_records += rescued;
            let b = self.packs.backend();
            for p in &rec.packs {
                report.deleted_bytes += self.packs.pack_bytes(p)?;
                // Index first, so no new reader picks the pack up.
                b.delete(&format!("packs/{p}.idx"))?;
                b.delete(&format!("packs/{p}.pack"))?;
                report.deleted_packs += 1;
            }
            for plan in plans.iter().filter(|p| p.id == rec.id) {
                b.delete(&plan.key)?;
            }
            b.delete(&rec.key)?;
            self.packs.refresh()?;
        }

        // --- Sweep phase --------------------------------------------------
        for plan in plans.iter().filter(|p| due(p.ts) && !condemned_ids.contains(p.id.as_str())) {
            let live = self.mark(tracer, &mut report)?;
            let current: HashSet<String> = self.packs.live_packs()?.into_iter().collect();
            let mut victims = Vec::new();
            for p in plan.packs.iter().filter(|p| current.contains(*p)) {
                let entries = self.packs.pack_entries(p)?;
                let total: u64 = entries.iter().map(|e| e.stored_len as u64).sum();
                let live_bytes: u64 =
                    entries.iter().filter(|e| live.contains(&e.hash)).map(|e| e.stored_len as u64).sum();
                let garbage = if total == 0 { 1.0 } else { 1.0 - live_bytes as f64 / total as f64 };
                let size = self.packs.pack_bytes(p)?;
                if garbage >= opts.min_garbage_ratio || size < opts.small_pack_bytes {
                    victims.push((p.clone(), entries));
                }
            }
            // Consolidating a single small, fully live pack is pointless churn.
            if victims.len() == 1 && victims[0].1.iter().all(|e| live.contains(&e.hash)) {
                victims.clear();
            }
            if !opts.dry_run {
                let mut copied: HashSet<Hash> = HashSet::new();
                for (_, entries) in &victims {
                    for e in entries {
                        if live.contains(&e.hash) && copied.insert(e.hash) {
                            self.packs.copy_record(&e.hash)?;
                        }
                    }
                }
                self.packs.flush()?;
                report.copied_records += copied.len();
                if !victims.is_empty() {
                    let names: Vec<String> = victims.iter().map(|v| v.0.clone()).collect();
                    self.write_record("condemned", &plan.id, &names)?;
                } else {
                    // Nothing to do. Retire the plan so a new one can be made.
                    self.packs.backend().delete(&plan.key)?;
                }
                self.packs.refresh()?;
            }
            report.condemned_packs += victims.len();
            report.swept.push(plan.key.clone());
        }

        // --- Plan phase ---------------------------------------------------
        let pending = self.gc_records("plan")?;
        if pending.is_empty() && !opts.dry_run {
            let packs = self.packs.live_packs()?;
            if !packs.is_empty() {
                let id = format!("{:016x}", rand_id());
                report.planned = Some(self.write_record("plan", &id, &packs)?);
            }
        }
        if report.live_hashes == 0 {
            self.mark(tracer, &mut report)?;
        }
        Ok(report)
    }
}

fn rand_id() -> u64 {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(t, std::process::id()), &mut h);
    std::hash::Hasher::finish(&h)
}

/// Summary of on-backend GC state (for `stat`).
pub fn gc_state(repo: &LocalRepo) -> Result<HashMap<&'static str, usize>> {
    let mut m = HashMap::new();
    m.insert("plans", repo.gc_records("plan")?.len());
    m.insert("condemned", repo.gc_records("condemned")?.len());
    Ok(m)
}
