# pico2peta

A repository for **immutable, content-addressed data**, from tiny objects to very large ones, where **names move according to per-namespace rules**.

- **Objects** are identified by BLAKE3-256. Small objects are bundled into append-only packs. Large objects are split with FastCDC (deduplicated chunks plus a manifest). Every read is verified.
- **Names** (refs) move only through optimistic compare-and-swap over the read set the writer depended on: `replace(name, read_set, new_hash) → ok | conflict`. Each namespace has a rule: `mutable`, `immutable` (create-once), or `head-release` (`heads/*` mutable, `releases/*` create-once).
- **Storage** is local disk or any S3-compatible store (`crates/backend-s3`). Many processes can write one repository at once. Crash recovery is tested by injecting a fault at every write and fsync point.
- **GC and repack** run in phases with grace periods, are safe while writers are active, and are crash-tested at every fault point. A *grouped* repack compresses similar objects together in frames (Experiment 1's V2 layout). On a real Git import it comes within 1.02× of `git gc --aggressive` on payload.
- **Model weights:** `put --delta-base` / `--delta-from-ref` stores a new checkpoint as an XOR delta against the previous one (ZipLLM-style), −65% on a synthetic full fine-tune.
- **Provenance:** record what was made from what, then query lineage and impact. GC keeps the inputs of derivations alive, while their outputs can be dropped and regenerated.
- **Scale:** large pack indexes are memory-mapped, plus a multi-pack index. Large objects use a manifest tree with bounded fan-out, and every read streams with bounded memory.
- **Verification:** a Lean 4 model with machine-checked proofs (content immutability, no lost updates, immutable names never move, authorization, GC safety) and a Rust↔Lean differential test.
- **Server** (`crates/server`): HTTP API with bearer-token auth, per-namespace policies, an audit log, and Prometheus metrics. The CLI talks to a local repo or a server.

## Quick start

```sh
cargo build --release
alias p2p=target/release/pico2peta

p2p -C /data/repo init
H=$(p2p -C /data/repo put model.safetensors)            # streams; prints the hash
p2p -C /data/repo replace models/heads/main $H --expect absent
p2p -C /data/repo resolve models/heads/main             # "<hash> <version>"
p2p -C /data/repo get $H --range 0..1024 > head.bin

p2p -C /data/repo ns set pkg immutable                  # published names never move
p2p -C /data/repo ns set models head-release
p2p -C /data/repo gc                                    # from cron; default grace is 1h
p2p -C /data/repo put --delta-from-ref models/heads/main ckpt-v2.safetensors
p2p -C /data/repo delete models/heads/old --expect 3     # OCC delete (rules apply)
p2p -C /data/repo prov record train --in data=$D --out model=$M
p2p -C /data/repo prov why $M                           # lineage

# Against a server:
export P2P_REMOTE=https://p2p.example.com P2P_TOKEN=p2p_...
p2p put file.bin
```

CLI exit codes: 0 ok, 1 error, 2 not found/absent, 3 replace conflict, 4 forbidden by namespace rule, 5 unauthorized.

## Layout

| crate | role |
|---|---|
| `pico2peta-core` | `Hash`, `Error`, the `ObjectStore`/`Repository`/`Backend` traits, LocalDisk, the backend conformance suite, fault injection |
| `pico2peta-pack` | pack store: packs, index, bundling, chunking, recovery |
| `pico2peta-refs` | journaled ref store, OCC, namespace rules |
| `pico2peta-repo` | `LocalRepo` (= packs + refs), GC/repack (grouped) |
| `pico2peta-prov` | provenance records, lineage/impact, regeneration-aware GC tracer |
| `pico2peta-cli` | `pico2peta` binary |
| `pico2peta-backend-s3`, `-server`, `-client` | S3 backend, HTTP server, remote client |
| `bench/exp1` | Experiment 1: grouped compression vs `git gc --aggressive` |
| `fuzz/` | cargo-fuzz targets for every decoder |

Design: `docs/DESIGN_BRIEF.md` and `docs/PLAN.md`. Status: `docs/PROGRESS.md` and `docs/NET_PROGRESS.md`.
Measurements: `docs/EXP1.md` (vs Git) and `docs/BENCH.md` (throughput, model weights). HTTP API: `docs/API.md`.
