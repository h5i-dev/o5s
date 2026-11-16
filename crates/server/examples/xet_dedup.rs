//! Xet-style storage of a set of files: what HF's Xet backend would keep (PLAN §6.5 HF row).
//!
//!   cargo run --release -p o5s-server --example xet_dedup -- <file>...
//!
//! Chunks every file with the byte-exact Xet chunker, keeps each distinct chunk once (by
//! Xet chunk hash), and compresses it the way xet-core does per chunk: the smallest of none,
//! LZ4 and BG4+LZ4. Prints one JSON line. (Xorb framing and shard metadata are ignored:
//! they are under 0.1% of the payload.)

use o5s_chunk::{XetChunker, chunk_lengths};
use std::collections::HashSet;
use std::io::Write;

fn lz4(data: &[u8]) -> usize {
    let mut w = lz4_flex::frame::FrameEncoder::new(Vec::new());
    w.write_all(data).unwrap();
    w.finish().unwrap().len()
}

fn bg4_split(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    for k in 0..4 {
        out.extend(d.iter().skip(k).step_by(4));
    }
    out
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let (mut seen, mut total, mut unique, mut stored, mut chunks) = (HashSet::new(), 0u64, 0u64, 0u64, 0u64);
    for f in &files {
        let data = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let mut c = XetChunker::new();
        let mut off = 0;
        for len in chunk_lengths(&mut c, &data) {
            let chunk = &data[off..off + len];
            off += len;
            total += len as u64;
            chunks += 1;
            if seen.insert(*blake3::hash(chunk).as_bytes()) {
                unique += len as u64;
                stored += len.min(lz4(chunk)).min(lz4(&bg4_split(chunk))) as u64;
            }
        }
    }
    println!(
        "{{\"files\":{},\"bytes\":{total},\"chunks\":{chunks},\"distinct_chunks\":{},\"unique_bytes\":{unique},\"xet_stored_bytes\":{stored}}}",
        files.len(),
        seen.len()
    );
}
