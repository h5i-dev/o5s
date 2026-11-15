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

/// Pack format v1: magic, then records.
pub const PACK_MAGIC_V1: &[u8; 8] = b"P2PPACK1";
/// Pack format v2 (current): magic, a 16-byte random nonce, then records.
/// The nonce makes every pack's content (and so its content-derived name)
/// unique, even when two packs hold the same records in the same order. GC
/// relies on this: a rewritten copy must never alias a condemned pack.
pub const PACK_MAGIC: &[u8; 8] = b"P2PPACK2";
/// Length of the v2 pack header (magic + nonce).
pub const PACK_HEADER_V2: usize = 24;

/// Header length implied by a pack's first 8 bytes, or `None` if not a pack.
pub fn pack_header_len(magic: &[u8]) -> Option<u64> {
    if magic == PACK_MAGIC_V1 {
        Some(8)
    } else if magic == PACK_MAGIC {
        Some(PACK_HEADER_V2 as u64)
    } else {
        None
    }
}
/// Index format v1 magic (read-only; v2 is written).
pub const IDX_MAGIC: &[u8; 8] = b"P2PIDX01";
/// Magic at the start of every pack record.
pub const REC_MAGIC: u32 = u32::from_le_bytes(*b"P2PR");
/// Record header length in bytes.
pub const REC_HEADER: usize = 4 + 1 + 1 + 2 + 32 + 8 + 4;
/// Record trailer (CRC-32) length in bytes.
pub const REC_TRAILER: usize = 4;
/// v1 index entry length in bytes.
pub const IDX_ENTRY: usize = 32 + 8 + 4 + 8 + 1 + 1;
/// v2 entries add `frame_inner: u32` and `frame_len: u32` (0 = not framed).
pub const IDX_ENTRY_V2: usize = IDX_ENTRY + 8;
/// Index format v2 magic (read-only).
pub const IDX_MAGIC_V2: &[u8; 8] = b"P2PIDX02";
/// Index format v3 magic (current). Layout after the magic:
/// `n_records u64 | n_entries u64 | records | entries | blake3[32]`, where a
/// record is `offset u64 | stored_len u32 | raw_len u64 | kind u8 | codec u8`
/// (22 B) and an entry is `hash[32] | record u32 | inner u32 | len u32` (44 B).
/// Frame members share their frame's record. For non-frame records,
/// `inner = len = 0` and the length is the record's `raw_len`.
pub const IDX_MAGIC_V3: &[u8; 8] = b"P2PIDX03";
const IDX3_REC: usize = 22;
const IDX3_ENT: usize = 44;
/// Upper bound on a blob record's logical length (blobs are below the
/// chunk threshold, which `PackConfig::validate` caps at 2 GiB).
pub const MAX_BLOB: u64 = 1 << 31;
/// Upper bound on a stored payload. Rejects hostile lengths before allocating.
pub const MAX_STORED: u32 = (1 << 31) + (1 << 20);
/// Upper bound on a frame's uncompressed size.
pub const MAX_FRAME: u64 = 64 << 20;
/// Upper bound on a frame's member count.
pub const MAX_FRAME_MEMBERS: usize = 1 << 20;

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
/// What a pack record holds.
pub enum Kind {
    /// A whole object (or chunk), at most `MAX_BLOB` bytes.
    Blob = 0,
    /// A manifest node: the `(hash, length)` list of a large object or of a range of one.
    Manifest = 1,
    /// Several small objects compressed together. The payload is an
    /// uncompressed member directory followed by the (codec-compressed)
    /// concatenated bodies. The record hash is the BLAKE3 of the
    /// concatenated bodies. The frame itself isn't an addressable object;
    /// its members are.
    Frame = 2,
    /// Frame format v2 (written since 2026-11-15): the directory holds only
    /// member lengths (4 B each). Member hashes live in the index, and
    /// recovery recomputes them from the decompressed bodies.
    Frame2 = 3,
}

impl Kind {
    /// True for both frame formats.
    pub fn is_frame(self) -> bool {
        matches!(self, Kind::Frame | Kind::Frame2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// How a record payload is encoded.
pub enum Codec {
    /// Stored as is.
    Raw = 0,
    /// zstd-compressed.
    Zstd = 1,
}

impl Kind {
    fn from_u8(b: u8) -> Result<Kind> {
        match b {
            0 => Ok(Kind::Blob),
            1 => Ok(Kind::Manifest),
            2 => Ok(Kind::Frame),
            3 => Ok(Kind::Frame2),
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
    /// Record kind.
    pub kind: Kind,
    /// Payload codec.
    pub codec: Codec,
    /// Content hash of the logical object (for frames, of the concatenated bodies).
    pub hash: Hash,
    /// Logical (uncompressed) length.
    pub raw_len: u64,
    /// Stored payload length.
    pub stored_len: u32,
}

impl RecHeader {
    /// Total record length: header + payload + trailer.
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
    if stored_len > MAX_STORED || (kind == Kind::Blob && raw_len > MAX_BLOB) || (kind.is_frame() && raw_len > MAX_FRAME)
    {
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
        (Codec::Zstd, Kind::Frame | Kind::Frame2) => {
            return Err(Error::Corrupt("use decode_frame_bodies for frame records".into()));
        }
    };
    if h.kind == Kind::Blob && out.len() as u64 != h.raw_len {
        return Err(Error::Corrupt(format!("length mismatch for {}", h.hash)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Manifest: list of (chunk hash, chunk length)
// ---------------------------------------------------------------------------

/// Encodes a manifest node.
pub fn encode_manifest(chunks: &[(Hash, u64)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + chunks.len() * 40);
    v.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for (h, l) in chunks {
        v.extend_from_slice(h.as_bytes());
        v.extend_from_slice(&l.to_le_bytes());
    }
    v
}

/// Decodes a manifest node (length-checked).
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
// Frames
//   v1 (Kind::Frame):  [count u32][(hash[32], len u32) * count][bodies]
//   v2 (Kind::Frame2): [count u32][len u32 * count][bodies]
// Bodies are the concatenated members, codec-compressed. The record hash is
// the BLAKE3 of the concatenated (uncompressed) bodies.
// ---------------------------------------------------------------------------

/// Builds a v2 frame payload (record kind [`Kind::Frame2`]).
/// Returns `(codec, payload, frame_hash, raw_len)`.
pub fn encode_frame(members: &[&[u8]], level: i32) -> Result<(Codec, Vec<u8>, Hash, u64)> {
    if members.is_empty() || members.len() > MAX_FRAME_MEMBERS {
        return Err(Error::Parse("frame needs 1..=2^20 members".into()));
    }
    let mut dir = Vec::with_capacity(4 + members.len() * 4);
    dir.extend_from_slice(&(members.len() as u32).to_le_bytes());
    let mut bodies = Vec::new();
    for b in members {
        dir.extend_from_slice(&(b.len() as u32).to_le_bytes());
        bodies.extend_from_slice(b);
    }
    let raw_len = bodies.len() as u64;
    if raw_len > MAX_FRAME {
        return Err(Error::Parse("frame too large".into()));
    }
    let fh = Hash::of(&bodies);
    let (codec, body) = match zstd::bulk::compress(&bodies, level) {
        Ok(c) if level != 0 && c.len() < bodies.len() => (Codec::Zstd, c),
        _ => (Codec::Raw, bodies),
    };
    dir.extend_from_slice(&body);
    Ok((codec, dir, fh, raw_len))
}

/// A frame member: `(hash, offset in the decompressed frame, length)`.
pub type FrameMember = (Hash, u32, u32);

/// Parses a frame directory: `(per-member (hash if v1, offset, len), offset where bodies start)`.
/// A directory entry: `(hash if stored (v1), offset, length)`.
type DirEntry = (Option<Hash>, u32, u32);

fn frame_dir(kind: Kind, payload: &[u8]) -> Result<(Vec<DirEntry>, usize)> {
    let width = match kind {
        Kind::Frame => 36,
        Kind::Frame2 => 4,
        _ => return Err(Error::Corrupt("not a frame record".into())),
    };
    if payload.len() < 4 {
        return Err(Error::Corrupt("short frame".into()));
    }
    let n = rd_u32(&payload[0..4]) as usize;
    let dir_end = n
        .checked_mul(width)
        .and_then(|x| x.checked_add(4))
        .filter(|e| *e <= payload.len() && n <= MAX_FRAME_MEMBERS && n > 0);
    let Some(dir_end) = dir_end else { return Err(Error::Corrupt("bad frame directory".into())) };
    let mut out = Vec::with_capacity(n);
    let mut off: u64 = 0;
    for i in 0..n {
        let e = &payload[4 + i * width..4 + (i + 1) * width];
        let (h, len) = if width == 36 { (Some(rd_hash(&e[0..32])), rd_u32(&e[32..36])) } else { (None, rd_u32(e)) };
        if off + len as u64 > MAX_FRAME {
            return Err(Error::Corrupt("frame member out of bounds".into()));
        }
        out.push((h, off as u32, len));
        off += len as u64;
    }
    Ok((out, dir_end))
}

/// Decompresses a frame's bodies and checks them against the record header.
pub fn decode_frame_bodies(h: &RecHeader, payload: &[u8]) -> Result<Vec<u8>> {
    let (members, start) = frame_dir(h.kind, payload)?;
    let total: u64 = members.iter().map(|m| m.2 as u64).sum();
    if total != h.raw_len {
        return Err(Error::Corrupt("frame directory length mismatch".into()));
    }
    let body = &payload[start..];
    let out = match h.codec {
        Codec::Raw => body.to_vec(),
        Codec::Zstd => {
            zstd::bulk::decompress(body, h.raw_len as usize).map_err(|e| Error::Corrupt(format!("zstd: {e}")))?
        }
    };
    if out.len() as u64 != h.raw_len || Hash::of(&out) != h.hash {
        return Err(Error::Corrupt("frame body mismatch".into()));
    }
    Ok(out)
}

/// Members of a frame record with their content hashes. For v2 frames the
/// hashes are recomputed from the decompressed bodies (used by recovery).
pub fn frame_members(h: &RecHeader, payload: &[u8]) -> Result<Vec<FrameMember>> {
    let (dir, _) = frame_dir(h.kind, payload)?;
    if dir.iter().all(|d| d.0.is_some()) {
        return Ok(dir.into_iter().filter_map(|(h, o, l)| h.map(|h| (h, o, l))).collect());
    }
    let bodies = decode_frame_bodies(h, payload)?;
    Ok(dir.into_iter().map(|(_, o, l)| (Hash::of(&bodies[o as usize..o as usize + l as usize]), o, l)).collect())
}

// ---------------------------------------------------------------------------
// Pack index (sorted, fixed-size entries, BLAKE3 trailer)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// One pack index entry: where an object (or chunk, or frame member) lives in a pack.
pub struct IdxEntry {
    /// Object hash.
    pub hash: Hash,
    /// Record offset in the pack.
    pub offset: u64,
    /// Stored payload length of the record.
    pub stored_len: u32,
    /// Logical length of the object.
    pub raw_len: u64,
    /// Record kind (members of frames are `Blob`).
    pub kind: Kind,
    /// Payload codec.
    pub codec: Codec,
    /// For an object stored inside a frame: `(offset within the
    /// decompressed frame, decompressed frame length)`. Here `offset` and
    /// `stored_len` describe the frame record.
    pub frame: Option<(u32, u32)>,
}

/// Encodes a sorted index (format v2) with a BLAKE3 trailer.
pub fn encode_index(entries: &mut [IdxEntry]) -> Vec<u8> {
    entries.sort_by_key(|e| e.hash);
    // One record per distinct record offset (frame members share theirs).
    let mut recs: Vec<(u64, u32, u64, u8, u8)> = Vec::new();
    let mut rec_of: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    let mut ents: Vec<(Hash, u32, u32, u32)> = Vec::with_capacity(entries.len());
    for e in entries.iter() {
        let (kind, raw, inner, len) = match e.frame {
            Some((inner, flen)) => (Kind::Frame2 as u8, flen as u64, inner, e.raw_len as u32),
            None => (e.kind as u8, e.raw_len, 0, 0),
        };
        let id = *rec_of.entry(e.offset).or_insert_with(|| {
            recs.push((e.offset, e.stored_len, raw, kind, e.codec as u8));
            (recs.len() - 1) as u32
        });
        ents.push((e.hash, id, inner, len));
    }
    let mut v = Vec::with_capacity(24 + recs.len() * IDX3_REC + ents.len() * IDX3_ENT + 32);
    v.extend_from_slice(IDX_MAGIC_V3);
    v.extend_from_slice(&(recs.len() as u64).to_le_bytes());
    v.extend_from_slice(&(ents.len() as u64).to_le_bytes());
    for (off, sl, raw, kind, codec) in recs {
        v.extend_from_slice(&off.to_le_bytes());
        v.extend_from_slice(&sl.to_le_bytes());
        v.extend_from_slice(&raw.to_le_bytes());
        v.push(kind);
        v.push(codec);
    }
    for (h, id, inner, len) in ents {
        v.extend_from_slice(h.as_bytes());
        v.extend_from_slice(&id.to_le_bytes());
        v.extend_from_slice(&inner.to_le_bytes());
        v.extend_from_slice(&len.to_le_bytes());
    }
    let sum = blake3::hash(&v);
    v.extend_from_slice(sum.as_bytes());
    v
}

/// Zero-copy view over a v3 index (for example, a memory-mapped file). Lookups
/// binary-search the sorted entry table, so memory use is the OS page cache, not
/// the process heap.
pub struct IdxV3View<T: AsRef<[u8]>> {
    data: T,
    nr: usize,
    ne: usize,
}

impl<T: AsRef<[u8]>> IdxV3View<T> {
    /// Validates the header, sizes and BLAKE3 trailer of a v3 index.
    pub fn new(data: T) -> Result<Self> {
        let b = data.as_ref();
        if b.len() < 24 + 32 || &b[..8] != IDX_MAGIC_V3 {
            return Err(Error::Corrupt("not a v3 index".into()));
        }
        let (body, sum) = b.split_at(b.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return Err(Error::Corrupt("index checksum mismatch".into()));
        }
        let (nr, ne) = (rd_u64(&body[8..16]), rd_u64(&body[16..24]));
        let size = usize::try_from(nr)
            .ok()
            .and_then(|r| r.checked_mul(IDX3_REC))
            .zip(usize::try_from(ne).ok().and_then(|e| e.checked_mul(IDX3_ENT)))
            .and_then(|(a, b)| a.checked_add(b))
            .and_then(|x| x.checked_add(24));
        if size != Some(body.len()) {
            return Err(Error::Corrupt("index length mismatch".into()));
        }
        Ok(IdxV3View { nr: nr as usize, ne: ne as usize, data })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.ne
    }

    /// True if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.ne == 0
    }

    fn entry_hash(&self, i: usize) -> &[u8] {
        let base = 24 + self.nr * IDX3_REC + i * IDX3_ENT;
        &self.data.as_ref()[base..base + 32]
    }

    /// Decodes entry `i` (`i < len()`).
    pub fn entry(&self, i: usize) -> Result<IdxEntry> {
        let body = self.data.as_ref();
        decode_v3_entry(
            &body[24..24 + self.nr * IDX3_REC],
            &body[24 + self.nr * IDX3_REC + i * IDX3_ENT..][..IDX3_ENT],
            self.nr,
        )
    }

    /// Binary search by hash.
    pub fn find(&self, h: &Hash) -> Result<Option<IdxEntry>> {
        let (mut lo, mut hi) = (0usize, self.ne);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.entry_hash(mid).cmp(h.as_bytes().as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return self.entry(mid).map(Some),
            }
        }
        Ok(None)
    }
}

fn decode_v3_entry(recs: &[u8], e: &[u8], nr: usize) -> Result<IdxEntry> {
    let id = rd_u32(&e[32..36]) as usize;
    if id >= nr {
        return Err(Error::Corrupt("index entry references a missing record".into()));
    }
    let r = &recs[id * IDX3_REC..(id + 1) * IDX3_REC];
    let (offset, stored_len, raw) = (rd_u64(&r[0..8]), rd_u32(&r[8..12]), rd_u64(&r[12..20]));
    let kind = Kind::from_u8(r[20])?;
    let codec = Codec::from_u8(r[21])?;
    let (inner, len) = (rd_u32(&e[36..40]), rd_u32(&e[40..44]));
    if kind.is_frame() {
        if raw > MAX_FRAME || inner as u64 + len as u64 > raw {
            return Err(Error::Corrupt("frame member out of bounds in index".into()));
        }
        Ok(IdxEntry {
            hash: rd_hash(&e[0..32]),
            offset,
            stored_len,
            raw_len: len as u64,
            kind: Kind::Blob,
            codec,
            frame: Some((inner, raw as u32)),
        })
    } else {
        Ok(IdxEntry { hash: rd_hash(&e[0..32]), offset, stored_len, raw_len: raw, kind, codec, frame: None })
    }
}

fn decode_index_v3(body: &[u8]) -> Result<Vec<IdxEntry>> {
    if body.len() < 24 {
        return Err(Error::Corrupt("short v3 index".into()));
    }
    let nr = rd_u64(&body[8..16]);
    let ne = rd_u64(&body[16..24]);
    let size = usize::try_from(nr)
        .ok()
        .and_then(|r| r.checked_mul(IDX3_REC))
        .zip(usize::try_from(ne).ok().and_then(|e| e.checked_mul(IDX3_ENT)))
        .and_then(|(a, b)| a.checked_add(b))
        .and_then(|x| x.checked_add(24));
    if size != Some(body.len()) {
        return Err(Error::Corrupt("index length mismatch".into()));
    }
    let (nr, ne) = (nr as usize, ne as usize);
    let recs = &body[24..24 + nr * IDX3_REC];
    let ents = &body[24 + nr * IDX3_REC..];
    (0..ne).map(|i| decode_v3_entry(recs, &ents[i * IDX3_ENT..(i + 1) * IDX3_ENT], nr)).collect()
}

/// Decodes and verifies an index (format v1, v2 or v3).
pub fn decode_index(b: &[u8]) -> Result<Vec<IdxEntry>> {
    if b.len() < 16 + 32 {
        return Err(Error::Corrupt("bad index header".into()));
    }
    if &b[..8] == IDX_MAGIC_V3 {
        let (body, sum) = b.split_at(b.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return Err(Error::Corrupt("index checksum mismatch".into()));
        }
        return decode_index_v3(body);
    }
    let width = if &b[..8] == IDX_MAGIC {
        IDX_ENTRY
    } else if &b[..8] == IDX_MAGIC_V2 {
        IDX_ENTRY_V2
    } else {
        return Err(Error::Corrupt("bad index header".into()));
    };
    let (body, sum) = b.split_at(b.len() - 32);
    if blake3::hash(body).as_bytes() != sum {
        return Err(Error::Corrupt("index checksum mismatch".into()));
    }
    let n = rd_u64(&body[8..16]);
    let expect = usize::try_from(n).ok().and_then(|n| n.checked_mul(width)).and_then(|x| x.checked_add(16));
    let n = n as usize;
    if expect != Some(body.len()) {
        return Err(Error::Corrupt("index length mismatch".into()));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &body[16 + i * width..16 + (i + 1) * width];
        let frame = if width == IDX_ENTRY_V2 {
            let (inner, flen) = (rd_u32(&e[54..58]), rd_u32(&e[58..62]));
            if flen == 0 { None } else { Some((inner, flen)) }
        } else {
            None
        };
        let kind = Kind::from_u8(e[52])?;
        if kind.is_frame() {
            return Err(Error::Corrupt("frame records are not indexed".into()));
        }
        out.push(IdxEntry {
            hash: rd_hash(&e[0..32]),
            offset: rd_u64(&e[32..40]),
            stored_len: rd_u32(&e[40..44]),
            raw_len: rd_u64(&e[44..52]),
            kind,
            codec: Codec::from_u8(e[53])?,
            frame,
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
                frame: if i % 2 == 0 { Some((i as u32, 100)) } else { None },
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
