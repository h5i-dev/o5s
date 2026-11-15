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

Run (needs nightly; on this host it lives on scratch):

```sh
export RUSTUP_HOME=/mnt/scratch/pico2peta/rustup-nightly CARGO_TARGET_DIR=/mnt/scratch/pico2peta/cargo-target-fuzz
cargo +nightly fuzz run pack_record -- -max_total_time=300
```

The same "never panics" properties also run on stable as proptests
(`crates/pack/tests/decoders.rs`, `crates/refs/tests/refs.rs`), so CI covers
them without nightly.
