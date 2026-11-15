//! Cross-domain dedup (ROADMAP P5): the same content stored through three
//! fronts' framings is stored about once.
//! - Git: the canonical loose-object form `"blob <size>\0" + data`.
//! - OCI: an uncompressed tar layer (512-byte header + data + padding).
//! - Generic/PyPI file: the raw bytes.
//!
//! FastCDC re-synchronizes within a chunk or two after the differing header,
//! so all three share every chunk but the first and last. Compressed
//! containers (deflated wheels, gzipped layers) change the bytes themselves,
//! and that would need front-side decompression (stored as an uncompressed
//! payload plus recompression parameters), so this test does not claim it.
use pico2peta_core::ObjectStore;
use pico2peta_repo::{LocalRepo, PackConfig};

fn walk(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p).map_or(0, |rd| {
        rd.flatten()
            .map(|e| if e.path().is_dir() { walk(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
            .sum()
    })
}

#[test]
fn same_content_via_git_oci_and_raw_framings_is_stored_once() {
    let mut x = 0x1234_5678_9ABC_DEF1u64;
    let data: Vec<u8> = (0..16 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let git = [format!("blob {}\0", data.len()).into_bytes(), data.clone()].concat();
    let mut tar = vec![0u8; 512];
    tar[..10].copy_from_slice(b"model.bin\0");
    tar.extend_from_slice(&data);
    tar.resize(tar.len().div_ceil(512) * 512 + 1024, 0); // padding + end-of-archive blocks
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let raw_h = r.put_stream(&mut &data[..]).unwrap();
    r.flush().unwrap();
    let one = walk(&d.path().join("objects"));
    let git_h = r.put_stream(&mut &git[..]).unwrap();
    let tar_h = r.put_stream(&mut &tar[..]).unwrap();
    r.flush().unwrap();
    let all = walk(&d.path().join("objects"));
    // Three distinct objects (identity is per framing)...
    assert!(raw_h != git_h && git_h != tar_h);
    for (h, v) in [(raw_h, &data), (git_h, &git), (tar_h, &tar)] {
        assert_eq!(&r.get(&h, None).unwrap(), v);
    }
    // ...stored about once: the two extra framings cost < 5 % of one copy.
    let extra = all - one;
    eprintln!("one copy {one} B; git + oci framings added {extra} B ({:.2} %)", 100.0 * extra as f64 / one as f64);
    assert!(extra * 20 < one, "extra {extra} vs one {one}");
}
