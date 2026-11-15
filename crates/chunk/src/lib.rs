//! Content-defined chunkers behind one streaming trait.
//!
//! * [`XetChunker`]: byte-exact Hugging Face Xet chunking
//!   (<https://huggingface.co/docs/xet/chunking>). It is a Gear hash `h = (h << 1) + TABLE[b]`,
//!   with min 8 KiB, target 64 KiB, max 128 KiB, `MASK = 0xFFFF_0000_0000_0000`, the
//!   rust-gearhash table, and min-size skip-ahead. Matching boundaries give chunk-level dedup
//!   compatibility with HF-hosted content. Conformance is tested against the published reference
//!   chunk list (`tests/xet_reference.rs`).
//! * [`FastCdcChunker`]: FastCDC v2020 (normalization level 1), using the `fastcdc` crate's own
//!   cut function. Its boundaries are identical to `fastcdc::v2020::FastCDC` /
//!   `StreamCDC`, which the pack store uses today, so it is a drop-in.
//!
//! Both are *streaming*. Feed data in pieces of any size with [`Chunker::update`] and the
//! boundaries are the same as for one contiguous buffer. Chunkers report chunk **lengths**; the
//! caller keeps the bytes. Neither is wired into the pack store. That is a per-namespace option
//! for the core (changing the chunker changes chunk identity, not object identity).

mod xet_table;

/// A streaming content-defined chunker.
pub trait Chunker: Send {
    /// Consumes `data` (the next bytes of the stream). For every chunk that becomes complete,
    /// appends its length to `out`.
    fn update(&mut self, data: &[u8], out: &mut Vec<usize>);
    /// Ends the stream: appends the remaining chunk lengths (if any) and resets for reuse.
    fn finish(&mut self, out: &mut Vec<usize>);
    /// Short name, for logs and benchmarks.
    fn name(&self) -> &'static str;
}

/// Chunks a whole buffer; returns the chunk lengths.
pub fn chunk_lengths(c: &mut dyn Chunker, data: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    c.update(data, &mut out);
    c.finish(&mut out);
    out
}

// ---------------------------------------------------------------------------
// Xet
// ---------------------------------------------------------------------------

/// Xet chunk-size parameters (fixed by the spec).
pub const XET_MIN: usize = 8 * 1024;
pub const XET_TARGET: usize = 64 * 1024;
pub const XET_MAX: usize = 128 * 1024;
pub const XET_MASK: u64 = 0xFFFF_0000_0000_0000;
/// The Gear hash depends only on the last 64 bytes, so a chunk's first
/// `XET_MIN - 64 - 1` bytes need not be hashed (spec: "min-size skip-ahead").
const XET_SKIP: usize = XET_MIN - 64 - 1;

/// Byte-exact Xet chunker. Streaming state: the hash and the current chunk length.
#[derive(Clone, Debug, Default)]
pub struct XetChunker {
    h: u64,
    /// Bytes in the current (unfinished) chunk.
    size: usize,
}

impl XetChunker {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Chunker for XetChunker {
    fn update(&mut self, mut data: &[u8], out: &mut Vec<usize>) {
        let table = &xet_table::TABLE;
        while !data.is_empty() {
            // Skip-ahead: bytes before XET_SKIP never influence a testable hash.
            if self.size < XET_SKIP {
                let n = (XET_SKIP - self.size).min(data.len());
                self.size += n;
                data = &data[n..];
                continue;
            }
            // Hash up to the max-size boundary. Bytes before size == XET_MIN are hashed
            // without testing; after that only the mask is tested (max is the loop bound).
            let limit = (XET_MAX - self.size).min(data.len());
            let mut h = self.h;
            let untested = (XET_MIN - 1).saturating_sub(self.size).min(limit);
            for &b in &data[..untested] {
                h = (h << 1).wrapping_add(table[b as usize]);
            }
            let mut cut = None;
            for (k, &b) in data[untested..limit].iter().enumerate() {
                h = (h << 1).wrapping_add(table[b as usize]);
                if h & XET_MASK == 0 {
                    cut = Some(untested + k + 1);
                    break;
                }
            }
            if cut.is_none() && self.size + limit == XET_MAX {
                cut = Some(limit); // forced boundary at max size
            }
            match cut {
                Some(n) => {
                    out.push(self.size + n);
                    self.size = 0;
                    self.h = 0;
                    data = &data[n..];
                }
                None => {
                    self.h = h;
                    self.size += limit;
                    data = &data[limit..];
                }
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<usize>) {
        if self.size > 0 {
            out.push(self.size);
        }
        *self = Self::default();
    }

    fn name(&self) -> &'static str {
        "xet"
    }
}

// ---------------------------------------------------------------------------
// FastCDC (v2020, as used by the pack store)
// ---------------------------------------------------------------------------

/// FastCDC v2020, normalization level 1, original GEAR table (the `fastcdc` crate's defaults).
#[derive(Clone, Debug)]
pub struct FastCdcChunker {
    min: usize,
    avg: usize,
    max: usize,
    mask_s: u64,
    mask_l: u64,
    /// Pending bytes of the current chunk (at most `max`; FastCDC needs lookahead).
    buf: Vec<u8>,
}

/// Invalid chunker parameters.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidParams(pub String);

impl std::fmt::Display for InvalidParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidParams {}

impl FastCdcChunker {
    /// `min/avg/max` within the `fastcdc::v2020` limits, with `min <= avg <= max`.
    pub fn new(min: u32, avg: u32, max: u32) -> Result<Self, InvalidParams> {
        use fastcdc::v2020 as f;
        let ok = (f::MINIMUM_MIN..=f::MINIMUM_MAX).contains(&min)
            && (f::AVERAGE_MIN..=f::AVERAGE_MAX).contains(&avg)
            && (f::MAXIMUM_MIN..=f::MAXIMUM_MAX).contains(&max)
            && min <= avg
            && avg <= max;
        if !ok {
            return Err(InvalidParams(format!("FastCDC sizes {min}/{avg}/{max} out of range")));
        }
        let bits = f::logarithm2(avg) as usize;
        Ok(FastCdcChunker {
            min: min as usize,
            avg: avg as usize,
            max: max as usize,
            mask_s: f::MASKS[bits + 1],
            mask_l: f::MASKS[bits - 1],
            buf: Vec::with_capacity(max as usize),
        })
    }

    /// The pack store's default parameters (16 / 64 / 256 KiB).
    pub fn pack_default() -> Self {
        // Constant, valid parameters.
        match Self::new(16 << 10, 64 << 10, 256 << 10) {
            Ok(c) => c,
            Err(e) => unreachable!("{e}"),
        }
    }

    fn cut(&self, src: &[u8]) -> usize {
        let (_, n) = fastcdc::v2020::cut(
            src,
            self.min,
            self.avg,
            self.max,
            self.mask_s,
            self.mask_l,
            self.mask_s << 1,
            self.mask_l << 1,
        );
        n
    }
}

impl Chunker for FastCdcChunker {
    fn update(&mut self, mut data: &[u8], out: &mut Vec<usize>) {
        // Fast path: with nothing pending, cut directly in the caller's buffer.
        while self.buf.is_empty() && data.len() >= self.max {
            let n = self.cut(data);
            out.push(n);
            data = &data[n..];
        }
        while !data.is_empty() {
            let take = (self.max - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() < self.max {
                return; // need more data before a cut is final
            }
            let n = self.cut(&self.buf);
            out.push(n);
            self.buf.drain(..n);
            while self.buf.is_empty() && data.len() >= self.max {
                let n = self.cut(data);
                out.push(n);
                data = &data[n..];
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<usize>) {
        while !self.buf.is_empty() {
            let n = self.cut(&self.buf);
            let n = if n == 0 { self.buf.len() } else { n };
            out.push(n);
            self.buf.drain(..n);
        }
    }

    fn name(&self) -> &'static str {
        "fastcdc"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Straight transcription of the spec's pseudocode (no skip-ahead), as an oracle.
    fn xet_oracle(d: &[u8]) -> Vec<usize> {
        if d.len() < XET_MIN {
            return if d.is_empty() { vec![] } else { vec![d.len()] };
        }
        let (mut h, mut start, mut out) = (0u64, 0usize, Vec::new());
        for (i, &b) in d.iter().enumerate() {
            h = (h << 1).wrapping_add(xet_table::TABLE[b as usize]);
            let size = i + 1 - start;
            if size < XET_MIN {
                continue;
            }
            if size >= XET_MAX || h & XET_MASK == 0 {
                out.push(i + 1 - start);
                start = i + 1;
                h = 0;
            }
        }
        if start < d.len() {
            out.push(d.len() - start);
        }
        out
    }

    #[test]
    fn xet_matches_spec_pseudocode() {
        for (n, seed) in [(0, 1), (100, 2), (XET_MIN - 1, 3), (XET_MIN, 4), (3 << 20, 5), (1 << 20, 6)] {
            let d = data(n, seed);
            assert_eq!(chunk_lengths(&mut XetChunker::new(), &d), xet_oracle(&d), "n={n}");
        }
        // Pathological input: all zeroes forces max-size cuts.
        let z = vec![0u8; 1 << 20];
        let got = chunk_lengths(&mut XetChunker::new(), &z);
        assert_eq!(got, xet_oracle(&z));
        assert!(got[..got.len() - 1].iter().all(|&l| l == XET_MAX || l >= XET_MIN));
    }

    #[test]
    fn fastcdc_matches_the_crate() {
        for (n, seed) in [(0, 1), (1000, 2), (16 << 10, 3), (5 << 20, 4)] {
            let d = data(n, seed);
            let want: Vec<usize> =
                fastcdc::v2020::FastCDC::new(&d, 16 << 10, 64 << 10, 256 << 10).map(|c| c.length).collect();
            assert_eq!(chunk_lengths(&mut FastCdcChunker::pack_default(), &d), want, "n={n}");
        }
        assert!(FastCdcChunker::new(10, 64 << 10, 256 << 10).is_err());
        assert!(FastCdcChunker::new(128 << 10, 64 << 10, 256 << 10).is_err());
    }

    #[test]
    fn streaming_is_split_invariant() {
        let d = data(4 << 20, 9);
        fn xet() -> Box<dyn Chunker> {
            Box::new(XetChunker::new())
        }
        fn fcdc() -> Box<dyn Chunker> {
            Box::new(FastCdcChunker::pack_default())
        }
        for mk in [xet as fn() -> Box<dyn Chunker>, fcdc] {
            let whole = chunk_lengths(&mut *mk(), &d);
            for piece in [1usize, 7, 4096, 65_537, 300_000] {
                let mut c = mk();
                let mut out = Vec::new();
                for p in d.chunks(piece) {
                    c.update(p, &mut out);
                }
                c.finish(&mut out);
                assert_eq!(out, whole, "{} piece={piece}", c.name());
            }
            assert_eq!(whole.iter().sum::<usize>(), d.len());
        }
    }
}
