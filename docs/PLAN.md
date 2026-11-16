# o5s — Plan

Status: **pre-registered 2026-11-15**, before any experiment code exists.
The Experiment 1 thresholds in §4 are frozen. If they change later, the old values stay in this
file (struck through, with the date and the reason), and EXP1.md reports results against the
**original** values.

## 1. Milestones

**Revision 2026-11-15:** The human raised the target to production grade (see "Quality bar" in
AGENT_TASK.md) and split the work across two sessions. Lean and the model/difftest are deferred.
§4 is unchanged and still frozen.

Ownership: **core** = session `o5s` (branch `demo`). **net** = session `o5s-net`
(branch `net`), which owns the S3 backend, server, client, CI and deploy.

| # | Milestone | Owner | Done when |
|---|-----------|-------|-----------|
| M0 | Plan | core | this file; the §4 thresholds are frozen |
| M1 | Core API v1 | core | `Backend`, `ObjectStore`, `Repository` traits stable in `o5s-core`; LocalDisk passes the backend conformance suite |
| M2 | Pack store | core | multi-process safe (per-writer staging + flock, content-addressed create-only sealed packs, re-list on miss); streaming `put_stream`/`open` with bounded memory; FastCDC chunking + manifests; crash recovery tested by fault injection at every write/fsync point |
| M3 | Ref store | core | durable journal with checksummed records, an exclusive lock around validate+append+fsync (one commit point), catch-up replay from other processes, namespace rules, multi-process contention test |
| M4 | Local repo + CLI | core | `o5s-repo::LocalRepo: Repository`; CLI over `dyn Repository` (local, or remote via the net client) |
| M5 | Experiment 1 | core | `bench/exp1`, `docs/EXP1.md`, verdict against §4 |
| M6 | GC / repack | core | mark/sweep with a grace period that is safe against concurrent writers; repack (similarity grouping if Exp1 passes) |
| N1–N5 | S3 backend, server, client, CI, deploy | net | see AGENT_TASK_NET.md |
| M7 | Hardening | both | fuzz targets for every decoder, benchmarks with recorded numbers, CHANGELOG |
| M8 | Verification (deferred) | core | reference model + proptest difftest, Lean proofs P1–P3 (§5) |
| M9 | Later layers | both | provenance, Git/OCI adapters |

Every milestone ends with: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
green, a commit, and a `docs/PROGRESS.md` update. `net` is merged into `demo` at each
milestone, at least every ~2 hours.

## 2. Crate layout (Cargo workspace)

```
crates/
  core/        o5s-core        Hash, Error, Name/Rule/Observed, ObjectStore + Repository traits,
                                     backend::{Backend, LocalDisk, conformance}, fault injection
  pack/        o5s-pack        PackStore: packs, index v3 + mmap + MIDX, bundling, FastCDC + manifest
                                     trees, frames, XOR deltas, recovery
  refs/        o5s-refs        RefStore: journal + compaction, OCC replace/delete/transactions, rules
  repo/        o5s-repo        LocalRepo = PackStore + RefStore: Repository; GC/repack (grouped); fsck
  prov/        o5s-prov        provenance records, checkpointed log, lineage/impact, GC tracer
  weights/     o5s-weights     model-weights kind: tensor-aligned deltas for safetensors
  cli/         o5s-cli         binary `o5s` over dyn Repository (local or --remote)
  backend-s3/  (net)                 S3 Backend
  server/      (net)                 HTTP service (+ Git smart-HTTP, OCI distribution)
  client/      (net)                 remote Repository
bench/exp1/                          Experiment 1 harness
bench/micro/                         throughput and model-weights benchmarks
fuzz/                                cargo-fuzz targets (core and net)
lean/                                Lean 4 model, proofs and executable model (difftest)
```

*(Layout updated 2026-11-15 to match the code; §4 is unchanged.)*

## 3. Key design decisions (defaults; can be revised)

- **Identity.** `hash = BLAKE3-256(content)` of the whole logical object. Large objects are
  stored as a manifest (a list of chunk hashes and lengths) under the object hash. Chunks are
  ordinary blobs addressed by their own hash.
- **Backend contract.** Immutable, create-only blobs (`put_if_absent`), ranged reads, listing,
  and delete for GC only. Synchronous, `Send + Sync`. Retries happen inside the backend. The full
  contract is in the `o5s_core::backend` docs.
- **Packs.** These are records `[magic | kind | codec | hash | raw_len | stored_len | payload | crc32]`
  after an 8-byte versioned pack magic. Each writer handle owns its own staging file
  `staging/<random>.pack` and holds an exclusive flock on it. Sealing hashes the file, then runs
  `put_if_absent(packs/<blake3>.pack)` followed by `put_if_absent(packs/<blake3>.idx)`. The index's
  presence is the commit point, and pack names are content-derived, so concurrent or repeated
  seals are idempotent. On open, an orphaned staging file (its flock is free) is recovered: the
  torn tail is truncated, then the file is sealed.
- **Visibility.** `flush()` seals the writer's open pack, so the objects become visible to every
  other process. `replace` flushes before committing, which means a ref never points at an object
  that other readers can't see. Readers re-list `packs/` when a lookup misses. The resulting
  small packs are consolidated by repack (M6).
- **Streaming.** `put_stream` buffers at most `chunk_threshold` bytes. Above that it streams
  through FastCDC, hashing incrementally. `open` yields one chunk at a time, verifying each one.
- **Refs / OCC.** Every ref has `(hash, version)`. A read set is `[(name, Absent | Version(v))]`
  and must include the target. Commit happens iff every entry matches (Kung–Robinson validation;
  versions avoid ABA). The commit point is: exclusive flock on the journal, replay any records
  appended by other processes, validate, append a checksummed record, fsync, unlock.
- **Namespace rules.** These match on the longest prefix: `Mutable`, `ImmutableAfterPublish`
  (create-once), `HeadMutableReleaseImmutable` (`heads/*` mutable, `releases/*` create-once,
  everything else forbidden). A rule may only be set on a prefix with no existing refs, so
  immutability can't be retracted. Violations return `Forbidden`, which is distinct from
  `Conflict`.
- **Referential integrity.** `replace` requires `new_hash` to exist (`MissingObject`).

## 4. Experiment 1 — pre-registered protocol and thresholds

**Question.** On real source repositories, how close does *similarity-grouped zstd compression of
whole objects* (no explicit deltas) get to Git's most compact packing (`git gc --aggressive`)?

### 4.1 Corpus (fixed now)
Bare mirror clones (`git clone --mirror`) of:

| Repo | Why |
|---|---|
| `BurntSushi/ripgrep` | small, Rust |
| `jqlang/jq` | small/medium, C |
| `redis/redis` | medium, C, long history |
| `git/git` | large, long history, Git's own heavy-delta workload |

The exact ref snapshot (the `git for-each-ref` digest) and object counts are recorded in EXP1.md.
If one repo can't be processed in the 31 GB / 15 GB RAM budget, it is dropped and the drop is
reported. It is not replaced by a friendlier repo.

### 4.2 Object set
All objects reachable from all refs (`git rev-list --objects --all`) plus annotated tags: blobs,
trees, commits, tags. Both systems store **exactly the same object payloads** (Git's canonical
uncompressed object bodies, type-tagged), so the comparison isolates compression strategy only.

### 4.3 Baseline
On a fresh copy of each mirror: `git -c pack.threads=4 gc --aggressive --prune=now`
(Git 2.50.1 defaults: window 250, depth 50). **Baseline size B = bytes of `.pack` + `.idx`**.
`.rev`/`.bitmap` files are optional accelerators and are excluded, but reported. Also reported,
not judged: plain `git gc` (non-aggressive) size and the wall time of both.

### 4.4 Candidate variants (all reported; no variant is added after results are seen)
Grouping key for every grouped variant: objects are partitioned by `(type, extension of path)`.
Within a partition, objects are ordered by `(path, first-seen commit time)`, so all versions of a
file are adjacent and in history order. Trees use their directory path. Commits and tags are
ordered by commit time.

- **V0 — ungrouped:** each object zstd-19 compressed on its own. Strawman.
- **V1 — grouped + trained dictionary:** one zstd dictionary (≤ 112 KiB) per partition, each
  object compressed alone with it, at level 19. Fully random-access. Dictionaries count toward
  size.
- **V2 — grouped frames:** consecutive objects in a partition are concatenated into zstd frames
  of ≤ 4 MiB uncompressed, at level 19 with the window covering the frame. Reading one object
  decodes at most one frame.
- **V3 — unbounded (informational ceiling only):** each partition is one zstd `--long=27`
  frame at level 19. Not random-access and **not eligible** for the verdict.

**Our size S(V)** = sum of compressed payloads + dictionaries + an index at 48 bytes/object
(32-byte BLAKE3 hash + 8-byte offset + 4-byte length + 4-byte flags, i.e. what the real index
would cost).

### 4.5 Metrics and pass thresholds (frozen)
Per repo `R(V) = S(V) / B`. The verdict uses **V\* = whichever of V1, V2 has the lower geometric
mean R across repos**. Picking the better of two pre-declared variants is the only post-hoc
choice allowed.

| Verdict | Condition |
|---|---|
| **STRONG PASS** | geomean R(V\*) ≤ 1.00 **and** every repo R ≤ 1.15 |
| **PASS** | geomean R(V\*) ≤ 1.15 **and** every repo R ≤ 1.30 |
| **FAIL** | anything else |

Secondary thresholds, which V\* must also meet for PASS (measured warm-cache, 4 threads,
reported per repo):
- **Random read latency:** over 10,000 uniformly random objects, single-object `get`
  p50 ≤ 5 ms and p99 ≤ 25 ms.
- **Build time:** packing time from object payloads in memory/on scratch ≤ 1.0× the wall time of
  `git gc --aggressive` on the same repo.
- **Peak RSS** ≤ 6 GB.

What follows from each result (also decided in advance):
- PASS or STRONG PASS: grouped packing becomes the default repack strategy for the "source code"
  kind.
- FAIL: run Experiment 1b (explicit delta encoding against the previous version of the same
  path, then zstd). The thresholds above stay as they are, and 1b is judged against them as a
  separate experiment.

### 4.6 Threats to validity (acknowledged up front)
- Four repos is a small sample.
- Our index cost is modeled, not measured from a production index.
- Git's SHA-1-bearing trees compress poorly for both systems. Because the payloads are identical,
  this is fair, but it would differ if o5s stored its own tree format.
- Read latency is measured on the experimental reader, not the M2 pack reader.

### 4.8 Experiment 1b: pre-registered 2026-11-15, before any 1b code

**Trigger.** §4.5's rule applied: Experiment 1 FAILS. On redis, V2 has R = 1.336 > 1.30 (the per-repo
PASS limit), and its build ratio is 1.30 > 1.0. git.git hadn't finished at that point, but it can't change the verdict.

Everything in §4.1–§4.3 and §4.5 (corpus, object set, baseline, metrics, **thresholds**, V\* rule) applies
unchanged. The only changes are the candidate variants and their index model:

- **V4: delta, max chain depth 50.** Partitions and order are exactly as §4.4. An object's
  *predecessor* is the object immediately before it in partition order, if that one has the same path
  (blobs, trees). For commits and tags, which have no path, it's simply the previous object in the
  partition. If there's a predecessor and the predecessor's chain depth is < 50, the object is compressed
  at zstd level 19 with the predecessor's canonical bytes as a zstd reference prefix ("patch-from";
  `window_log = clamp(ceil(log2(prefix + object)), 23, 27)`). Otherwise it's compressed alone at level 19
  (depth 0). Every object is an individually addressable compressed record.
- **V5: delta, max chain depth 10.** The same, with depth limit 10 (cheaper reads).
- **Size model:** payload + 52 B/object (§4.4's 48 B plus a 4-byte predecessor reference).
- **Read latency** (§4.5 secondary): one random read decodes the whole chain from its depth-0 root up to
  the object. Same p50/p99 thresholds, warm cache.
- **V\*** = whichever of V4, V5 has the lower geometric-mean R. The verdict follows §4.5's table.
- **Consequences:** PASS → delta-then-zstd becomes the repack strategy for the source-code kind
  (with frames for objects without predecessors). FAIL → report honestly and keep Git-compatible
  storage as the recommended configuration for source code (net's Git adapter plus grouped repack, opt-in).
  Don't add further variants after seeing 1b results.

### 4.9 Experiment 1c: pre-registered 2026-11-15, before any 1c code (net session)

**Why a new experiment.** Experiment 1 and 1b both FAILED. §4.8 says "don't add further variants after
seeing 1b results", and that rule is honoured: **the 1 and 1b verdicts stand unchanged**. 1c is a *separate*
experiment commissioned by the human (docs/RESEARCH_SPEEDUP.md §3), with its own pre-registration.
Disclosed up front: its design was informed by the 1b failure analysis (1b only ever deltas against the
same path's previous version, while Git searches 250 candidate bases).

**Unchanged from §4.1–§4.5.** The corpus (ripgrep, jq, redis, git.git mirrors in
`$BENCH_DATA/mirrors`), the object set, the partitioning and in-partition order (§4.4), canonical payloads,
SHA-1 verification of every sampled read, warm-cache latency, threads = 4, and **all thresholds in §4.5**.
The baseline B for the verdict is still Exp1's measured `git gc --aggressive` (Git 2.50.1) size, reused from
`results.jsonl` as 1b did. The build-time threshold uses Exp1's measured `git gc --aggressive` wall time.
If a build ratio falls within ±20 % of 1.0 (the host noise seen in Exp1), both sides are re-timed back to
back under the bench lock, and the re-timed pair decides.

**Candidates** (both are reported, and V\* is the lower geomean R of the two):
- **V6: resemblance-selected base, patch-from delta, max chain depth 50.**
  - *Features (Odess-style content-defined sampling).*
    - Rolling Gear hash `h = (h << 1) + G[byte]` over the object, where G is 256 u64 values from
      splitmix64 seeded with `0x0DE55_5EED`.
    - A position is sampled when `h >> 57 == 0` (rate 1/128).
    - For each sample, 12 linear transforms `t_i = m_i·h + a_i (mod 2^64)`, with odd m_i and a_i from the
      same splitmix64 stream; feature `f_i` = the maximum of `t_i` over all samples.
    - 3 super-features `SF_j = FNV-1a-64(f_{4j} ‖ … ‖ f_{4j+3})`.
    - Objects with no sample (in practice < ~256 B) have no SFs.
  - *Candidates.* Within the object's partition, among the previous **W = 250** objects in partition order
    (bounded like Git's window), those whose base chain depth is < 50 and whose size is within 1/2–2× of
    the object.
  - *Score.* The number of super-features shared (0–3), plus 0.5 if the candidate has the same path. The
    best score wins, ties go to the nearest candidate, and a score of 0 means no delta (compress alone).
    With no SFs, the same-path predecessor (1b's base) is used if it is within W and depth < 50.
  - *Encoding.* Exactly as V4 (§4.8): zstd level 19 with the base's canonical bytes as reference prefix,
    `window_log = clamp(ceil(log2(base + object)), 23, 27)`, one individually addressable record per object.
- **V7: as V6, but trial-encoded.** The top **K = 3** candidates by score (any score ≥ 0.5, plus "no base")
  are each trial-compressed at zstd level 3. The smallest result's base is chosen and then encoded at
  level 19. This approximates Git's try-the-candidates search at bounded cost.
- **Size model:** payload + 52 B/object, as in §4.8. Features are build-time only and are not stored.
- **Read latency:** decode the whole chain, as in §4.8, with the same thresholds.

**Extra baselines** (reported, *not* judged). With **Git 2.56.1** (built from source; 2.50.1 lacks
`--path-walk`), on a fresh copy of each mirror:
- `git repack -adf --window=250 --depth=50 --path-walk`
- `git repack -adf --window=250 --depth=50 --name-hash-version=2`

Each is reported as R' = S(V\*)/B' next to the frozen R, with B' = `.pack` + `.idx`. If R passes but R'
does not, the text says so plainly.

**Verdict:** §4.5's table for V\*, unchanged.

**Consequences:**
- PASS: resemblance-selected delta becomes the recommended repack strategy for the source-code kind. The
  core lead decides on integration; `store.rs` is not touched by this experiment.
- FAIL: report honestly. Git-compatible storage stays the recommendation, and no further variants are
  added after 1c.

**Threats to validity (in addition to §4.6):**
- The design was informed by the 1b results.
- 4 repos remain a small sample.
- The extra baselines use a newer Git than the frozen baseline.
- Partitions are processed in parallel (4 threads), but candidate search inside a partition is sequential,
  which may make the build-time threshold hard to meet on the largest partitions.

## 5. Lean 4 properties (M8, done; see lean/README.md)

Pure model, structurally mirroring `o5s-model`:
- **P1 Content immutability:** for any operation sequence, if `get h = some d` at time t, then
  `get h = some d` at every later time, and `hash d = h` for every stored `d` (with an abstract
  injective `hash` as an explicit assumption).
- **P2 No undetected overwrite:** if two `replace`s validate against the same observed version of
  a name, at most one succeeds. Equivalently, a successful replace's read set exactly matches the
  pre-state.
- **P3 Immutable names can't move:** in an `ImmutableAfterPublish` or release namespace, once
  `resolve n = some h`, it stays `some h` forever.
- P4 (if time permits): authorization, meaning unauthorized principals can't read or move.
- P5 (if time permits): GC/repack safety, meaning every object reachable from a ref survives.

Explicit assumptions, not proved: the hash is collision-free; the backend's `put` is atomic and
durable; fsync is honest; the journal append is atomic at record granularity (enforced by a
checksum and truncating a torn tail, and covered by Rust tests, not proofs).

Toolchain: `leanprover/lean4:v4.31.0`, Mathlib by local path from
`/mnt/scratch/ec2-user/lake/arith/packages/mathlib`, and the `.lake` build dir on scratch.

## General amendment (2026-11-15, from the human): build-time thresholds

For every experiment **registered after 2026-11-15**, a build-time condition against a baseline is met when the
**geometric mean of the per-repo build ratios is ≤ 1.0×, and every repo's ratio is ≤ 1.25×**. The back-to-back re-time
rule (±20 % of 1.0) still applies to each repo's ratio. Experiments registered earlier (Exp1, 1b, 1c, 2, 3) keep their
own thresholds and verdicts. Context: Experiment 3 failed only on jq's re-timed build ratio (1.18×, about 2 s absolute),
and the human overrode that consequence (EXP1.md).

## 6. Phase 3 (2026-11-15): one store, uniform core, fronts (docs/VISION.md)

**Direction (from the human).** One store replaces Git hosting, Hugging Face, Artifactory, registries and the
rest. The core treats every domain the same. Domain-specific wins live in **fronts** that plug into a **Front
API** in the core. Exp1/1b/1c are findings for the Git front, not a reason to drop source code. Every front must
reach specialist level, and the thresholds are pre-registered here **before any Phase 3 measurement code exists**.

### 6.1 Milestones

| # | Milestone | Owner | Done when |
|---|---|---|---|
| P1 | Front API in the core | core | A per-namespace **domain profile**: chunker, encoding pipeline, codecs, derived-object records (generic `base + patch` with pluggable patch formats), grouping key, name rules. The existing domain pieces (weights XOR delta, byte grouping, the Xet chunker choice) sit behind it, and the core names no domain. Profiles are config, and reads never depend on them. |
| P2 | Windowed delta-search encoder | core | A reusable building block (git pack-objects style: type / name-hash / size ordering, window, depth, trial deltas) that emits derived-object records. Experiment 2 (§6.3) is run on it. |
| P3 | Generic front at specialist level | core | §6.4 is met and measured. |
| P4 | Fronts on the Front API | net | The Git, OCI and HF fronts (and PyPI/npm/Maven when built) are ported onto P1, and their thresholds are in §6.5 and measured. |
| P5 | Cross-domain | both | One authz/audit, provenance across fronts, dedup across domains, one GC policy, promotion and signing. |

**Rules for every Phase 3 measurement:**
- Thresholds and protocol are committed before the measurement code.
- No variant or threshold changes after official results are seen. A new attempt is a new, separately
  pre-registered experiment, and earlier verdicts stand.
- Timing runs hold `flock /mnt/scratch/pico2peta/bench.lock`.
- Raw results go to JSONL, and the verdict is computed mechanically by a script, as in Exp1.

### 6.2 Front API design constraints (not thresholds)
- The core stays domain-agnostic. A front may own: a name prefix (enforced by the core), an external-id index (a
  generic core primitive, e.g. the sorted id-segment map the net session built for Git/OCI), a GC tracer for
  what it references, encoding choices per put, and protocol code. A front can't change identity (BLAKE3 of the
  bytes `get` returns) or bypass OCC, rules, authz or GC safety.
- Derived objects (`base + patch`) are generic: the patch format is pluggable. XOR (weights), copy/insert (Git
  style) and zstd patch-from are formats, not kinds. Each has a decoder that is fuzzed and never panics.
- Every existing repository stays readable, with no migration.

### 6.3 Experiment 2: Git storage parity (core), pre-registered 2026-11-15, before any Experiment 2 code

> **Withdrawn 2026-11-15, before any Experiment 2 code was written.** The human replaced Phase 3 step (3)
> (a Git-style windowed delta-search encoder) with one generic, domain-agnostic similarity orderer and framer
> (docs/ORDERING.md). Its evaluation is §6.6 (Experiment 3). Git storage parity (≤ 1.00 per repo) is reported
> there as a line of the verdict. The text below is kept for the record and is not run.

**Question.** Can the core's generic building blocks, driven by a Git-class encoder, store a Git repository's
objects in no more space than `git gc --aggressive`, while keeping random reads fast?

**Corpus, object set, payloads, SHA-1 verification:** as in §4.1–§4.2 (ripgrep, jq, redis, git.git mirrors in
`$BENCH_DATA/mirrors`, canonical `"<type> <size>\0<body>"` payloads, every sampled read SHA-1-verified).

**Baseline B (frozen numbers, from Exp1's `results.jsonl`):** `git -c pack.threads=4 gc --aggressive
--prune=now`, Git 2.50.1, `.pack` + `.idx`:

| repo | objects | B (bytes) | git --aggressive wall (s) |
|---|---|---|---|
| ripgrep | 23,245 | 6,227,408 | 6.37 |
| jq | 24,771 | 16,161,931 | 12.24 |
| redis | 274,683 | 268,625,488 | 303.78 |
| git | 861,069 | 319,033,230 | 531.71 |

Extra baselines, reported and **not judged**: Git 2.56.1 `git repack -adf --window=250 --depth=50` with
`--path-walk` and with `--name-hash-version=2` (B'), as in §4.9.

**Measured size (no modeling).** S_total = every byte the system stores to serve these objects to a Git client:
- S_core: all files under the repository's `objects/` after the candidate's full repack (packs and indexes, in
  the production format).
- I: the Git front's external-id index for these objects (SHA-1 → BLAKE3), as actually stored. If the net
  session's index isn't ready, a core-built minimal one is used and disclosed.

R = S_total / B. S_core/B and payload-only are reported too.

**Candidates** (V\* = the lower geomean R):
- **V8: windowed delta search.** Git pack-objects-style ordering (type, then Git's v1 name hash of the path, then
  size descending), window **W = 250**, max delta chain depth **50**, trial deltas against every eligible
  candidate in the window (Git-like size bounds), keeping the smallest. The result is stored as core
  derived-object records (`base + patch`, copy/insert patch format), with whatever generic record grouping and
  compression the core provides.
- **V9 (optional): Git pack passthrough.** The Git front stores Git's own `--aggressive` pack representation as
  content-addressed objects, with reads resolving Git deltas. It is eligible only if implemented before the
  official run.

**Development discipline.** Encoder development and tuning may use **ripgrep and jq only** (the dev set). Before
the official run, the encoder commit is recorded in `docs/EXP1.md` (Experiment 2 section). redis and git.git are
measured only in the official run, which runs all four repos once on that commit.

**Thresholds (all must hold):**

| condition | threshold |
|---|---|
| size | **every repo R ≤ 1.00** (parity on each repo, not just on average) |
| random reads | 10,000 uniformly random objects, warm cache, **4 concurrent reader threads**: p50 ≤ 5 ms, p99 ≤ 25 ms per repo |
| build | full repack wall time ≤ 1.0× B's wall time (4 threads). If within ±20 % of 1.0, both are re-timed back to back under the bench lock, and the re-timed pair decides. |
| memory | peak RSS ≤ 6 GB |

**Verdict:** **PASS** if all hold, otherwise **FAIL**. Also reported: geomean R, R vs B', and R per component.

**Consequences:**
- PASS: V\* becomes the Git front's storage encoding.
- FAIL: reported honestly. Per VISION.md, source code is not dropped: the next attempt is pre-registered
  separately.

**Threats to validity, stated up front:**
- *Per-object overhead.* On git.git, the core index at 44 B/entry (32-byte BLAKE3 ids) against Git's ~28 B is
  ≈ 13.8 MB (4.3 % of B) before any record headers or the SHA-1 map. The encoder therefore has to beat Git's
  payload, not just match it, unless the core's per-object overhead is reduced first. Reducing it is allowed:
  it is generic core work, done before the official run.
- Four repos.
- Build time on a shared host.
- V8's design is informed by Exp1b/1c.

### 6.4 Generic front (core): thresholds, pre-registered 2026-11-15

The baseline is "files on a filesystem / S3 with versioning". Measured on this host (local disk), under the
bench lock, as the median of 3 runs:

| metric | baseline | threshold |
|---|---|---|
| durable write, 1 GiB incompressible | `dd if=… of=… bs=1M conv=fsync` on the same volume | ≥ 0.90× baseline throughput |
| verified read, 1 GiB (warm) | `cat file > /dev/null` (warm) | ≥ 0.50× baseline (the store verifies BLAKE3 on every read; plain files verify nothing) |
| 4 KiB range read in a 1 GiB object | — | p99 ≤ 1 ms (warm) |
| storage, 10 versions of a 1 GiB file, each with 0.1 % of bytes randomly rewritten (scattered 1-byte edits, deterministic seed) | S3-style versioning = 10 full copies | ≤ 1.15 GiB total, i.e. near-perfect dedup (each 64 KiB chunk containing an edit is stored again) |
| storage, the same 10 versions with 1 MiB inserted at a random offset each time | 10 full copies | ≤ 1.15 GiB total |

The scattered-edit case is the hard one for chunk dedup. 0.1 % of 1 GiB is about 1 M edits, which touch almost
every 64 KiB chunk. If the store can't meet it with chunking alone, a generic derived-object encoding
(`base + patch`) is the expected route. Either way the number is reported.

### 6.6 Experiment 3: generic similarity ordering and framing (core), pre-registered 2026-11-15, before any Experiment 3 code

**Question (docs/ORDERING.md).** Does one domain-agnostic building block (content features → similarity order →
similarity-aware frames with reference prefixes) beat the ad-hoc Git-specific layout (Exp1 V2: extension
partition, path order, fixed 4 MiB frames), reach `git gc --aggressive` size on source, and help outside Git too?

**Main corpus, object set, payloads, verification, size model:** exactly Exp1 (§4.1, §4.2, §4.4 canonical
payloads, SHA-1 verification of every sampled read). The size is S(V) = payload + any dictionaries or reference
metadata + the **48 B/object** modeled index, so the variants are directly comparable with Exp1's V0–V3. They are
implemented in the Exp1 harness (`bench/exp1`). **Baseline B:** the frozen Exp1 `git gc --aggressive` numbers
(table in §6.3).

**Variants (all reported). The ablations are the ones ORDERING.md lists:**

| id | order | framing |
|---|---|---|
| **A0** | V2: extension partition, path, history position (re-run for a same-host comparison) | fixed 4 MiB |
| **A1** | similarity order (below), with hints | fixed 4 MiB |
| **A2** | V2's path order | similarity cuts + reference prefix (below) |
| **A3** | similarity order, with front hints as tie-breakers | similarity cuts + reference prefix |
| **A4** | similarity order, **no hints** (content and size only) | similarity cuts + reference prefix |

**V\* = the lower geomean R of A3 and A4** (the full design, with and without hints). A0–A2 are reported as
ablations and are not eligible.

**Algorithm, fixed now (constants tunable on the dev set only, then frozen):**
- *Features:* Exp1c's Odess-style extractor (`bench/exp1c`: Gear rolling hash, sampling 1/128, 12 linear
  transforms, 3 super-features). Objects over 1 MiB get features per 64 KiB window, unioned. Objects with no sample
  (< ~256 B) have no super-features.
- *Order:*
  - Candidate neighbours are the objects sharing ≥ 1 super-feature (LSH: one bucket table per super-feature, with
    buckets larger than 256 truncated to the 256 nearest in size).
  - Similarity = number of shared super-features, then the number of equal features (of 12).
  - Greedy chaining: take the largest unplaced object, then repeatedly append the most similar unplaced
    candidate. Ties go to front hints in A1/A3 (same path, then nearest path, then nearest history position), and
    to nearest size in A4. When no candidate is left, start a new chain.
  - Then 2-opt on the chain, within windows of 64.
  - Featureless objects go last: by hints in A1/A3, by size in A4.
- *Framing:*
  - Cut before an object that shares no super-feature with any of the frame's last 8 objects, once the frame
    holds ≥ 64 KiB. The cap is C = 16 MiB raw (tunable, bounded by the read thresholds below).
  - At a forced cut inside a similar run (the next object shares super-features with the previous frame), the new
    frame is compressed with the most similar earlier object (from an earlier frame) as a zstd reference prefix.
  - Reference depth ≤ 2: a read decodes at most 3 frames.
  - zstd level 19 throughout.
- *Hints:* path and history position only (the same information V2 uses). They break ties and never decide the
  layout.

**Amendment 2026-11-15 (dev set only, before any redis/git.git run; disclosed).** Dev runs on ripgrep and jq
(4 reader threads, 2,000 sampled reads, numbers below) changed three things. None was tested on held-out data.

1. *Chain starts.*
   - **Change:** the next chain no longer starts at "the largest unplaced object". It starts at the unplaced object
     nearest to the last placed one by a coarse content sketch: a 64-bit SimHash of the byte histogram, searched
     among the 64 numeric neighbours on each side, by Hamming distance, then size. The first chain still starts at
     the largest object.
   - **Why:** super-features only detect near-duplicates, so with size-ordered starts adjacent chains were
     unrelated.
   - **Effect (4 MiB frames):** ripgrep A1 1.003 → 0.944, A3 0.924 → 0.863, and A4 0.863.
2. *Framing constants.*
   - **Change:** natural cuts are effectively off (`min = cap`). The cap is **6 MiB**, lookback 8, reference depth
     ≤ 2. These are the frozen values.
   - **Why:** natural cuts at 64 KiB–1 MiB cost 17–24 % (ripgrep A3 1.179 at 64 KiB, 1.021 at 1 MiB), because
     different files of one language still compress well together without sharing super-features.
   - **Effect of the cap:** 16 MiB fails p99 (≈ 33 ms), and 8 MiB fails ripgrep p50 (5.96 ms). 6 MiB gives ripgrep
     A4 0.847 (p50 4.0, p99 9.7 ms) and jq A4 0.794 (p50 3.6, p99 10.5 ms).
3. *Reads.*
   - **Change:** a read decodes its frame (and its reference's frame) only up to the object's end, using zstd
     streaming.
   - **Effect:** ripgrep p99 at 4 MiB fell from 18.3 to 6.6 ms.
   - **Size:** unaffected.

Ablation definitions are unchanged (A1 keeps fixed 4 MiB frames, and A2/A3/A4 use the framing above). Dev-set
reference for comparison: A0 (V2) = ripgrep 0.850, jq 0.928.

**Thresholds (§4.5, unchanged).** STRONG PASS: geomean R(V\*) ≤ 1.00 and every repo ≤ 1.15. PASS: geomean ≤ 1.15
and every repo ≤ 1.30. FAIL: anything else. Secondary thresholds, all required for (STRONG) PASS:
- random reads over 10,000 uniformly random objects, warm, with **4 concurrent reader threads**: p50 ≤ 5 ms,
  p99 ≤ 25 ms;
- build ≤ 1.0× the Exp1 `git gc --aggressive` wall time (4 threads; if within ±20 %, both are re-timed back to back
  under the bench lock);
- peak RSS ≤ 6 GB.

Also reported as its own line: **Git storage parity, every repo R(V\*) ≤ 1.00** (VISION.md's target).

**Generalization (each judged on its own, with the same variants minus the path-based ones where there are no
paths):**
- *Weights:* every tensor of SmolLM2-135M and SmolLM2-135M-Instruct as an object (544 objects, real bf16). The
  reference is the file order (all base tensors, then all instruct tensors) with fixed 4 MiB frames.
- *Packages:* the member files of 10 consecutive releases each of 3 pure-Python PyPI packages (decompressed from
  their wheels), objects = files. The reference is archive order with fixed 4 MiB frames. If the network can't
  reach PyPI, this corpus is reported as not run (disclosed), not substituted.
- *Criterion:* A4 (no hints) "generalizes" on a corpus if its size is ≤ 0.98× the reference ordering, with reads
  inside the thresholds above.

**Process.**
- Develop and tune only on ripgrep and jq (short runs), and on the generalization corpora.
- Record the frozen commit in EXP1.md before the official run.
- Run redis and git.git once, in the batch benchmark phase, under the bench lock.
- Generate the verdict mechanically by extending `scripts/exp1-report.py` (`EXP1_ELIGIBLE=a3,a4`).

**Consequences.**
- PASS: the orderer becomes the core's repack layout for every front. `Grouper`/`GroupKey` become hints.
- FAIL: reported honestly. The building block still ships if it beats A0 on every corpus; otherwise V2 stays.

**Threats to validity:**
- The design is informed by Exp1/1b/1c.
- Four repos.
- The 48 B/object index model (the production index is 44 B, plus record overhead).
- Generalization corpora are small.

### 6.7 Scale tests (ROADMAP item 4), pre-registered 2026-11-15, before any scale-test code

**Goal.** Find the first bottleneck at each scale and fix it, then record the limits in ARCHITECTURE.md. This is not
a pass/fail experiment on the product. The targets below say what counts as "a bottleneck to fix". A miss is
analysed, fixed if it's a defect, and re-measured, and the before and after are both reported.

**Workload** (`o5s-bench scale`, deterministic seed, local disk, bench lock, windows ≤ 60 min):
- **Objects:** N distinct 64-byte objects (random, so incompressible and no dedup), written with `put` in batches,
  `flush` every 10⁵. N = 10⁶, 10⁷, and 10⁸ only if the projected disk use (≈ 160 B per object, packs plus index)
  fits in the space left after keeping 5 GB free. Otherwise that scale is reported as not run, with the reason.
- **Names:** M names (`s/<i % 1000>/<i>`) set with `replace_many` in batches of 10⁴, M = 10⁶ and 10⁷ (same disk rule).
- **Provenance:** R records (one input, one output, small meta), R = 10⁶.

**Metrics and targets, per scale:**

| metric | target |
|---|---|
| put throughput (objects/s, including flushes) | at N = 10⁷ ≥ 50 % of the rate at the first 10⁶ |
| warm random `get`, p99 | ≤ 1 ms |
| cold open (new handle, index load) | ≤ 30 s at N = 10⁷ |
| RSS after open + 10⁵ gets, per object | ≤ 64 B/object (indexes are memory-mapped) |
| `replace` (single name) at M names | ≥ 1,000 commits/s |
| `resolve` p99 at M names | ≤ 1 ms |
| ref store open at M names | ≤ 10 s at M = 10⁶ |
| GC full cycle (plan, sweep, delete with zero grace) on N = 10⁷ (all live) | ≤ 30 min, RSS ≤ 6 GB |
| provenance `load` at R = 10⁶ | ≤ 10 s |

Each run reports load average, disk used, and the profile of whatever misses its target.

### 6.5 Net fronts, pre-registered 2026-11-15, before any front measurement code (net session)

**Common rules.** Every check uses the **real, unmodified client**. Timing runs hold the bench lock. Each
metric is the median of 3 runs. Raw results go to JSONL, and a script computes the verdicts. Specialist
baselines run locally on this host. A front's verdict tiers are, per metric:
- **STRONG PASS:** ≤ 1.00× the specialist (parity or better).
- **PASS:** ≤ 1.25×.
- **FAIL:** anything else.

Functional compatibility is pass/fail, and any failure fails the front. Disk budget: each benchmark cleans up
after itself.

| front | specialist baseline (local) | real clients | functional (all must pass) | measured (tiers above) |
|---|---|---|---|---|
| **Git protocol** | `git daemon` (Git 2.50.1) serving a bare mirror | git 2.50.1, protocol v2 and v0 | clone, fetch, push (incl. `--atomic`, tags, deletes), `fsck --full` clean on every clone | clone wall time, clone bytes on the wire, incremental fetch (last 10 commits) time, full push time; corpus ripgrep, jq, redis (heads + tags). **Storage size is §6.3** (core). |
| **HF/Xet** | none can run locally (the Hub is remote). The reference is the same files as plain objects through the generic `/v1` front, plus Xet-style chunk dedup computed with `crates/chunk` | `huggingface_hub` (version pinned at run time) **with its default `hf_xet` install**, and also with `HF_HUB_DISABLE_XET=1` | create_repo, upload_file, upload_folder (incl. LFS), hf_hub_download, snapshot_download, repo info / file listing, revision by branch and commit; downloaded bytes identical (sha256) | upload and download time of a real model (SmolLM2-135M, about 270 MB) vs the generic-front reference; **storage of a model family** (SmolLM2-135M + SmolLM2-135M-Instruct) vs Xet chunk dedup of the same files (Xet's dedup on HF) |
| **OCI** | CNCF Distribution (`registry`, v3 binary) on local disk | `oras` and/or `crane` (real binaries); the distribution-spec conformance suite | conformance: all 4 workflows, 0 failures (as now) | push and pull time for a set of real images (at least 3 related tags of one image plus 1 unrelated); stored bytes with cross-image dedup |
| **PyPI** | `devpi-server` (or `pypiserver` if devpi can't run) | `pip` and `uv` | install from the simple API (PEP 503/691), upload (`twine` or `uv publish`), **upstream proxy-cache** of pypi.org with exactly one fetch per file | proxy-cache **hit** latency (p50 per file over 50 files), cold-miss latency, stored bytes |
| **npm** | `verdaccio` | `npm` | install, publish, upstream proxy-cache of registry.npmjs.org | as PyPI |
| **Maven** | `reposilite` (or Nexus OSS if it fits the disk budget) | `mvn` | resolve dependencies, deploy, upstream proxy-cache of Maven Central | as PyPI |

**Cross-front checks** (reported, not tiered): one principal, policy and audit log across fronts, and dedup
of an identical file stored through two fronts (stored once).

**Consequences:**
- FAIL on a metric: reported honestly, and the next attempt is pre-registered separately, as in §6.1.
- Functional FAIL: the front is not done.

### 6.8 No-regression rule for the h5i-app rebuild (docs/H5I_PLAN.md), pre-registered 2026-11-16, before any h5i code

**Baseline.** `docs/BASELINE.md` on the commit tagged `baseline-pre-h5i`. It covers:
- the Exp3 official results (§6.6);
- the core suite `scripts/baseline-core.sh micro|scale` (3 reps for micro, 1 for scale);
- the §6.4 generic-front run;
- net's §6.5 final batch.

Every number carries its commit, host load and raw-data file.

**Rerun (H3).** After the rebuild, the same benchmarks run on the rebuilt commit:
- the same scripts, commands, corpora, inputs and repetition counts;
- in announced bench windows, under the bench lock;
- the load is reported per result, as for the baseline;
- a result whose load makes it unusable is rerun once later, as in the batch protocol.

**Rule.**
1. **Metrics.** Every baseline metric must be no worse than its baseline value by more than the larger of 3 %
   or that metric's baseline run-to-run range (max − min over the baseline reps, relative to the baseline
   median).
   - Metrics with one baseline rep use 3 %.
   - "Worse" means lower for throughputs and higher for times, latencies, sizes and memory.
   - The compared values are medians where there are reps.
2. **Tiers.** No pass/fail tier may get worse: STRONG PASS → PASS, PASS → FAIL, or a met threshold becoming missed.
3. **Exp3 sizes** must be byte-identical. The one allowed exception is an intentionally unchanged encoding
   whose layout differs only by deterministic reordering; that is reported with the reason.
4. **Functional checks** (net's real-client checks, the gate) must all still pass.

**Consequence.** A regression is a FAIL. It is fixed before the release candidate, not rationalized. The RC
(`v0.1.0-rc.1`) is tagged only after the rerun passes this rule.

**Amendment 2026-11-16 (human ruling, written before these runs).** The three core H3 FAILs on `cbbbb8a`
(BENCH 2026-11-16 H3) stay recorded as FAIL by the letter; those rows are not edited. Each is decided by
**one** pre-declared, better-controlled re-measurement on the final H3 commit, under the bench lock with the
other session idle. The start load and a 10 s process log are recorded with each. The result is acted on: if a
metric still regresses, it is a real regression, found and fixed before H4, not explained away.
1. **Ingest put.** A quiet window (load < 2.5 at start). The `baseline-pre-h5i` and H3 `o5s-bench` binaries
   run interleaved (`ingest <redis dump> <dir> 1024`, the same input as the suite) for **5 rounds**,
   alternating baseline then H3. The verdict compares the two medians of `put` MB/s against the §6.8
   allowance: max(3 %, the baseline binary's range in these 5 rounds).
2. **Durable-write tier (§6.4).** The H3 `o5s-bench generic <dir> 1024` runs **5 times** in one window. Each
   run measures our put and `dd conv=fsync` back to back, giving one ratio per round. The verdict is the median
   of the 5 ratios against the tier threshold of 0.90×.
3. **Redis A3 peak RSS (Exp3).** The redis A3 build (`exp3 run <redis work dir> a3 4 10000`), **3 reps** each
   with the baseline and the H3 `exp3` binary, interleaved. The verdict compares the two medians of
   `peak_rss_kb` against the §6.8 allowance: max(3 %, the baseline binary's range in these 3 reps).

**Amendment 2026-11-16 (human ruling, H4 HF target; written before the final batch).** For HF, the §6.5
upload and download times and the H4 target (≤ 1.2× the `/v1` reference) are judged on **server-side busy
time**, not on the client's wall time. The reason: dev checks showed that the real client's own CPU
(`huggingface_hub` and `hf_xet`: chunking, hashing, LZ4, reconstruction, SHA-256) is above the target even
with a zero-time server (NET_PROGRESS, H4).
- **Measured.** The same file bytes for each operation, Xet upload, Xet download, LFS upload and LFS
  download, compared with `/v1` PUT and GET of the same file. Medians of 3 runs, as before.
- **Server-side busy time.** The union over the operation of each request's interval on the server. A
  request starts when it is received and ends when its response body has been fully sent (the
  end-of-stream event), so streamed download bodies count, not just time to first byte. Overlapping
  requests count once. It is computed from the server's response log exactly as in
  `bench/fronts/hf_split.py`, for the HF operations and for the `/v1` reference alike.
- **Verdict.** Per operation, H4 meets its target when the server busy time is ≤ 1.2× `/v1`'s. The §6.5
  tiers apply to the same ratio (STRONG PASS ≤ 1.00×, PASS ≤ 1.25×).
- **Informational rows (not judged against the H4 target).** End-to-end client wall time and client CPU
  (`getrusage`, all threads) per operation, next to `/v1`'s wall time. The baseline's end-to-end rows stay as
  recorded. The new server-side rows have no pre-h5i baseline. This amendment does not change how §6.8
  treats the end-to-end rows.

**Amendment 2026-11-16 (human rulings on the H4 HF questions; written before net's final-batch windows).**
1. **Downloads (Xet and LFS) are judged on server CPU time** (user + system, all threads, of the server
   process over the operation) against `/v1`'s server CPU for the same bytes, ≤ 1.2× (tiers as above).
   Server busy time stays an **informational** row for downloads, because a streamed body's interval
   lasts until the client has read it, so it includes `huggingface_hub`'s own read speed. Uploads stay
   judged on server busy time.
2. **LFS upload keeps the Xet chunker** (`hf/` profile) and is **recorded as a FAIL** against the 1.2×
   target at its measured ratio (dev: 1.34×, serial Xet chunker on the put path). The reason is that
   LFS↔Xet chunk dedup (the same file uploaded through LFS and through Xet sharing chunks) is worth more
   than the time. FastCDC for LFS PUTs measured 1.04× in a dev check but is not applied. **Post-RC item:**
   a double-buffered parallel Xet chunker in the store's put path. `pico2peta_chunk::xet_cuts_parallel[_seg]`
   already gives byte-identical cuts. A first, non-double-buffered wiring was slower and was reverted.
