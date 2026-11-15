//! On-disk formats: pack records, pack index, chunk manifests.
//!
//! Record (little endian):
//! ```text
//! magic  u32  = "P2PR"
//! kind   u8   0 = blob, 1 = manifest
//! codec  u8   0 = raw,  1 = zstd
//! rsvd   u16  = 0
//! hash   [32] content hash of the *logical* object
//! raw    u64  logical length (blob: bytes; manifest: total object length)
//! slen   u32  stored payload length
//! payload [slen]
//! crc32  u32  over all preceding bytes of the record
//! ```

use pico2peta_core::{Error, Hash, Result};

pub const PACK_MAGIC: &[u8; 8] = b"P2PPACK1";
pub const IDX_MAGIC: &[u8; 8] = b"P2PIDX01";
pub const REC_MAGIC: u32 = u32::from_le_bytes(*b"P2PR");
pub const REC_HEADER: usize = 4 + 1 + 1 + 2 + 32 + 8 + 4;
pub const REC_TRAILER: usize = 4;
pub const IDX_ENTRY: usize = 32 + 8 + 4 + 8 + 1 + 1;
/// Upper bound on a blob record's logical length (blobs are below the
/// chunk threshold, which `PackConfig::validate` caps at 2 GiB).
pub const MAX_BLOB: u64 = 1 << 31;
/// Upper bound on a stored payload. Rejects hostile lengths before allocating.
pub const MAX_STORED: u32 = (1 << 31) + (1 << 20);

// Fixed-width little-endian readers. Callers bounds-check the slice
// first, so the copies below always see exactly N bytes.
fn rd_u32(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[..4]);
    u32::from_le_bytes(a)
}
fn rd_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}
fn rd_hash(b: &[u8]) -> Hash {
    let mut a = [0u8; 32];
    a.copy_from_slice(&b[..32]);
    Hash(a)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Blob = 0,
    Manifest = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Raw = 0,
    Zstd = 1,
}

impl Kind {
    fn from_u8(b: u8) -> Result<Kind> {
        match b {
            0 => Ok(Kind::Blob),
            1 => Ok(Kind::Manifest),
            _ => Err(Error::Corrupt(format!("bad record kind {b}"))),
        }
    }
}
impl Codec {
    fn from_u8(b: u8) -> Result<Codec> {
        match b {
            0 => Ok(Codec::Raw),
            1 => Ok(Codec::Zstd),
            _ => Err(Error::Corrupt(format!("bad codec {b}"))),
        }
    }
}

/// Parsed record header (payload excluded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecHeader {
    pub kind: Kind,
    pub codec: Codec,
    pub hash: Hash,
    pub raw_len: u64,
    pub stored_len: u32,
}

impl RecHeader {
    pub fn total_len(&self) -> u64 {
        (REC_HEADER + self.stored_len as usize + REC_TRAILER) as u64
    }
}

/// Encodes a full record.
pub fn encode_record(kind: Kind, codec: Codec, hash: &Hash, raw_len: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(REC_HEADER + payload.len() + REC_TRAILER);
    v.extend_from_slice(&REC_MAGIC.to_le_bytes());
    v.push(kind as u8);
    v.push(codec as u8);
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(hash.as_bytes());
    v.extend_from_slice(&raw_len.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    let crc = crc32fast::hash(&v);
    v.extend_from_slice(&crc.to_le_bytes());
    v
}

/// Parses a header from at least `REC_HEADER` bytes.
pub fn parse_header(b: &[u8]) -> Result<RecHeader> {
    if b.len() < REC_HEADER {
        return Err(Error::Corrupt("short record header".into()));
    }
    if rd_u32(&b[0..4]) != REC_MAGIC {
        return Err(Error::Corrupt("bad record magic".into()));
    }
    let kind = Kind::from_u8(b[4])?;
    let codec = Codec::from_u8(b[5])?;
    let hash = rd_hash(&b[8..40]);
    let raw_len = rd_u64(&b[40..48]);
    let stored_len = rd_u32(&b[48..52]);
    if stored_len > MAX_STORED || (kind == Kind::Blob && raw_len > MAX_BLOB) {
        return Err(Error::Corrupt("record length out of bounds".into()));
    }
    Ok(RecHeader { kind, codec, hash, raw_len, stored_len })
}

/// Verifies the CRC of a complete record and returns (header, payload).
pub fn decode_record(b: &[u8]) -> Result<(RecHeader, &[u8])> {
    let h = parse_header(b)?;
    let total = h.total_len() as usize;
    if b.len() < total {
        return Err(Error::Corrupt("short record".into()));
    }
    let body = &b[..total - REC_TRAILER];
    let crc = rd_u32(&b[total - REC_TRAILER..total]);
    if crc32fast::hash(body) != crc {
        return Err(Error::Corrupt(format!("record crc mismatch for {}", h.hash)));
    }
    Ok((h, &body[REC_HEADER..]))
}

/// Decompresses a payload according to its codec.
pub fn unpack_payload(h: &RecHeader, payload: &[u8]) -> Result<Vec<u8>> {
    let zerr = |e: std::io::Error| Error::Corrupt(format!("zstd: {e}"));
    let out = match (h.codec, h.kind) {
        (Codec::Raw, _) => payload.to_vec(),
        (Codec::Zstd, Kind::Blob) => zstd::bulk::decompress(payload, h.raw_len as usize).map_err(zerr)?,
        (Codec::Zstd, Kind::Manifest) => zstd::stream::decode_all(payload).map_err(zerr)?,
    };
    if h.kind == Kind::Blob && out.len() as u64 != h.raw_len {
        return Err(Error::Corrupt(format!("length mismatch for {}", h.hash)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Manifest: list of (chunk hash, chunk length)
// ---------------------------------------------------------------------------

pub fn encode_manifest(chunks: &[(Hash, u64)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + chunks.len() * 40);
    v.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for (h, l) in chunks {
        v.extend_from_slice(h.as_bytes());
        v.extend_from_slice(&l.to_le_bytes());
    }
    v
}

pub fn decode_manifest(b: &[u8]) -> Result<Vec<(Hash, u64)>> {
    if b.len() < 4 {
        return Err(Error::Corrupt("short manifest".into()));
    }
    let n = rd_u32(&b[0..4]) as usize;
    if Some(b.len()) != n.checked_mul(40).and_then(|x| x.checked_add(4)) {
        return Err(Error::Corrupt("manifest length mismatch".into()));
    }
    Ok((0..n)
        .map(|i| {
            let o = 4 + i * 40;
            (rd_hash(&b[o..o + 32]), rd_u64(&b[o + 32..o + 40]))
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Pack index (sorted, fixed-size entries, BLAKE3 trailer)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdxEntry {
    pub hash: Hash,
    pub offset: u64,
    pub stored_len: u32,
    pub raw_len: u64,
    pub kind: Kind,
    pub codec: Codec,
}

pub fn encode_index(entries: &mut [IdxEntry]) -> Vec<u8> {
    entries.sort_by_key(|e| e.hash);
    let mut v = Vec::with_capacity(16 + entries.len() * IDX_ENTRY + 32);
    v.extend_from_slice(IDX_MAGIC);
    v.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for e in entries.iter() {
        v.extend_from_slice(e.hash.as_bytes());
        v.extend_from_slice(&e.offset.to_le_bytes());
        v.extend_from_slice(&e.stored_len.to_le_bytes());
        v.extend_from_slice(&e.raw_len.to_le_bytes());
        v.push(e.kind as u8);
        v.push(e.codec as u8);
    }
    let sum = blake3::hash(&v);
    v.extend_from_slice(sum.as_bytes());
    v
}

pub fn decode_index(b: &[u8]) -> Result<Vec<IdxEntry>> {
    if b.len() < 16 + 32 || &b[..8] != IDX_MAGIC {
        return Err(Error::Corrupt("bad index header".into()));
    }
    let (body, sum) = b.split_at(b.len() - 32);
    if blake3::hash(body).as_bytes() != sum {
        return Err(Error::Corrupt("index checksum mismatch".into()));
    }
    let n = rd_u64(&body[8..16]);
    let expect = usize::try_from(n).ok().and_then(|n| n.checked_mul(IDX_ENTRY)).and_then(|x| x.checked_add(16));
    let n = n as usize;
    if expect != Some(body.len()) {
        return Err(Error::Corrupt("index length mismatch".into()));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &body[16 + i * IDX_ENTRY..16 + (i + 1) * IDX_ENTRY];
        out.push(IdxEntry {
            hash: rd_hash(&e[0..32]),
            offset: rd_u64(&e[32..40]),
            stored_len: rd_u32(&e[40..44]),
            raw_len: rd_u64(&e[44..52]),
            kind: Kind::from_u8(e[52])?,
            codec: Codec::from_u8(e[53])?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_crc() {
        let h = Hash::of(b"x");
        let r = encode_record(Kind::Blob, Codec::Raw, &h, 1, b"x");
        let (hd, p) = decode_record(&r).unwrap();
        assert_eq!(hd.hash, h);
        assert_eq!(p, b"x");
        let mut bad = r.clone();
        bad[REC_HEADER] ^= 1;
        assert!(decode_record(&bad).is_err());
    }

    #[test]
    fn index_roundtrip() {
        let mut es: Vec<IdxEntry> = (0..10u8)
            .map(|i| IdxEntry {
                hash: Hash::of(&[i]),
                offset: i as u64 * 100,
                stored_len: 7,
                raw_len: 9,
                kind: Kind::Blob,
                codec: Codec::Zstd,
            })
            .collect();
        let b = encode_index(&mut es);
        assert_eq!(decode_index(&b).unwrap(), es);
        let mut bad = b.clone();
        bad[20] ^= 1;
        assert!(decode_index(&bad).is_err());
    }

    #[test]
    fn manifest_roundtrip() {
        let m = vec![(Hash::of(b"a"), 5), (Hash::of(b"b"), 7)];
        assert_eq!(decode_manifest(&encode_manifest(&m)).unwrap(), m);
    }
}
