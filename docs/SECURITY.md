# Security review: core (ROADMAP item 6)

Scope: the storage core, i.e. `crates/{core,pack,refs,repo,prov,attest,order}`.
The server and fronts are reviewed separately (ROADMAP item 6 of the net list,
docs/NET_PROGRESS.md). The threat model has two parts:
- **Untrusted object bytes:** an attacker can write arbitrary objects through
  any front, and may hand-edit or corrupt pack files on a backend.
- **Untrusted requests:** an attacker can issue arbitrary reads and queries.

Operators and their config files are trusted. Even so, config parsing is
fuzzed so that it never panics.

## Decoder limits (every bound is checked before allocating)

| input | bound | where |
|---|---|---|
| blob / stored record | `MAX_BLOB` 2 GiB, `MAX_STORED` 2 GiB + 1 MiB | `pack/src/format.rs` |
| frame (Frame/Frame2/Frame3) | `MAX_FRAME` 64 MiB, `MAX_FRAME_MEMBERS` 2^20 | `format.rs` |
| manifest, zstd-compressed | at most `MAX_MANIFEST_BYTES` (2^20 entries) decompressed; the decode stops at limit+1, so zip bombs fail | `format.rs::decode_bounded` |
| delta record, zstd-compressed | at most `MAX_DELTA_BYTES` (`MAX_DELTA_SEGS` 2^20 segments) | `format.rs::decode_bounded` |
| delta chain depth | 32 (`MAX_DELTA_DEPTH`) | `pack/src/store.rs` |
| Frame3 reference chain | 16 (`MAX_REF_DEPTH`); the writer produces at most 2, and a cycle stops at the cap | `store.rs` |
| manifest tree depth | 16 (`depth_limit`), streaming; memory is depth × fanout plus the read-ahead window | `store.rs` |
| ref journal record | 16 MiB (`MAX_RECORD`); batch size 10,000 (`MAX_BATCH`) | `refs/src/lib.rs` |
| provenance record field / items | 1 MiB / 2^16 per call | `prov/src/lib.rs` |
| provenance checkpoint index | length must equal 24 + 64·(n_fwd+n_bwd), with checked arithmetic | `prov/src/lib.rs::read_index` |
| DSSE envelope | 1 MiB (`MAX_ENVELOPE`); the object size is checked before it is read, and promote skips oversized or invalid envelopes instead of failing | `attest/src/lib.rs` |
| pack index / MIDX | length and fanout checks before use | `pack/src/format.rs` |

Remaining by design:
- `get` of one blob allocates up to its size (≤ 2 GiB). Streaming readers
  (`open`, HTTP ranges) do not.
- A deployment that must cap memory per request should use `open` and put a
  body-size limit in front (the server does).

## Path handling

- Backend keys are validated before any path is formed. The following are all
  refused (tested in `core/src/backend.rs`):
  - empty components, `..`, a leading or trailing `/`;
  - whitespace and backslashes.
- Ref names follow the rule grammar in `core` and are fuzzed (`names`).
- Temporary files use per-process, per-sequence nonce names (`.tmp-<pid>-<seq>-<nonce>`) in the target directory and are renamed into place.
- Fuzz and test helpers write only under `TMPDIR`.

## Resource exhaustion

- **Similarity repack:** frames are capped at 6 MiB and reference depth at 2.
  Each LSH query looks at no more than `BUCKET_NEAREST` (256) candidates per
  bucket, so a crafted corpus of identical sketches costs linear work, not
  quadratic. Bucket memory is linear in the number of objects.
- **Delta fan-out:** each level of a delta chain touches only the base
  segments that overlap the requested output range. A read is therefore
  O(depth × segments), not exponential.
- **Search index:** memory grows with the provenance log, and refresh is
  incremental from the last indexed head. Cycles in the log stop on a
  seen-set.

## Races

- **GC vs. put:** writers that link existing objects call `ensure_live`, not
  `contains`. A condemned-only copy is then revived instead of being
  collected under the writer. This covers provenance, attestations, promote
  and Frame3 references.
- **Cached pack handles:** the LocalDisk fd cache checks `nlink == 0` on every
  hit, so a pack that GC deleted is never trusted
  (`cached_pack_handles_see_external_deletes`).
- **Ref journal group commit:** the leader holds the flock through fsync. On
  a failed batch, the leader releases the batch lock before reloading. This
  fixes a self-deadlock, and the tests have a 120 s deadline guard.
- **Promote:** OCC over both `src` and `dst` (`update_many`). A concurrent
  move of either one makes the promote a no-op, never a wrong move (Lean P7,
  `updateMany_stale_noop`).

- **Concurrent `put_many` / `put` of the same object:** the dedup check and the append are not atomic
  together, so two writers may both append a record for the same hash. That is harmless. Both records hold
  the same content, and the index keeps one location (`insert_loc` ranks them). GC copies only one.
- **Hard-linked index cache vs GC:**
  - On local disk, `idx-cache/<pack>.idx3` is a hard link to `packs/<pack>.idx`.
  - When GC deletes the pack, the link keeps the inode until the next refresh, which removes cache entries
    of vanished packs. Mapped views stay valid meanwhile; the cost is only that the disk space is freed late.
  - Pack indexes are content-named and create-only, so a re-created pack has identical index bytes.
  - Every mapping is checksum-verified.

## Findings fixed in this review

1. Zstd-compressed manifest and delta records were decompressed without a
   size bound (zip bomb). They are now bounded (`decode_bounded`).
2. Attestation envelopes were read in full before the size check. The size is
   now checked first, and promote skips bad envelopes.
3. The fd cache trusted packs that had been deleted. Fixed with the nlink
   check (found by net's HF test).
4. Group-commit self-deadlock on the failure path (see Races).

## Fuzz targets

Every core format has a cargo-fuzz target (see fuzz/README.md). The targets
added in this review:
- `frames`
- `attestations`
- `provenance`
- `delta_config`
- `search_index`

Run results are recorded in PROGRESS.md.
