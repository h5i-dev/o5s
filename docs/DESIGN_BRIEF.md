# pico2peta — Design Brief

Source: summary of prior design discussions (originally in Japanese; translated and condensed here).

## One-line concept

A general-purpose repository for **immutable, content-addressed data**, stored in **size-independent packs**, where **names move according to per-namespace rules**. From tiny objects (pico) to petabyte-scale (peta).

## Layers (bottom up)

1. **Physical layer:** Append-only immutable packs plus an index (hash → location within pack), on S3 or local disk. Small objects are bundled; large objects are split by content-defined chunking. Similar objects are placed adjacent and compressed together.
2. **Object layer:** Everything is identified by content hash. Directories and Snapshots are objects that reference other objects.
3. **Name layer:** Names (Refs) point to objects. Updates are compare-and-swap ("replace if still in the expected state"), but the check is against the **read set actually depended on**, not the whole state (OCC).
4. **Provenance layer:** Records what was made from what (inputs, operation, outputs). Used to scope re-verification and to decide whether something can be deleted and regenerated.
5. **Compatibility layer:** Adapters speaking Git, OCI, PyPI, etc., plus upstream-registry caching.
6. **Cross-cutting:** Authorization & audit, retention & GC, signing & promotion (dev → prod), search.

## Core API

- `put(data) → hash`
- `get(hash, range?) → data`
- `resolve(name) → hash`
- `replace(name, read_set, new_hash) → ok | conflict`

## Per-kind differences are configuration

| Setting | Source code | Packages | Model weights |
|---|---|---|---|
| Name update rule | Mutable | Immutable once published | Head mutable, releases immutable |
| Conflict resolution | 3-way merge, else regenerate | Forbidden | Reject and rebuild |
| Fetch strategy | Whole pack | Whole, upstream cache | Chunked, partial/lazy fetch |
| Retention | Full history | Published versions forever | Old versions deletable if provenance exists |

## Properties to prove in Lean 4

- Stored object contents never change.
- Conflicting updates never cause an undetected overwrite.
- Immutable names cannot be moved.
- Unauthorized principals can neither read nor move names.
- Referenced objects are never lost by GC or repacking.

Proofs cover only the extracted pure core. PostgreSQL / S3 behavior is stated as explicit assumptions.

## Performance methodology

Compare per kind against specialized systems (Git, OCI registries, Xet, …). Pass thresholds are fixed numerically **in advance**. GPU only where measurement shows compute is the bottleneck.

## Open questions

- **Compression:** How close can "place similar objects together and compress" get to Git's storage size on source code? → **This is the first experiment.**
- **Read-set granularity:** per file, or derived from the provenance graph?
- **Hash mapping:** How to map Git SHA-1 / OCI compressed-sha256 to internal hashes.
- **Index placement:** Billions of objects — PostgreSQL, or files alongside packs?

## Related work (by layer)

**Storage (bundle / split / compress)**
- Haystack (OSDI 2010) — bundling small objects; append-only log store, 32-byte in-memory index per object. Starting point for pack+index design.
- Git is for Data (CIDR 2023, XetHub) — closest prior art: Git UX at TB scale; small-chunk dedup aggregated into large blocks.
- ZipLLM (NSDI 2026) — generic chunking is suboptimal for weights; tensor-level dedup + XOR-delta vs base model; −54.1% storage over 3,048 models. Justifies "per-kind config".
- Principles of Dataset Versioning (VLDB 2015) — delta vs. full storage tradeoff (storage vs. recreation cost), mostly NP-hard; includes analysis of Git repacking.
- FastCDC (ATC 2016), VectorCDC (FAST 2025) — chunking baselines.

**Distribution**
- Slacker (FAST 2016) — lazy partial fetch.
- DupHunter (ATC 2020) — registry dedup vs. pull performance; why dedup of compressed data is hard.

**Updates & concurrency**
- Keeping Master Green at Scale (EuroSys 2019, Uber SubmitQueue) and CI at Scale: Lean, Green, and Fast (2025) — relevant to multi-agent `replace` contention.
- Kung & Robinson 1981 (OCC) — read-set conflict detection.

**Verification**
- Aeneas (ICFP 2022) — Rust → Lean translation (no interior mutability / unsafe); defines the provable-core boundary.
- How We Built Cedar (2024) — Lean model + proofs, plus differential random testing of the Rust implementation against the model. Use this approach.
- ShardStore (SOSP 2021) — executable reference models in Rust + conformance checking; crash-consistency/concurrency kept outside full proofs.

**Others (unverified citations):** Venti (FAST 2002), Nix thesis (Dolstra 2006), Build Systems à la Carte (ICFP 2018), in-toto (USENIX Security 2019).

Recommended first reads: Git is for Data, ZipLLM, Cedar.

## Naming

Working name is `pico2peta` (repo name). `divrem` was rejected (taken on crates.io, collides with arithmetic functions). Decide a final public name after benchmarks show the headline feature.
