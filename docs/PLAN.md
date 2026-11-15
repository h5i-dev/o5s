# pico2peta — Plan

Status: **pre-registered 2026-11-15**, before any experiment code exists.
The Experiment 1 thresholds in §4 are frozen. If they change later, the old values stay in this
file (struck through, with the date and the reason), and EXP1.md reports results against the
**original** values.

## 1. Milestones

**Revision 2026-11-15:** The human raised the target to production grade (see "Quality bar" in
AGENT_TASK.md) and split the work across two sessions. Lean and the model/difftest are deferred.
§4 is unchanged and still frozen.

Ownership: **core** = session `pico2peta` (branch `demo`). **net** = session `pico2peta-net`
(branch `net`), which owns the S3 backend, server, client, CI and deploy.

| # | Milestone | Owner | Done when |
|---|-----------|-------|-----------|
| M0 | Plan | core | this file; the §4 thresholds are frozen |
| M1 | Core API v1 | core | `Backend`, `ObjectStore`, `Repository` traits stable in `pico2peta-core`; LocalDisk passes the backend conformance suite |
| M2 | Pack store | core | multi-process safe (per-writer staging + flock, content-addressed create-only sealed packs, re-list on miss); streaming `put_stream`/`open` with bounded memory; FastCDC chunking + manifests; crash recovery tested by fault injection at every write/fsync point |
| M3 | Ref store | core | durable journal with checksummed records, an exclusive lock around validate+append+fsync (one commit point), catch-up replay from other processes, namespace rules, multi-process contention test |
| M4 | Local repo + CLI | core | `pico2peta-repo::LocalRepo: Repository`; CLI over `dyn Repository` (local, or remote via the net client) |
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
  core/        pico2peta-core        Hash, Error, Name/Rule/Observed, ObjectStore + Repository traits,
                                     backend::{Backend, LocalDisk, conformance}, fault injection
  pack/        pico2peta-pack        PackStore: packs, index v3 + mmap + MIDX, bundling, FastCDC + manifest
                                     trees, frames, XOR deltas, recovery
  refs/        pico2peta-refs        RefStore: journal + compaction, OCC replace/delete/transactions, rules
  repo/        pico2peta-repo        LocalRepo = PackStore + RefStore: Repository; GC/repack (grouped); fsck
  prov/        pico2peta-prov        provenance records, checkpointed log, lineage/impact, GC tracer
  weights/     pico2peta-weights     model-weights kind: tensor-aligned deltas for safetensors
  cli/         pico2peta-cli         binary `pico2peta` over dyn Repository (local or --remote)
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
  contract is in the `pico2peta_core::backend` docs.
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
  this is fair, but it would differ if pico2peta stored its own tree format.
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

Pure model, structurally mirroring `pico2peta-model`:
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

### 6.5 Net fronts

*(Added by the net session: Git protocol (clone/fetch/push time and bytes on the wire; Git storage size is
§6.3), HF/Xet, OCI, PyPI/npm/Maven. No measurement code for a front before its row is committed here.)*
