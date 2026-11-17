# o5s architecture and on-disk formats

This is the reference for operators and implementers. The code is authoritative; each section names
the module that implements it. All integers are little endian.

## Repository layout (`LocalRepo`, crates/repo)

```text
<root>/config.toml         format = 1, plus [pack] tunables, [refs] store, optional [chunking] (written once at init, create-only)
<root>/staging/<id>.pack   open packs, one per writer handle, each exclusively flock'ed
<root>/objects/            LocalDisk backend root (another Backend, e.g. S3, may be used instead)
    packs/<P>.pack           sealed packs; P = BLAKE3 of the pack bytes (create-only)
    packs/<P>.idx            sorted index for P; its existence is the pack's commit point
    gc/<ts>-<id>.plan        GC: packs eligible for the next sweep
    gc/<ts>-<id>.condemned   GC: packs to delete once the grace period has passed
<root>/refs/journal[.<g>]  ref journal, generation g (0 = "journal")
<root>/refs/snapshot.<g>   full ref state at the start of generation g (g ≥ 1)
<root>/refs/CURRENT        current generation number (a hint; tombstones are authoritative)
<root>/idx-cache/          derived, safe to delete: mapped *.idx3 copies and midx-*.midx
```

## Objects (crates/pack)

**Identity.** An object's id is BLAKE3-256 of its full content, whatever its size or storage form.
`get(id)` always returns bytes that hash to `id` (verified on every whole read; ranged reads verify
every record they touch).

### Pack file (`O5SPACK2`)
`"O5SPACK2" | nonce[16] | record*`. The nonce makes every pack's bytes unique, so a pack's
content-derived name never aliases another pack (GC relies on this). `O5SPACK1` (no nonce) is still read.

### Record
```text
magic u32 "O5SR" | kind u8 | codec u8 | 0u16 | hash[32] | raw_len u64 | stored_len u32 | payload | crc32 u32
```
`codec`: 0 raw, 1 zstd, 2 grouped zstd (blobs only: `width u8 | zstd(group(raw, width))`, where `group` lays out
byte `k` of every `width`-byte element as plane `k`, then the `len % width` tail; used for bf16/fp32 weights and their
XOR deltas when the writer passes a width hint). `kind`:

| kind | payload | hash / raw_len |
|---|---|---|
| 0 Blob | the object (≤ 2 GiB) | object hash / length |
| 1 Manifest | `n u32 | (child hash[32], len u64)*` | hash/length of the byte range it covers |
| 2 Frame (v1, read-only) | `n u32 | (hash[32], len u32)* | bodies` | BLAKE3 of concatenated bodies |
| 3 Frame2 | `n u32 | len u32* | bodies` | BLAKE3 of concatenated bodies |
| 4 Delta | `base[32] | xor[32] [| n u32 | (dst, src, len u64)*]` | object hash / length |
| 5 Frame3 | `ref[32] | n u32 | len u32* | bodies`, bodies zstd-compressed with object `ref`'s bytes as reference prefix | BLAKE3 of concatenated bodies |

- **Large objects** (≥ `chunk_threshold`) are content-defined chunks stored as blobs, under a tree of manifests
  with at most `manifest_fanout` entries per node. Every node is keyed by the content hash of its range.
  The chunker is FastCDC v2020 (`cdc_min/avg/max`) by default, or byte-exact Hugging Face Xet (8/64/128 KiB)
  per name prefix (`[chunking]` in config.toml, used by `put_stream_for`). The format doesn't record which
  chunker cut an object; reads never depend on it.
- **Frames** hold many small objects compressed together. Members are indexed individually; a frame isn't
  itself addressable. A grouped repack lays them out by content similarity (`crates/order`, docs/ORDERING.md):
  - features, then a similarity order (front hints only break ties), then frames of up to 6 MiB;
  - a frame cut inside a similar run is written as a **Frame3**, compressed against its most similar earlier
    object (reference depth ≤ 2);
  - a read decodes a frame only as far as the member it needs (the cached prefix at least doubles on each
    re-decode, and a request past half the frame decodes, and verifies, all of it). The record CRC and each
    member's content hash are always checked;
  - a Frame3's reference is a GC child of every member, and its members keep kind 5 in the index, so GC,
    `ensure_live` and recovery follow it. Recovery resolves a reference from earlier records of the same staging
    file.
- **Deltas**: object = (aligned base) XOR `xor`, where `xor` is an ordinary object. With segments, object
  bytes `[dst, dst+len)` align with base bytes `[src, src+len)`. Uncovered bytes are XORed with zero.

### Pack index (`O5SIDX03`)
```text
"O5SIDX03" | n_rec u64 | n_ent u64 | rec[n_rec] | ent[n_ent] | blake3[32]
rec = offset u64 | stored_len u32 | raw_len u64 | kind u8 | codec u8           (22 B)
ent = hash[32] | rec u32 | inner u32 | len u32   (sorted by hash; 44 B)
```
Frame members share their frame's record (`inner`/`len` locate the member). v1 (`O5SIDX01`, 54 B
entries) and v2 (`O5SIDX02`, 62 B) are still read.

### Multi-pack index (`O5SMIDX1`, local and derived)
`"O5SMIDX1" | n_packs u32 | name[64]* | n u64 | (hash[32], pack u32, entry u32)* | blake3[32]`.

## Write path and concurrency (crates/pack)
1. A writer appends records to its own staging file (`flock`ed). Small objects are bundled one record
   each. Large ones are chunked, and chunks already present are deduplicated (ignoring condemned copies).
   `put_stream` is a pipeline: the caller's thread chunks; workers hash, check dedup, compress (after a
   4 KiB zstd-1 compressibility probe) and encode records; one thread hashes the whole object; one
   committer appends in chunk order and builds the manifest tree. The open pack's BLAKE3 is kept
   incrementally, so sealing doesn't read the pack back.
2. `flush`/seal: fsync, hash the file, `put_if_absent(packs/P.pack)` (hard link on local disk), then
   `put_if_absent(packs/P.idx)`, unlink staging, fsync the directory. Repeated or concurrent seals are
   idempotent.
3. Recovery on open: any staging file whose lock is free belongs to a dead writer. It's scanned record
   by record, truncated at the first torn or corrupt record, and sealed.
4. Readers re-list `packs/` on a miss, and before trusting a dedup hit if their view is older than 60 s.

## Names (crates/refs)

Journal file: `"O5SREFJ1" | (len u32 | crc32 u32 | payload)*`. Payload tags:

| tag | meaning |
|---|---|
| 1 | Set(name, hash, version) |
| 2 | Rule(prefix, rule) |
| 3 | Rotated(next generation): tombstone, the commit point of compaction |
| 4 | Batch (replace_many) |
| 5 | Delete(name, last version) |
| 6 | Txn (update_many: nested Set/Delete records) |

**Commit point.** Take an exclusive `flock` on the current journal, catch up (truncating a torn tail),
validate (input → read set → rules), append one record, fsync, unlock. Reads take a shared lock.
**Compaction.** Write `snapshot.<g+1>` (`O5SREFS1` + records + crc32) and an empty `journal.<g+1>`,
append `Rotated(g+1)` to the old journal (the commit point), then update `CURRENT`. Handles that reach
the tombstone switch generations.

**Rules** match on the longest prefix: `mutable`, `immutable` (create-once, never deleted),
`head-release` (`heads/*` mutable, `releases/*` create-once, anything else forbidden). A rule can only be
set on a prefix that has no refs, so immutability can never be retracted.

## Fronts (crates/repo/src/front.rs; docs/VISION.md)

The core is domain-agnostic. Domain behaviour comes from **fronts** registered when a repository is opened
(`LocalRepo::with_fronts`).
- **Encoding plan.** `Front::plan` returns an `Encoding` for each put: chunker, byte-grouping width, and whether to
  store the object as a derived `base + patch` object (patch format: XOR with alignment segments; more formats to
  come). It only affects storage: identity is always the BLAKE3 of the bytes `get` returns.
- **Owned prefixes.** Names under a claimed prefix (longest claim wins, so `git/` and `git/hf/` nest) can only be
  moved through `LocalRepo::front(id)`. Reads are unrestricted.
- **GC and repack.** Every front sees every object; GC follows the union of their children
  (`gc_with_fronts`), and repack merges their grouping keys.
- **Profiles.** `[profiles.<p>]` (`front`, `chunker`, `derive = "none" | "previous"`) assigned to name prefixes in
  `[namespaces]` decide what `put_named(name, src)` does. `derive = "previous"` offers the name's current object as
  the base.
- **External ids.** `idmap` (sorted, content-addressed segments behind one ref, union-merged with OCC) maps Git
  oids, OCI digests and so on to core hashes.
- **Built-in fronts:** `safetensors` (crates/weights: grouping width from the dominant dtype, tensor-aligned XOR
  delta against the previous version). Net's Git, OCI and HF fronts are being ported onto this API.

## GC (crates/repo/src/gc.rs)

Phases, each at least `grace` (≥ 10 min) apart:
- **plan:** list live packs.
- **sweep:** mark from refs, through the `Tracer`. Copy live records out of mostly-garbage or small packs, then
  write `condemned`. By default (`[gc] similarity_repack = true`, CLI `gc --no-similarity-repack` to disable),
  live small objects are laid out by content similarity (frames and Frame3). A `Grouper` (a front) only adds
  tie-breaking hints. This is the human's decision after Experiment 3 (EXP1.md).
- **delete:** mark again, rescue anything live that is only in condemned packs, verify, then delete
  `.idx` before `.pack`.

`replace` calls `ensure_live`, which rewrites a condemned-only target, so a slow writer can't create a
dangling ref.

## Trees (crates/tree; kernel `o5s_kernel::tree`)

A **tree** is a standard, domain-agnostic object: a snapshot of one directory. The core stores bytes only, so
keeping a directory as one name per file loses file modes and the directory structure, and moving N names is
N ref writes. A tree is one object, and one name can point at it.

**Format** (canonical; one encoding per directory, so equal directories have equal hashes):

```
"O5STREE1" | entry* ,  entry = mode u8 | name_len u16 LE | name | hash [32]
```

- **mode:** 0 regular file, 1 executable file, 2 symlink (the hash is a blob of the link target's bytes),
  3 subtree (the hash is a tree).
- **name:** one path segment as raw bytes, 1 to 65,535 bytes, no `/`, no NUL, not `.` or `..`. Names are not
  ref names, so they need no name encoding; any byte string a filesystem allows works.
- Entries are strictly increasing in byte order, so names are unique.
- Nothing follows the last entry, and the whole object is at most 2 GiB.
- The identity is the BLAKE3 of these bytes, like every object. Storage (packs, chunking, compression) is
  unchanged.

**Kernel and proofs.** `decode`, `encode`, the canonical-form `check` and the GC view `children` are h5i
kernels, extracted with Charon/Aeneas. `verify/core/proofs/Tree.lean` proves, on the extracted code:
- `decode_spec`: the decoder is total and equals its spec on every input. It accepts exactly the bytes that
  are `encodeL es` for a canonical entry list `es`, and returns that `es`.
- `encode_spec` / `check_spec`: the encoder produces exactly `encodeL es`, and refuses exactly the
  non-canonical lists.
- `decode_encode`, `encode_decode`, `encodeL_inj`: the two round trips, and that the encoding is unique.
- `children_spec`: the tracer returns exactly the entry hashes, in order, and fails on anything that is not a
  canonical tree.

Mutants in `h5i-app.toml`: duplicate names accepted, mode 4 accepted, `.` accepted, the tracer drops an
entry. The fuzz target `tree` is local only.

**GC.** `TreeFront` (id `tree`, claims no names) reports a tree's entry hashes as its children, so blobs and
subtrees referenced only by a live tree stay live. It reads the object only when its first 8 bytes are the
magic. Bytes that merely start with the magic but are not canonical are leaves (never an error).
- Like any front, it must be registered in every process that runs GC on the repository. The CLI registers
  it. A GC without it collects tree-only blobs; `crates/tree/tests` keeps a control test for that.
- The server registers it too (a request to net).
- `put_tree` calls `ensure_live` on every entry first. So a tree never references a missing or condemned
  object, which mark would report as corruption.

**Filesystem side.**
- `put_dir` (CLI `o5s tree put DIR [--exclude NAME]`) walks without following symlinks. It stores files with
  `put_stream`, records the user-exec bit as mode 1, and refuses special files. Everything goes through one
  writer with one seal at the end, so the printed hash is durable.
- `checkout` (CLI `o5s tree get TREE DIR`) needs DIR absent or empty. It creates every file new, creates
  symlinks without following them, and writes only below DIR, since names are single segments. Executables
  are 0755 and other files 0644, under the umask. Reads are verified. A checkout is not fsynced: it is a
  working copy, not the store.
- `diff` (CLI `o5s tree diff A B`) merge-joins sorted entries. It skips equal subtree hashes without reading
  them and lists leaf paths as A/D/M/T.
- Checkout and diff are iterative, with a depth limit of 4,096.

## Provenance (crates/prov)

- Records `O5SPROV1`: canonical `op | params? | inputs | outputs | meta`.
- Log nodes `O5SPLOG2`: `prev? | checkpoint? | total u64 | records`, with the head at ref `_prov/log`.
- Checkpoint index `O5SPIDX1`: `n_fwd u64 | n_bwd u64 | (key, record)*`, both tables sorted. It's rebuilt
  when the recent records reach max(1024, total/8).

## Verification map

Every decision below is an h5i-app kernel in `crates/kernel`, extracted with Charon/Aeneas; the theorems
are about the extracted code (`verify/core`, docs/H5I_PLAN.md). Authorization (P4) is net's kernel
(`verify/net`).

| property | theorem (verify/core/proofs) | tests (Rust) |
|---|---|---|
| P1 a read returns only what was stored under its key; puts never overwrite | `read_accepts_only_key`, `put_never_overwrites` | read-path verification, fuzz, `fsck` |
| P2 no undetected overwrite; versions | `commit_validated`, `commit_versions`, `decide_spec` (total) | contention tests (threads, processes), kernel-vs-model proptest |
| P3 published names never move (incl. deletes, transactions) | `commit_respects_rules`, `mobility_of_spec`, `governing_spec` | rule tests, model proptest |
| P4 unauthorized principals can't move/read | net: `guarded_transition.authorized` (`verify/net`) | server authz tests |
| P5 GC never deletes a live object's only copy | `delete_keeps_live` (+ `is_victim_spec`) | GC crash sweep, concurrent-writer GC, SIGKILL test, Frame3 reference GC test |
| P6 a front's names (and rule prefixes) are written only through its handle | `guard_owner`, `may_set_rule_spec` | nested-claims and every-write-path tests (`crates/repo/tests/front.rs`) |
| P7 promotion moves only `dst`, never from a stale `src` | `promote_plan`, `promote_src_fresh` | `crates/attest` promotion test |
| retention keeps the newest N and deletes only what rules and provenance allow | `plan_spec` | `crates/prov/tests/retention.rs` |

Crash consistency (fsync honesty, atomic rename, create-only puts) is assumed by the proofs and tested
by fault injection at every write/fsync point plus real SIGKILLs.

## Measured limits (PLAN §6.7, single host, 2026-11-15)

| dimension | measured | how it scales |
|---|---|---|
| objects | 10⁷ × 64 B: put 201k/s, cold open 3.6 s, get p99 13 µs; GC full cycle 111 s at 2.1 GB peak; 226 B/object on disk (64 B data + 56 B record framing + 66 B pack index + ~40 B MIDX). 10⁸ not run: ≈ 22.6 GB would not fit the disk rule | Indexes over 4 MiB are memory-mapped (hard links into `idx-cache` on local disk), plus a multi-pack index. Heap is 3.4 B/object; resident memory is ≈ 110 B/object, almost all reclaimable mapped-file pages (RssFile). |
| names | 10⁷: replace 7.5k/s, resolve p99 7.6 µs, open 6.4 s | The whole ref state is in memory: ≈ 208 B per name (2.1 GB at 10⁷). For more names, use idmaps (sorted segments behind one ref) for name-like tables, as the Git and OCI fronts do. |
| provenance | 10⁶ records: load 0.35 s (checkpointed index) | Records are objects; the index grows with records since the last checkpoint. |
| batch name updates | `update_many` linear in the batch (≤ 2²⁰ entries) | |
| verified read (§6.4, 1 GiB, 2026-11-16) | strict 0.30×, eof 0.41× of warm `cat` on 4 cores: **the 0.50× target is missed** (recorded by the human's decision) | The whole-object BLAKE3 runs on several cores (`o5s_pack::phash`), but strict hashes every byte twice (per chunk and whole object). More cores would raise both ratios; re-measuring on a many-core host is a post-RC item. |

## Deployment topology and limits

- **Ref store choice** (`[refs] store` in config.toml, or `o5s init --refs-on-backend`):
  - `local` (default): a journal with an `flock` commit point. All writers must be on one host, but any
    number of processes on that host may share it.
  - `backend`: a create-only log under `refs/` on the pack backend (`refs/log/<seq>`, plus
    `refs/snap/<seq>-<ts>` snapshots). Commits are linearized by atomic conditional create (S3 `If-None-Match`),
    so **several hosts may write one repository** when packs and refs are on shared storage. Log entries
    are trimmed only behind snapshots older than 1 h. A writer that paused longer than 60 s between
    catching up and committing re-checks, and fails rather than acknowledging a trimmed slot.
- **Packs can live on S3** (`open_with_backend`). Pack and index blobs are create-only and
  content-named, so they're safe even if several hosts write the same bucket. But only one host may own
  the refs (see above), and staging stays local to each writer.
- Both ref engines share the pure decision logic (`RefState::decide_*`). Both are checked against the
  model proptest; their decision logic is the extracted kernel (`verify/core`).

## Backup, restore and integrity

- **Consistent backup:** copy `refs/` first, then `objects/`. Objects are immutable and the refs only point at
  flushed objects, so a backup taken in this order is consistent even while writers run. Objects written
  after the refs copy are just unreferenced. Pause GC for the duration, because GC is what deletes packs.
- **Restore:** put both directories back and run `o5s fsck`.
- **Integrity:** `o5s fsck` reads and verifies every object and ref. Every normal read also verifies
  checksums and content hashes, so corruption is always reported. Whether it can be *returned as data first*
  depends on the read verification mode, described next.

### Read verification modes (`[read]` in config.toml, `ReadVerify` in the API, `get --read-verify`)

| mode | checks | guarantee |
|---|---|---|
| `strict` (**default**) | every record's CRC32; every chunk's BLAKE3 **before** any of its bytes are returned; for whole reads, the whole-object BLAKE3 at EOF | A reader never receives a byte that doesn't match its content hash. On corruption, the read fails before the first bad byte. |
| `eof` (opt-in) | every record's CRC32; on **whole** reads of chunked objects, the per-chunk BLAKE3 is skipped and the whole-object BLAKE3 is checked at EOF; ranged reads stay strict | Accidental corruption (bit rot, torn writes) is still stopped by the CRC32 before its bytes are returned. **CRC-valid crafted bytes can reach the caller before the error at EOF.** The read always ends in an error, so a caller that discards the data on error never keeps it. Use it for throughput when the backend is trusted. |

- **Policy.** `[read] verify` sets the mode of reads that don't choose one (`strict` unless configured).
  `[read] allow_eof = true` lets a single request ask for `eof`; `verify = "eof"` implies it. A request for `eof` on
  a repository that doesn't allow it is refused (`Unsupported`), never silently downgraded. That is how the server
  stays strict by default.
- **Scope.** Modes apply per repository and per request. Reads are addressed by content hash, not by name, so there
  is no per-namespace setting.
- **Objects stored whole** (a blob, a frame member) are verified by one hash in both modes, so `eof` changes
  nothing for them.
