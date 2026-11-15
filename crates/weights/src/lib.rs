//! Model-weights kind: store a new checkpoint as an XOR delta against the
//! previous one, **aligned per tensor**. For safetensors files
//! (`u64 header length | JSON header | tensor data`), each tensor of the new
//! file is aligned with the same-named, same-size tensor of the base, so
//! reordered tensors, resized neighbours, and edited headers don't break the
//! delta. Anything without a counterpart is stored as is.
//!
//! [`put_checkpoint`] falls back to a plain (offset-aligned) delta when
//! either file isn't safetensors.
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod bytegroup;

use pico2peta_core::{Error, Hash, ObjectStore, Result};
use std::collections::HashMap;
use std::io::Read;

/// Upper bound on a safetensors JSON header (the format caps it at 100 MB).
pub const MAX_HEADER: u64 = 100 << 20;

/// A parsed safetensors header: total header bytes (8 + JSON) and tensor
/// name → absolute byte range in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Bytes before tensor data (`8 + json_len`).
    pub data_start: u64,
    /// Tensor name → `[start, end)` absolute offsets.
    pub tensors: HashMap<String, (u64, u64)>,
}

/// Parses a safetensors header from `head` (which must hold at least
/// `8 + json_len` bytes). Returns `None` if it isn't safetensors.
pub fn parse_layout(head: &[u8]) -> Option<Layout> {
    let n = u64::from_le_bytes(head.get(..8)?.try_into().ok()?);
    if n == 0 || n > MAX_HEADER {
        return None;
    }
    let json = head.get(8..8 + n as usize)?;
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let obj = v.as_object()?;
    let data_start = 8 + n;
    let mut tensors = HashMap::new();
    for (name, t) in obj {
        if name == "__metadata__" {
            continue;
        }
        let offs = t.get("data_offsets")?.as_array()?;
        let (a, b) = (offs.first()?.as_u64()?, offs.get(1)?.as_u64()?);
        if b < a {
            return None;
        }
        tensors.insert(name.clone(), (data_start.checked_add(a)?, data_start.checked_add(b)?));
    }
    Some(Layout { data_start, tensors })
}

/// Alignment segments `(dst, src, len)` mapping the new file onto the base:
/// the header onto the base header, and each tensor onto its same-named,
/// same-size counterpart. Sorted by `dst` and non-overlapping.
pub fn plan(base: &Layout, new: &Layout) -> Vec<(u64, u64, u64)> {
    let mut segs = vec![(0u64, 0u64, base.data_start.min(new.data_start))];
    for (name, &(ns, ne)) in &new.tensors {
        if let Some(&(bs, be)) = base.tensors.get(name)
            && be - bs == ne - ns
            && ne > ns
        {
            segs.push((ns, bs, ne - ns));
        }
    }
    segs.sort();
    // Drop overlaps (malformed headers with overlapping tensors): keep the first.
    let mut out: Vec<(u64, u64, u64)> = Vec::with_capacity(segs.len());
    for g in segs {
        if out.last().is_none_or(|l| l.0 + l.2 <= g.0) {
            out.push(g);
        }
    }
    out
}

/// Reads up to the safetensors header from `src` (fewer bytes if the input is
/// shorter or isn't safetensors; `put_checkpoint` then falls back).
fn read_head(src: &mut dyn Read) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    src.take(8).read_to_end(&mut head)?;
    if head.len() == 8 {
        let n = u64::from_le_bytes([head[0], head[1], head[2], head[3], head[4], head[5], head[6], head[7]]);
        if n > 0 && n <= MAX_HEADER {
            src.take(n).read_to_end(&mut head)?;
        }
    }
    Ok(head)
}

/// Stores the checkpoint read from `src` as a tensor-aligned delta against
/// `base` (falls back to an offset-aligned delta when either side isn't
/// safetensors). Returns the content hash of the new checkpoint.
pub fn put_checkpoint(store: &dyn ObjectStore, base: &Hash, src: &mut dyn Read) -> Result<Hash> {
    let head = read_head(src)?;
    let base_len = store.size(base)?;
    let bhead_len = store.get(base, Some(0..8.min(base_len)))?;
    let bn = bhead_len.get(..8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes).unwrap_or(0);
    let base_layout = if bn > 0 && bn <= MAX_HEADER && 8 + bn <= base_len {
        parse_layout(&store.get(base, Some(0..8 + bn))?)
    } else {
        None
    };
    let mut rest = std::io::Cursor::new(head.clone()).chain(src);
    match (base_layout, parse_layout(&head)) {
        (Some(b), Some(n)) => {
            // A malformed base header could point past its own end: drop such segments.
            let segs: Vec<(u64, u64, u64)> = plan(&b, &n)
                .into_iter()
                .filter(|&(_, src, len)| src.checked_add(len).is_some_and(|e| e <= base_len))
                .collect();
            store.put_delta_mapped(base, &segs, &mut rest)
        }
        _ => store.put_delta(base, &mut rest),
    }
}

/// Convenience: an error for non-safetensors input when a caller insists on it.
pub fn not_safetensors() -> Error {
    Error::Parse("not a safetensors file".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pico2peta_repo::{LocalRepo, PackConfig};

    fn safetensors(tensors: &[(&str, Vec<u8>)], meta: &str) -> Vec<u8> {
        let mut hdr = serde_json::Map::new();
        let mut off = 0u64;
        for (n, d) in tensors {
            hdr.insert(
                n.to_string(),
                serde_json::json!({"dtype": "F16", "shape": [d.len() / 2], "data_offsets": [off, off + d.len() as u64]}),
            );
            off += d.len() as u64;
        }
        hdr.insert("__metadata__".into(), serde_json::json!({ "note": meta }));
        let j = serde_json::to_vec(&serde_json::Value::Object(hdr)).unwrap();
        let mut v = (j.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(&j);
        for (_, d) in tensors {
            v.extend_from_slice(d);
        }
        v
    }

    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn disk(p: &std::path::Path) -> u64 {
        std::fs::read_dir(p).map_or(0, |rd| {
            rd.flatten()
                .map(|e| if e.path().is_dir() { disk(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
                .sum()
        })
    }

    #[test]
    fn reordered_and_resized_tensors_still_delta_well() {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
        let (a, b, c) = (noise(1, 1 << 20), noise(2, 1 << 20), noise(3, 1 << 20));
        let v1 = safetensors(&[("a", a.clone()), ("b", b.clone()), ("c", c.clone())], "v1");
        let h1 = r.put(&v1).unwrap();
        r.flush().unwrap();
        // v2: tensors reordered, a new tensor inserted first, header metadata changed,
        // and a light fine-tune of b (low bits).
        let mut b2 = b.clone();
        for x in b2.iter_mut().step_by(2) {
            *x ^= 1;
        }
        let v2 = safetensors(&[("new", noise(9, 4096)), ("c", c.clone()), ("b", b2), ("a", a.clone())], "v2, edited");
        let before = disk(&d.path().join("objects"));
        let h2 = put_checkpoint(&r, &h1, &mut &v2[..]).unwrap();
        r.flush().unwrap();
        let aligned = disk(&d.path().join("objects")) - before;
        assert_eq!(h2, Hash::of(&v2));
        assert_eq!(r.get(&h2, None).unwrap(), v2);
        // Offset-aligned delta (what plain put_delta does) in a fresh repo for comparison.
        let d2 = tempfile::tempdir().unwrap();
        let r2 = LocalRepo::init(d2.path(), &PackConfig::default()).unwrap();
        let g1 = r2.put(&v1).unwrap();
        r2.flush().unwrap();
        let before2 = disk(&d2.path().join("objects"));
        r2.put_delta(&g1, &mut &v2[..]).unwrap();
        r2.flush().unwrap();
        let naive = disk(&d2.path().join("objects")) - before2;
        assert!(aligned * 4 < naive, "tensor-aligned {aligned} B vs offset-aligned {naive} B");
        // Range reads across tensor boundaries.
        let mid = v2.len() / 2;
        assert_eq!(r.get(&h2, Some(mid as u64 - 10..mid as u64 + 10)).unwrap(), &v2[mid - 10..mid + 10]);
    }

    #[test]
    fn falls_back_for_non_safetensors() {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
        let base = noise(4, 100_000);
        let hb = r.put(&base).unwrap();
        let mut v = base.clone();
        v[10] ^= 1;
        let h = put_checkpoint(&r, &hb, &mut &v[..]).unwrap();
        assert_eq!(r.get(&h, None).unwrap(), v);
    }

    #[test]
    fn short_and_malformed_inputs_fall_back() {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
        let hb = r.put(b"base").unwrap();
        for v in [&b"tiny"[..], &b""[..], &[5, 0, 0, 0, 0, 0, 0, 0, b'{'][..]] {
            let h = put_checkpoint(&r, &hb, &mut &v[..]).unwrap();
            assert_eq!(r.get(&h, None).unwrap(), v);
        }
        // A base whose header points past its own end still works (segments are clamped).
        let mut bad = safetensors(&[("a", noise(1, 1000))], "x");
        bad.truncate(bad.len() - 500);
        let hbad = r.put(&bad).unwrap();
        let v2 = safetensors(&[("a", noise(1, 1000))], "y");
        let h2 = put_checkpoint(&r, &hbad, &mut &v2[..]).unwrap();
        assert_eq!(r.get(&h2, None).unwrap(), v2);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_layout(b"").is_none());
        assert!(parse_layout(&[255u8; 16]).is_none());
        let mut v = 5u64.to_le_bytes().to_vec();
        v.extend_from_slice(b"{bad}");
        assert!(parse_layout(&v).is_none());
    }
}
