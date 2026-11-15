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
//! before trusting a dedup hit, and they ignore copies in condemned packs.
//! Before committing a ref, `replace`/`replace_many` call
//! [`ObjectStore::ensure_live`] on the target: it rewrites the object (and its
//! chunks) into live storage if GC has condemned the only copy, and returns
//! false if the object is gone, so the replace then fails with `MissingObject`
//! instead of creating a dangling ref. Readers whose pack disappears re-list
//! and follow the rescued copy.
//!
//! **Remaining timing assumption:** the few milliseconds between
//! `ensure_live` and the journal commit, plus the dedup refresh interval
//! (60 s), are far shorter than the grace period. A slow writer (put now,
//! replace hours later) is therefore safe: at worst its `replace` fails and it
//! must put again. [`GcOptions::grace`] defaults to 1 hour and must be at least
//! 10 minutes unless `allow_short_grace` is set (tests only). Objects
//! referenced *indirectly* (children found by a [`Tracer`]) must be passed
//! through `ensure_live` by the adapter that links them, as the Git and OCI
//! adapters do.

use crate::LocalRepo;
use pico2peta_core::backend::PutSource;
use pico2peta_core::{Error, Hash, ObjectStore, Repository, Result};
use pico2peta_pack::format::Kind;
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

/// Where an object should be placed in a similarity-grouped repack: objects
/// with the same `group` are compressed together in frames, in `order`.
/// Example (Git): group = file extension, order = path + commit time.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey {
    /// Objects with the same group are compressed together.
    pub group: String,
    /// Position within the group (lexicographic).
    pub order: String,
}

/// Supplies grouping hints for repack (Experiment 1, V2 layout). It's called
/// once per sweep with the live objects being repacked and returns keys for
/// those it knows; unknown objects are copied verbatim. Adapters implement it
/// (e.g. the Git adapter by walking trees: paths are known only from trees).
pub trait Grouper: Send + Sync {
    /// Group keys for (some of) `live`. Objects without a key are copied verbatim.
    fn assign(&self, repo: &dyn Repository, live: &HashSet<Hash>) -> Result<HashMap<Hash, GroupKey>>;
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
    /// Repack every planned pack, even fully live ones (like
    /// `git gc --aggressive`). Use it with a [`Grouper`] to regroup a whole repository.
    pub repack_all: bool,
    /// zstd level for frames written by a grouped repack (Exp1: 19).
    pub frame_level: i32,
    /// Maximum uncompressed frame size for a grouped repack (Exp1: 4 MiB).
    pub frame_cap: usize,
}

impl Default for GcOptions {
    fn default() -> Self {
        GcOptions {
            grace: Duration::from_secs(3600),
            allow_short_grace: false,
            min_garbage_ratio: 0.2,
            small_pack_bytes: 16 << 20,
            dry_run: false,
            repack_all: false,
            frame_level: 19,
            frame_cap: 4 << 20,
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
        self.fault("gc.write-record")?;
        let key = format!("{GC_PREFIX}{:012}-{id}.{kind}", now_secs());
        let mut body = packs.join("\n");
        body.push('\n');
        self.packs.backend().put_if_absent(&key, PutSource::Bytes(body.as_bytes()))?;
        Ok(key)
    }

    /// Every record referenced by a manifest or delta record in a pack that will
    /// *survive* (any sealed pack not in `excluded`), transitively. These must
    /// never be deleted, even if unreferenced (garbage left by an interrupted
    /// put): a surviving parent pointing at a deleted child would make the index
    /// serve an object that can't be read.
    fn protected_children(&self, excluded: &HashSet<String>) -> Result<HashSet<Hash>> {
        let mut out = HashSet::new();
        let mut queue: VecDeque<Hash> = VecDeque::new();
        for m in self.packs.backend().list("packs/")? {
            let Some(name) = m.key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx")) else { continue };
            if excluded.contains(name) {
                continue;
            }
            let entries = match self.packs.pack_entries(name) {
                Ok(e) => e,
                Err(Error::KeyNotFound(_)) => continue, // deleted meanwhile
                Err(e) => return Err(e),
            };
            for e in entries {
                if e.frame.is_none() && matches!(e.kind, Kind::Manifest | Kind::Delta) {
                    queue.push_back(e.hash);
                }
            }
        }
        while let Some(h) = queue.pop_front() {
            match self.packs.chunks(&h) {
                Ok(Some(kids)) => {
                    for k in kids {
                        if out.insert(k) {
                            queue.push_back(k);
                        }
                    }
                }
                Ok(None) => {}
                // Already broken (e.g. by an older version): nothing more to protect below it.
                Err(Error::NotFound(_)) | Err(Error::Corrupt(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
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
        self.gc_with(opts, tracer, None)
    }

    /// GC driven by the registered fronts: the union of their tracers, plus
    /// `extra` (e.g. provenance), and their groupers when `grouped`.
    pub fn gc_with_fronts(&self, opts: &GcOptions, extra: Option<&dyn Tracer>, grouped: bool) -> Result<GcReport> {
        struct Both<'a>(&'a dyn Tracer, Option<&'a dyn Tracer>);
        impl Tracer for Both<'_> {
            fn children(&self, h: &Hash, store: &dyn ObjectStore) -> Result<Vec<Hash>> {
                let mut c = self.0.children(h, store)?;
                if let Some(x) = self.1 {
                    for k in x.children(h, store)? {
                        if !c.contains(&k) {
                            c.push(k);
                        }
                    }
                }
                Ok(c)
            }
        }
        let tracer = self.fronts().tracer(self)?;
        let grouper = self.fronts().grouper();
        let both = Both(&tracer, extra);
        self.gc_with(opts, &both, if grouped { Some(&grouper) } else { None })
    }

    /// Like [`LocalRepo::gc`], with an optional [`Grouper`] for similarity-grouped repack.
    pub fn gc_with(&self, opts: &GcOptions, tracer: &dyn Tracer, grouper: Option<&dyn Grouper>) -> Result<GcReport> {
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
            let mut live = self.mark(tracer, &mut report)?;
            let doomed: HashSet<&String> = rec.packs.iter().collect();
            // Children of records that survive must survive too (see protected_children).
            let doomed_owned: HashSet<String> = rec.packs.iter().cloned().collect();
            live.extend(self.protected_children(&doomed_owned)?);
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
                self.fault("gc.delete-idx")?;
                b.delete(&format!("packs/{p}.idx"))?;
                self.fault("gc.delete-pack")?;
                b.delete(&format!("packs/{p}.pack"))?;
                report.deleted_packs += 1;
            }
            for plan in plans.iter().filter(|p| p.id == rec.id) {
                self.fault("gc.delete-plan")?;
                b.delete(&plan.key)?;
            }
            self.fault("gc.delete-condemned")?;
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
                if opts.repack_all || garbage >= opts.min_garbage_ratio || size < opts.small_pack_bytes {
                    victims.push((p.clone(), entries));
                }
            }
            // Consolidating a single small, fully live pack is pointless churn.
            if !opts.repack_all && victims.len() == 1 && victims[0].1.iter().all(|e| live.contains(&e.hash)) {
                victims.clear();
            }
            if !opts.dry_run {
                let mut copied: HashSet<Hash> = HashSet::new();
                // Keep what refs reach, plus every child of a record in a pack that survives.
                let victim_names: HashSet<String> = victims.iter().map(|v| v.0.clone()).collect();
                let protected = self.protected_children(&victim_names)?;
                let to_copy: HashSet<Hash> = victims
                    .iter()
                    .flat_map(|(_, es)| es.iter().map(|e| e.hash))
                    .filter(|h| live.contains(h) || protected.contains(h))
                    .collect();
                if let Some(g) = grouper {
                    copied.extend(self.write_grouped(g, &to_copy, opts)?);
                }
                for (_, entries) in &victims {
                    for e in entries {
                        if to_copy.contains(&e.hash) && copied.insert(e.hash) {
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
                    self.fault("gc.retire-plan")?;
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

impl LocalRepo {
    /// Writes the grouper-assigned small blobs of `set` as frames, grouped and
    /// ordered (Exp1 V2). Returns the hashes written.
    fn write_grouped(&self, g: &dyn Grouper, set: &HashSet<Hash>, opts: &GcOptions) -> Result<HashSet<Hash>> {
        let keys = g.assign(self, set)?;
        let thr = self.packs.config().chunk_threshold as u64;
        let mut items: Vec<(&GroupKey, Hash)> = Vec::new();
        for (h, k) in &keys {
            if set.contains(h) && self.packs.chunks(h)?.is_none() && self.packs.size(h)? < thr {
                items.push((k, *h));
            }
        }
        items.sort();
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
        let mut done = HashSet::new();
        let mut wave: Vec<Vec<Vec<u8>>> = Vec::new();
        let flush_wave = |wave: &mut Vec<Vec<Vec<u8>>>| -> Result<()> {
            // Compress the frames of a wave in parallel (put_frame compresses before
            // taking the writer lock), bounded at 2 frames per core in memory.
            let results: Vec<Result<Vec<Hash>>> = std::thread::scope(|sc| {
                let hs: Vec<_> = wave
                    .iter()
                    .map(|f| {
                        sc.spawn(move || {
                            let refs: Vec<&[u8]> = f.iter().map(|v| v.as_slice()).collect();
                            self.packs.put_frame(&refs, opts.frame_level)
                        })
                    })
                    .collect();
                hs.into_iter()
                    .map(|h| h.join().unwrap_or_else(|_| Err(Error::Corrupt("frame worker panicked".into()))))
                    .collect()
            });
            wave.clear();
            results.into_iter().try_for_each(|r| r.map(|_| ()))
        };
        let mut i = 0;
        while i < items.len() {
            let group = &items[i].0.group;
            let mut frame: Vec<Vec<u8>> = Vec::new();
            let mut bytes = 0usize;
            while i < items.len() && &items[i].0.group == group {
                let d = self.packs.get(&items[i].1, None)?;
                if !frame.is_empty() && bytes + d.len() > opts.frame_cap {
                    break;
                }
                bytes += d.len();
                frame.push(d);
                done.insert(items[i].1);
                i += 1;
            }
            wave.push(frame);
            if wave.len() >= workers * 2 {
                flush_wave(&mut wave)?;
            }
        }
        flush_wave(&mut wave)?;
        Ok(done)
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
