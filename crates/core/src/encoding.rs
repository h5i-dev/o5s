//! How one put is encoded: the domain-agnostic vocabulary of the Front API.
//!
//! The core never decides *why* an encoding is chosen. A front (see
//! `pico2peta_repo::front`) looks at the name and the content and returns an
//! [`Encoding`]. Every encoding is a storage choice only: the stored object's
//! identity is always the BLAKE3 of the bytes `get` returns, and reads never
//! depend on how the object was written.

use crate::Hash;

/// Encoding choices for one put. `Default` = the store's plain encoding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Encoding {
    /// Content-defined chunker for large objects, by id: `"fastcdc"` (the
    /// default) or `"xet"`. `None` = the store's default.
    pub chunker: Option<String>,
    /// The content is fixed-width numeric data with elements of this many
    /// bytes, so it may be byte-grouped (planes) before compression.
    pub group_width: Option<u8>,
    /// Store the object as `base + patch` instead of on its own.
    pub derive: Option<Derive>,
}

/// A derived object: `object = apply(patch_format, base, patch)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Derive {
    /// The base object (must exist).
    pub base: Hash,
    /// How the patch is expressed.
    pub patch: PatchFormat,
    /// Alignment for [`PatchFormat::Xor`]: `(dst, src, len)` maps object bytes
    /// `[dst, dst+len)` onto base bytes `[src, src+len)`. Empty = offset-aligned.
    pub segments: Vec<(u64, u64, u64)>,
}

/// Patch formats for derived objects. New formats are added here as variants;
/// each has a decoder that is fuzzed and never panics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatchFormat {
    /// Byte-wise XOR against the (aligned) base; uncovered bytes are XORed
    /// with zero. Good for same-layout numeric data (model checkpoints).
    Xor,
    /// XOR with the alignment discovered while streaming (API v1.16): the new
    /// content is cut into content-defined chunks, a chunk also present in the
    /// base anchors the alignment at its base offset, and other chunks keep the
    /// last anchor's shift. It handles scattered in-place edits (shift 0) and
    /// insertions/deletions (re-anchored right after) alike, domain-agnostic.
    /// `segments` must be empty.
    XorAnchored,
}

impl Encoding {
    /// Plain encoding with a chunker choice.
    pub fn chunker(id: impl Into<String>) -> Self {
        Encoding { chunker: Some(id.into()), ..Default::default() }
    }
}

/// How much a read verifies before returning bytes (API v1.17). Both modes
/// check every record's CRC32 and, for whole-object reads, the BLAKE3 of the
/// whole object at EOF. Corruption is always reported; the difference is *when*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadVerify {
    /// The default. Every chunk's BLAKE3 is checked before any of its bytes
    /// are returned, so a reader never receives bytes that don't match their
    /// content hash.
    #[default]
    Strict,
    /// Opt-in, for throughput. On whole-object reads the per-chunk BLAKE3 is
    /// skipped: bytes are returned once their record's CRC32 checks, and the
    /// whole-object BLAKE3 is checked at EOF. **CRC-valid crafted bytes can
    /// reach the caller before the error at EOF.** Ranged reads are always strict.
    Eof,
}

impl ReadVerify {
    /// Parses `"strict"` / `"eof"`.
    pub fn parse(s: &str) -> Option<ReadVerify> {
        match s {
            "strict" => Some(ReadVerify::Strict),
            "eof" => Some(ReadVerify::Eof),
            _ => None,
        }
    }

    /// `"strict"` / `"eof"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadVerify::Strict => "strict",
            ReadVerify::Eof => "eof",
        }
    }
}
