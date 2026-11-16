# Fuzz targets

Every on-disk decoder and every input parser has a target:

| target | covers |
|---|---|
| `pack_record` | pack record header, CRC, decompression |
| `pack_index` | `.idx` decoding |
| `manifest` | chunk manifests |
| `ref_journal` | ref journal records, plus a full `RefStore::open` on arbitrary journal bytes |
| `staging_recovery` | open-pack crash recovery on arbitrary staging bytes |
| `names` | ref names, hashes, rules, backend keys |
| `midx` | multi-pack index and mapped v3 index views |
| `git_push` | the full git receive-pack path (pkt-line commands, pack trailer, entry headers, zlib, OFS/REF/thin deltas, connectivity, ref CAS) against a temp repo |
| `git_parsers` | git object headers, tree/commit/tag parsing, delta application, upload-pack requests, pkt-lines |
| `oci_parsers` | OCI manifest validation, `/v2` routing, repo/tag/digest grammar |
| `segmap` | segment-map segment and root decoding |
| `audit_log` | audit log tail recovery and chain verification on arbitrary bytes |
| `s3_xml` | S3 XML responses (ListObjectsV2, errors, multipart) |
| `http_range` | `Range` header parsing (result must lie inside the object) |
| `frames` | Frame2/Frame3 bodies, prefix decoding and member enumeration with an attacker reference |
| `attestations` | DSSE envelopes and in-toto statements |
| `provenance` | provenance records and log nodes |
| `delta_config` | delta records (bounded decode) and repository config |
| `search_index` | search index build, text/prefix queries and lineage/impact over attacker records and checkpoint index |

Run (needs nightly; on this host it lives on scratch):

```sh
export RUSTUP_HOME=/mnt/scratch/pico2peta/rustup-nightly CARGO_TARGET_DIR=/mnt/scratch/pico2peta/cargo-target-fuzz
cargo +nightly fuzz run pack_record -- -max_total_time=300
```

The same "never panics" properties also run on stable as proptests
(`crates/pack/tests/decoders.rs`, `crates/refs/tests/refs.rs`, `crates/server/tests/never_panic.rs`, `crates/backend-s3/tests/never_panic.rs`), so CI covers
them without nightly.
