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
    if u32::from_le_bytes(b[0..4].try_into().unwrap()) != REC_MAGIC {
        return Err(Error::Corrupt("bad record magic".into()));
    }
    let kind = Kind::from_u8(b[4])?;
    let codec = Codec::from_u8(b[5])?;
    let hash = Hash(b[8..40].try_into().unwrap());
    let raw_len = u64::from_le_bytes(b[40..48].try_into().unwrap());
    let stored_len = u32::from_le_bytes(b[48..52].try_into().unwrap());
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
    let crc = u32::from_le_bytes(b[total - REC_TRAILER..total].try_into().unwrap());
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
    let n = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
    if b.len() != 4 + n * 40 {
        return Err(Error::Corrupt("manifest length mismatch".into()));
    }
    Ok((0..n)
        .map(|i| {
            let o = 4 + i * 40;
            (Hash(b[o..o + 32].try_into().unwrap()), u64::from_le_bytes(b[o + 32..o + 40].try_into().unwrap()))
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
    let n = u64::from_le_bytes(body[8..16].try_into().unwrap()) as usize;
    if body.len() != 16 + n * IDX_ENTRY {
        return Err(Error::Corrupt("index length mismatch".into()));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &body[16 + i * IDX_ENTRY..16 + (i + 1) * IDX_ENTRY];
        out.push(IdxEntry {
            hash: Hash(e[0..32].try_into().unwrap()),
            offset: u64::from_le_bytes(e[32..40].try_into().unwrap()),
            stored_len: u32::from_le_bytes(e[40..44].try_into().unwrap()),
            raw_len: u64::from_le_bytes(e[44..52].try_into().unwrap()),
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
