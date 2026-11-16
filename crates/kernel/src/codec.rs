//! On-disk decoders (docs/ARCHITECTURE.md): the pack record header.
//!
//! A record is `magic "O5SR" u32 | kind u8 | codec u8 | reserved u16 |
//! hash [32] | raw_len u64 | stored_len u32 | payload | crc u32`, little
//! endian. The header decoder checks the magic, the kind and codec codes,
//! and the length bounds before any allocation.

/// Header length in bytes.
pub const REC_HEADER: usize = 52;
/// `"O5SR"` as a little-endian u32.
pub const REC_MAGIC: u32 = 0x5253_354F;
/// Largest blob.
pub const MAX_BLOB: u64 = 1 << 31;
/// Largest stored payload.
pub const MAX_STORED: u32 = (1 << 31) + (1 << 20);
/// Largest frame (uncompressed).
pub const MAX_FRAME: u64 = 64 << 20;

/// A decoded record header (kind and codec as their on-disk codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub codec: u8,
    pub hash: [u8; 32],
    pub raw_len: u64,
    pub stored_len: u32,
}

/// Why a header is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer than [`REC_HEADER`] bytes.
    Short,
    /// Not `"O5SR"`.
    BadMagic,
    /// A kind code above 5.
    BadKind,
    /// A codec code above 2.
    BadCodec,
    /// A length over its bound.
    TooLong,
}

/// The little-endian u32 at `b[at..at + 4]` (the caller checks the range).
pub fn le32(b: &[u8], at: usize) -> u32 {
    // Arithmetic rather than shifts and ors: the same value (at most 2^32 - 1, no overflow).
    (b[at] as u32) + (b[at + 1] as u32) * 256 + (b[at + 2] as u32) * 65536 + (b[at + 3] as u32) * 16777216
}

/// The little-endian u64 at `b[at..at + 8]` (the caller checks the range).
pub fn le64(b: &[u8], at: usize) -> u64 {
    (le32(b, at) as u64) + (le32(b, at + 4) as u64) * 4294967296
}

/// Whether a kind code is a frame (Frame, Frame2, Frame3).
pub fn is_frame(kind: u8) -> bool {
    kind == 2 || kind == 3 || kind == 5
}

/// The 32 hash bytes at `b[8..40]` (the caller checks the range).
pub fn hash_at(b: &[u8]) -> [u8; 32] {
    let mut h = [0u8; 32];
    let mut i: usize = 0;
    while i < 32 {
        h[i] = b[8 + i];
        i += 1;
    }
    h
}

/// Whether a length is over its bound for this kind.
pub fn too_long(kind: u8, raw_len: u64, stored_len: u32) -> bool {
    if stored_len > MAX_STORED {
        return true;
    }
    if kind == 0 {
        return raw_len > MAX_BLOB;
    }
    if is_frame(kind) {
        return raw_len > MAX_FRAME;
    }
    false
}

/// Decodes a record header.
pub fn parse_header(b: &[u8]) -> Result<Header, HeaderError> {
    if b.len() < REC_HEADER {
        return Err(HeaderError::Short);
    }
    if le32(b, 0) != REC_MAGIC {
        return Err(HeaderError::BadMagic);
    }
    let kind = b[4];
    let codec_code = b[5];
    if kind > 5 {
        return Err(HeaderError::BadKind);
    }
    if codec_code > 2 {
        return Err(HeaderError::BadCodec);
    }
    let raw_len = le64(b, 40);
    let stored_len = le32(b, 48);
    if too_long(kind, raw_len, stored_len) {
        return Err(HeaderError::TooLong);
    }
    Ok(Header { kind, codec: codec_code, hash: hash_at(b), raw_len, stored_len })
}

/// What the ref-journal reader does at one record boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalStep {
    /// A torn tail (a crash mid-append, or a zero-filled tail): stop, keep what was read.
    Torn,
    /// The length is over the bound: corruption.
    TooLong,
    /// A CRC-invalid record followed by more data: corruption, never truncated.
    Checksum,
    /// A valid record of `end` bytes (framing included).
    Record(usize),
}

/// The ref-journal framing decision at one boundary: `rest_len` bytes remain,
/// `all_zero` if they are all zero, the record header claims `len` payload
/// bytes, and `crc_ok` is whether the payload's CRC matches (computed by the
/// caller when the record is complete; ignored otherwise).
pub fn journal_step(rest_len: usize, all_zero: bool, len: u32, max_record: u32, crc_ok: bool) -> JournalStep {
    if rest_len < 8 || all_zero {
        return JournalStep::Torn;
    }
    if len > max_record {
        return JournalStep::TooLong;
    }
    let need = len as usize;
    if need > rest_len - 8 {
        return JournalStep::Torn;
    }
    let end = 8 + need;
    if !crc_ok {
        if end == rest_len {
            return JournalStep::Torn;
        }
        return JournalStep::Checksum;
    }
    JournalStep::Record(end)
}
