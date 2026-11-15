//! Conformance against Hugging Face's published reference chunk list
//! (dataset `xet-team/xet-spec-reference-files`). Download to
//! `$P2P_XET_REF` (default `/mnt/scratch/pico2peta/bench-data/xet-ref`):
//!   Electric_Vehicle_Population_Data_20250917.csv and its `.chunks` file.
//! Skipped (with a notice) if the files are absent.

use pico2peta_chunk::{Chunker, XetChunker, chunk_lengths};

#[test]
fn matches_published_xet_chunk_boundaries() {
    let dir = std::env::var("P2P_XET_REF").unwrap_or_else(|_| "/mnt/scratch/pico2peta/bench-data/xet-ref".into());
    let base = std::path::Path::new(&dir).join("Electric_Vehicle_Population_Data_20250917.csv");
    let (Ok(data), Ok(list)) = (std::fs::read(&base), std::fs::read_to_string(base.with_extension("csv.chunks")))
    else {
        eprintln!("skipping: Xet reference files not found in {dir}");
        return;
    };
    let want: Vec<usize> = list
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().nth(1).expect("hash length").parse().expect("length"))
        .collect();
    assert_eq!(want.iter().sum::<usize>(), data.len(), "reference list covers the file");
    let got = chunk_lengths(&mut XetChunker::new(), &data);
    assert_eq!(got.len(), want.len(), "chunk count");
    assert_eq!(got, want, "chunk boundaries");
    // Streaming in odd-sized pieces gives the same boundaries.
    let mut c = XetChunker::new();
    let mut out = Vec::new();
    for p in data.chunks(999_983) {
        c.update(p, &mut out);
    }
    c.finish(&mut out);
    assert_eq!(out, want);
}
