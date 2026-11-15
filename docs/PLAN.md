# pico2peta — Plan

Status: **pre-registered 2026-11-15**, before any experiment code exists.
The Experiment 1 thresholds in §4 are frozen. If they change later, the old values stay in this
file (struck through, with the date and the reason), and EXP1.md reports results against the
**original** values.

## 1. Milestones

| # | Milestone | Done when |
|---|-----------|-----------|
| M0 | Plan (this file) | PLAN.md committed with the thresholds in §4 |
| M1 | Workspace + object layer | `Hash` (BLAKE3-256), `ObjectStore` trait, `put`, `get(hash, range)`, in-memory store; unit tests |
| M2 | Physical layer | Append-only packs + index on a `Backend` trait (local disk; S3 stub); small-object bundling; FastCDC chunking + manifest for large objects; ranged reads that fetch only the chunks they cover; crash recovery by rescanning the open pack |
| M3 | Name layer | Refs, `resolve`, `replace(name, read_set, new_hash) → Ok \| Conflict`, namespace rules (mutable / immutable-after-publish / head-mutable-release-immutable), durable ref journal |
| M4 | CLI | `pico2peta init/put/get/resolve/replace/ns/stat` over a local repo directory |
| M5 | Reference model + differential tests | Pure executable model (`pico2peta-model`); proptest drives random op sequences against model and implementation (including reopen/recovery) and compares every observable result |
| M6 | Experiment 1 | `bench/exp1` harness, results + methodology in `docs/EXP1.md`, verdict against §4 |
| M7 | Lean 4 core | `lean/`: model of objects/refs/replace; proofs P1–P3 (§5), P4–P5 if time allows |
| M8 | Later layers | Provenance, Git/OCI adapters, GC. Starts only after M1–M7 are solid |

Every milestone ends with: `cargo test` green, no build warnings, a commit on `demo`, and a
`docs/PROGRESS.md` update.

## 2. Crate layout (Cargo workspace)

```
Cargo.toml                 workspace
crates/
  core/     pico2peta-core     Hash, Range, errors, ObjectStore trait, Name, ReadSet types
  pack/     pico2peta-pack     physical layer: Backend trait (LocalDisk, S3 stub), pack writer/reader,
                               index, bundling, FastCDC chunking, PackStore: ObjectStore
  refs/     pico2peta-refs     name layer: namespace rules, RefStore (memory + journaled file)
  model/    pico2peta-model    pure reference model (BTreeMaps, no I/O) — mirrors lean/
  cli/      pico2peta-cli      binary `pico2peta`
  difftest/ (tests only)       proptest differential tests: model vs implementation
bench/
  exp1/     pico2peta-exp1     Experiment 1 harness (git object extraction, grouping, zstd)
lean/                          Lean 4 project (toolchain v4.31.0, Mathlib by local path)
docs/                          DESIGN_BRIEF, AGENT_TASK, PLAN, PROGRESS, EXP1
scripts/                       env.sh (scratch paths), exp1 driver scripts
```

Dependencies, kept small: `blake3`, `fastcdc`, `zstd` (with `zdict_builder`), `thiserror`,
`clap`, `proptest` (dev), `tempfile` (dev).

## 3. Key design decisions (defaults; can be revised)

- **Identity.** `hash = BLAKE3-256(content)` of the whole logical object, whatever its size.
  Chunking is a physical detail. A large object is stored as a *manifest* (a list of chunk hashes
  and lengths) indexed under the object's hash, so `put` and `get` never expose chunks.
- **Packs.** A pack is an immutable, append-only file of self-delimiting records
  `[magic | hash | kind | codec | raw_len | stored_len | payload | crc32]`. When a pack is sealed
  (at a size cap, 256 MiB by default), its sorted index file `<id>.idx` (hash → pack, offset,
  length, kind) is written. On restart, the one unsealed pack is rescanned and a torn trailing
  record is truncated.
- **Bundling.** Objects under `chunk_threshold` (default 1 MiB) are records in the current pack.
  Larger objects go through FastCDC (min/avg/max 16/64/256 KiB), and each chunk is stored as its
  own record, deduplicated by chunk hash.
- **Compression.** Each record is zstd-compressed and stored raw if that doesn't help.
  Similarity-grouped repacking is what Experiment 1 tests; it ships in the physical layer only if
  §4 passes.
- **Refs / OCC.** Every ref has `(hash, version)`, and `version` goes up by one on every
  successful change. A read set is a list of `(name, observed)` with
  `observed ∈ {Absent, Version(v)}`. `replace` commits **iff** every entry still matches the
  current state, which is OCC validation over the read set (Kung–Robinson). Versions rather than
  hash values avoid ABA. The target name must be in the read set, so blind writes are impossible.
- **Namespace rules** (by longest matching prefix):
  - `Mutable`: any validated replace is allowed.
  - `ImmutableAfterPublish`: allowed only when the name is `Absent` (create once).
  - `HeadMutableReleaseImmutable { heads, releases }`: names under `heads/` are mutable, names
    under `releases/` are create-once, and anything else in the namespace is rejected.
  A rule violation is a distinct error (`Forbidden`), not a `Conflict`.
- **Referential integrity.** `replace` requires `new_hash` to exist in the object store
  (`MissingObject` otherwise).

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

## 5. Lean 4 properties (M7)

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
