//! Xet CAS upload protocol for the HF front (what `hf_xet` speaks; the default upload path of
//! `huggingface_hub` when `hf_xet` is installed).
//!
//! Flow:
//! 1. `GET /api/<type>s/<repo>/xet-write-token/<rev>` returns `{casUrl, accessToken, exp}`. The
//!    token is an HMAC capability for that repo (see `hf.rs`).
//! 2. `POST <casUrl>/v1/xorbs/default/<xorb>`: a xorb is a sequence of compressed chunks, plus
//!    a footer that is ignored. We decompress it, recompute every chunk hash and the xorb's
//!    Merkle hash, and refuse any mismatch. The decompressed bytes become one core object,
//!    stored with the repo namespace's chunker (Xet chunking makes its chunks dedup against
//!    the files). A small record object (`P2PXORB1`: content hash, then each chunk's hash
//!    and end offset) is indexed by xorb hash in the segment map `git/_xet`. GC follows
//!    record → content (`GitTracer`).
//! 3. `POST <casUrl>/v1/shards`: a shard lists files as segments (xorb, chunk range), with a
//!    verification hash per segment and the file's sha256. Each segment's verification hash
//!    must match our recorded chunk hashes (the Xet rule: you can only reference chunks whose
//!    hashes you know). Each file is then rebuilt from the xorbs into a core object, and its
//!    Xet file hash and sha256 are both checked. Finally it is registered in the LFS index
//!    (and this repo's LFS ACL), exactly like an LFS upload, so the commit's `lfsFile`
//!    entries, `resolve` and GC work unchanged.
//!
//! Not implemented: global dedup queries (`/v1/chunks/...` always answers 404, so clients
//! upload what they don't have locally) and Xet downloads (`resolve` sends no `X-Xet-Hash`,
//! so clients download over plain HTTP).

use crate::hf::{HfState, RepoId};
use pico2peta_core::{Error, Hash, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;

pub type MerkleHash = [u8; 32];

const DATA_KEY: [u8; 32] = [
    102, 151, 245, 119, 91, 149, 80, 222, 49, 53, 203, 172, 165, 151, 24, 28, 157, 228, 33, 16, 155, 235, 43, 88, 180,
    208, 176, 75, 147, 173, 242, 41,
];
const INTERNAL_NODE_KEY: [u8; 32] = [
    1, 126, 197, 199, 165, 71, 41, 150, 253, 148, 102, 102, 180, 138, 2, 230, 93, 221, 83, 111, 55, 199, 109, 210, 248,
    99, 82, 230, 74, 83, 113, 63,
];
const VERIFICATION_KEY: [u8; 32] = [
    127, 24, 87, 214, 206, 86, 237, 102, 18, 127, 249, 19, 231, 165, 195, 243, 164, 205, 38, 213, 181, 219, 73, 230,
    65, 36, 152, 127, 40, 251, 148, 195,
];
const SHARD_HEADER_TAG: [u8; 32] = [
    72, 70, 82, 101, 112, 111, 77, 101, 116, 97, 68, 97, 116, 97, 0, 85, 105, 103, 69, 106, 123, 129, 87, 131, 165,
    189, 217, 92, 205, 209, 74, 169,
];
/// Largest chunk (2 × 64 KiB target).
const MAX_CHUNK: usize = 128 * 1024;
pub const MAX_XORB_BODY: usize = 80 << 20;
/// Uncompressed chunk data per xorb (xet-core's `MAX_XORB_BYTES`).
const MAX_XORB_DATA: usize = 64 << 20;
pub const MAX_SHARD_BODY: usize = 128 << 20;
const XORB_RECORD_MAGIC: &[u8; 8] = b"P2PXORB1";
const FILE_RECORD_MAGIC: &[u8; 8] = b"P2PXFIL1";
const FILE_RECORD_HEAD: usize = 8 + 32 + 32 + 32 + 4;
/// Downloads present each file as "virtual xorbs" of at most this many chunks and bytes
/// (the limits of real xorbs).
const VXORB_CHUNKS: usize = 8 * 1024;
const VXORB_BYTES: u64 = 64 << 20;

// ---------------------------------------------------------------------------
// Hashing (byte-exact with xet-core's merklehash)
// ---------------------------------------------------------------------------

pub fn chunk_hash(data: &[u8]) -> MerkleHash {
    *blake3::keyed_hash(&DATA_KEY, data).as_bytes()
}

/// Xet's hex form: the four little-endian u64 words, each as 16 hex digits.
pub fn to_hex(h: &MerkleHash) -> String {
    h.as_chunks::<8>().0.iter().map(|w| format!("{:016x}", u64::from_le_bytes(*w))).collect()
}

pub fn from_hex(s: &str) -> Option<MerkleHash> {
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..4 {
        let w = u64::from_str_radix(&s[16 * i..16 * i + 16], 16).ok()?;
        out[8 * i..8 * i + 8].copy_from_slice(&w.to_le_bytes());
    }
    Some(out)
}

fn word3(h: &MerkleHash) -> u64 {
    u64::from_le_bytes(h[24..32].try_into().unwrap_or([0; 8]))
}

fn merged(nodes: &[(MerkleHash, u64)]) -> (MerkleHash, u64) {
    let mut buf = String::with_capacity(nodes.len() * 88);
    let mut total = 0u64;
    for (h, s) in nodes {
        buf.push_str(&to_hex(h));
        buf.push_str(" : ");
        buf.push_str(&s.to_string());
        buf.push('\n');
        total += s;
    }
    (*blake3::keyed_hash(&INTERNAL_NODE_KEY, buf.as_bytes()).as_bytes(), total)
}

fn next_cut(h: &[(MerkleHash, u64)]) -> usize {
    if h.len() <= 2 {
        return h.len();
    }
    let end = 9.min(h.len());
    (2..end).find(|&i| word3(&h[i].0).is_multiple_of(4)).map_or(end, |i| i + 1)
}

fn aggregated(chunks: &[(MerkleHash, u64)]) -> MerkleHash {
    if chunks.is_empty() {
        return [0; 32];
    }
    let mut hv = chunks.to_vec();
    while hv.len() > 1 {
        let mut out = Vec::with_capacity(hv.len() / 2 + 1);
        let mut i = 0;
        while i < hv.len() {
            let c = i + next_cut(&hv[i..]);
            out.push(merged(&hv[i..c]));
            i = c;
        }
        hv = out;
    }
    hv[0].0
}

pub fn xorb_hash(chunks: &[(MerkleHash, u64)]) -> MerkleHash {
    aggregated(chunks)
}

pub fn file_hash(chunks: &[(MerkleHash, u64)]) -> MerkleHash {
    if chunks.is_empty() {
        return [0; 32];
    }
    *blake3::keyed_hash(&[0u8; 32], &aggregated(chunks)).as_bytes()
}

fn range_hash(chunks: &[MerkleHash]) -> MerkleHash {
    *blake3::keyed_hash(&VERIFICATION_KEY, &chunks.concat()).as_bytes()
}

// ---------------------------------------------------------------------------
// Xorb parsing
// ---------------------------------------------------------------------------

fn bad(m: impl Into<String>) -> Error {
    Error::Parse(format!("xet: {}", m.into()))
}

/// Inverse of Xet's BG4 split: plane `k` holds bytes `k, k+4, ...`; the first `len % 4` planes
/// are one byte longer.
fn bg4_regroup(g: &[u8]) -> Vec<u8> {
    let n = g.len();
    let (split, rem) = (n / 4, n % 4);
    let sizes = [split + (rem > 0) as usize, split + (rem > 1) as usize, split + (rem > 2) as usize, split];
    let mut starts = [0usize; 4];
    for k in 1..4 {
        starts[k] = starts[k - 1] + sizes[k - 1];
    }
    let mut out = vec![0u8; n];
    for (i, b) in out.iter_mut().enumerate() {
        *b = g[starts[i % 4] + i / 4];
    }
    out
}

fn lz4_decompress(data: &[u8], expect: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expect);
    lz4_flex::frame::FrameDecoder::new(data)
        .take(expect as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| bad(format!("lz4: {e}")))?;
    Ok(out)
}

/// Decompressed chunks of a serialized xorb (the footer, if any, is skipped).
pub fn parse_xorb(b: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut chunks = Vec::new();
    let mut i = 0;
    // Real xorbs hold at most 64 MiB of chunk data; LZ4 could otherwise expand a small body
    // ~255×, so bound the total before decompressing (memory exhaustion).
    let mut total = 0usize;
    while i < b.len() {
        if b[i..].starts_with(b"XETBLOB") {
            break; // footer
        }
        let h = b.get(i..i + 8).ok_or_else(|| bad("truncated chunk header"))?;
        if h[0] != 0 {
            return Err(bad(format!("chunk header version {}", h[0])));
        }
        let clen = u32::from_le_bytes([h[1], h[2], h[3], 0]) as usize;
        let ulen = u32::from_le_bytes([h[5], h[6], h[7], 0]) as usize;
        if ulen > MAX_CHUNK || clen > 2 * MAX_CHUNK {
            return Err(bad("chunk too large"));
        }
        total += ulen;
        if total > MAX_XORB_DATA {
            return Err(bad("xorb holds more than 64 MiB of chunk data"));
        }
        let data = b.get(i + 8..i + 8 + clen).ok_or_else(|| bad("truncated chunk"))?;
        let raw = match h[4] {
            0 => data.to_vec(),
            1 => lz4_decompress(data, ulen)?,
            2 => bg4_regroup(&lz4_decompress(data, ulen)?),
            s => return Err(bad(format!("unknown compression scheme {s}"))),
        };
        if raw.len() != ulen {
            return Err(bad("chunk length mismatch"));
        }
        chunks.push(raw);
        i += 8 + clen;
        if chunks.len() > 64 * 1024 {
            return Err(bad("too many chunks"));
        }
    }
    if chunks.is_empty() {
        return Err(bad("empty xorb"));
    }
    Ok(chunks)
}

// ---------------------------------------------------------------------------
// Xorb records
// ---------------------------------------------------------------------------

/// What we keep per xorb: the content object, and each chunk's hash and end offset.
pub struct XorbRecord {
    pub content: Hash,
    pub chunks: Vec<(MerkleHash, u32)>,
}

impl XorbRecord {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(44 + 36 * self.chunks.len());
        v.extend_from_slice(XORB_RECORD_MAGIC);
        v.extend_from_slice(self.content.as_bytes());
        v.extend_from_slice(&(self.chunks.len() as u32).to_le_bytes());
        for (h, end) in &self.chunks {
            v.extend_from_slice(h);
            v.extend_from_slice(&end.to_le_bytes());
        }
        v
    }

    pub fn decode(b: &[u8]) -> Option<XorbRecord> {
        if !b.starts_with(XORB_RECORD_MAGIC) || b.len() < 44 {
            return None;
        }
        let content = Hash(b[8..40].try_into().ok()?);
        let n = u32::from_le_bytes(b[40..44].try_into().ok()?) as usize;
        if b.len() != 44 + 36 * n {
            return None;
        }
        let chunks = b[44..]
            .as_chunks::<36>()
            .0
            .iter()
            .map(|c| {
                let (h, e) = c.split_at(32);
                (h.try_into().unwrap_or([0; 32]), u32::from_le_bytes(e.try_into().unwrap_or([0; 4])))
            })
            .collect();
        Some(XorbRecord { content, chunks })
    }

    fn start(&self, i: usize) -> u64 {
        if i == 0 { 0 } else { self.chunks[i - 1].1 as u64 }
    }
}

/// GC: the content object a xorb record references, if `head` (the object's first bytes) and
/// `size` describe one.
pub fn record_child(h: &Hash, head: &[u8], size: u64, store: &dyn pico2peta_core::ObjectStore) -> Result<Option<Hash>> {
    let head_size = FILE_RECORD_HEAD as u64;
    if head.starts_with(FILE_RECORD_MAGIC) && size >= head_size && (size - head_size).is_multiple_of(40) {
        let b = store.get(h, Some(0..40))?;
        return Ok(Some(Hash(b[8..40].try_into().map_err(|_| bad("file record"))?)));
    }
    if !head.starts_with(XORB_RECORD_MAGIC) || size < 44 || !(size - 44).is_multiple_of(36) {
        return Ok(None);
    }
    let b = store.get(h, Some(0..44))?;
    Ok(Some(Hash(b[8..40].try_into().map_err(|_| bad("record"))?)))
}

/// Stores an uploaded xorb. Returns whether it was new.
pub(crate) fn put_xorb(hf: &HfState, repo: &RepoId, url_hash: &MerkleHash, body: &[u8]) -> Result<bool> {
    let key = *url_hash;
    if hf.xet.get_live_blocking(&key)?.is_some() {
        return Ok(false);
    }
    let chunks = parse_xorb(body)?;
    let hs: Vec<(MerkleHash, u64)> = chunks.iter().map(|c| (chunk_hash(c), c.len() as u64)).collect();
    if xorb_hash(&hs) != *url_hash {
        return Err(bad(format!("xorb hash mismatch: body is {}", to_hex(&xorb_hash(&hs)))));
    }
    let mut ends = Vec::with_capacity(chunks.len());
    let mut off = 0u64;
    for (h, l) in &hs {
        off += l;
        ends.push((*h, u32::try_from(off).map_err(|_| bad("xorb too large"))?));
    }
    let mut cat = std::io::Cursor::new(chunks.concat());
    let content = hf.put_content(repo, &mut cat)?;
    let rec = XorbRecord { content, chunks: ends };
    let rh = hf.git_repo().put(&rec.encode())?;
    let n = hf.xet.insert_blocking(vec![(key, crate::segmap::Value { hash: rh, aux: off })])?;
    Ok(n > 0)
}

/// The records of many xorbs, with one batched map lookup (missing or collected ones absent).
fn load_records(hf: &HfState, xorbs: &[MerkleHash]) -> Result<HashMap<MerkleHash, XorbRecord>> {
    let mut keys: Vec<MerkleHash> = xorbs.to_vec();
    keys.sort_unstable();
    keys.dedup();
    let (_, root) = hf.xet.root_blocking()?;
    let vals = hf.xet.get_many(&root, &keys)?;
    let mut out = HashMap::with_capacity(keys.len());
    for (k, v) in keys.into_iter().zip(vals) {
        // As get_live_blocking: a collected record is a miss; a hit is made safe to use.
        let Some(v) = v else { continue };
        if !hf.git_repo().ensure_live(&v.hash)? {
            continue;
        }
        let b = hf.git_repo().get(&v.hash, None)?;
        let rec = XorbRecord::decode(&b).ok_or_else(|| Error::Corrupt(format!("bad xorb record {}", v.hash)))?;
        out.insert(k, rec);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shards
// ---------------------------------------------------------------------------

struct Segment {
    xorb: MerkleHash,
    bytes: u32,
    start: u32,
    end: u32,
}

struct ShardFile {
    hash: MerkleHash,
    segments: Vec<Segment>,
    verification: Vec<MerkleHash>,
    sha256: Option<[u8; 32]>,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap_or([0; 4]))
}

fn parse_shard(b: &[u8]) -> Result<Vec<ShardFile>> {
    if b.len() < 48 || b[..32] != SHARD_HEADER_TAG {
        return Err(bad("not a shard (bad magic)"));
    }
    let version = u64::from_le_bytes(b[32..40].try_into().unwrap_or([0; 8]));
    if version != 2 {
        return Err(bad(format!("shard version {version}")));
    }
    let mut i = 48;
    let mut next = |n: usize| -> Result<&[u8]> {
        let s = b.get(i..i + n).ok_or_else(|| bad("truncated shard"))?;
        i += n;
        Ok(s)
    };
    let mut files = Vec::new();
    loop {
        let h = next(48)?;
        let hash: MerkleHash = h[..32].try_into().unwrap_or([0; 32]);
        if hash == [0xff; 32] {
            break;
        }
        let flags = u32_at(h, 32);
        let n = u32_at(h, 36) as usize;
        if n > 1 << 20 {
            return Err(bad("too many segments"));
        }
        let mut segments = Vec::with_capacity(n);
        for _ in 0..n {
            let e = next(48)?;
            segments.push(Segment {
                xorb: e[..32].try_into().unwrap_or([0; 32]),
                bytes: u32_at(e, 36),
                start: u32_at(e, 40),
                end: u32_at(e, 44),
            });
        }
        let mut verification = Vec::new();
        if flags & (1 << 31) != 0 {
            for _ in 0..n {
                verification.push(next(48)?[..32].try_into().unwrap_or([0; 32]));
            }
        }
        let sha256 = if flags & (1 << 30) != 0 { Some(next(48)?[..32].try_into().unwrap_or([0; 32])) } else { None };
        files.push(ShardFile { hash, segments, verification, sha256 });
    }
    // The xorb section follows; we rely on our own records instead.
    Ok(files)
}

/// Registers the files of an uploaded shard. Returns how many files were registered.
pub(crate) fn put_shard(hf: &HfState, repo: &RepoId, body: &[u8]) -> Result<usize> {
    let files = parse_shard(body)?;
    let xorbs: Vec<MerkleHash> = files.iter().flat_map(|f| f.segments.iter().map(|s| s.xorb)).collect();
    let records = load_records(hf, &xorbs)?;
    for f in &files {
        let Some(sha) = f.sha256 else { return Err(bad("shard file without sha256 metadata")) };
        if f.verification.len() != f.segments.len() {
            return Err(bad("shard file without verification entries"));
        }
        // Validate every segment, collecting (record, byte range, chunk hashes).
        let mut plan = Vec::with_capacity(f.segments.len());
        let mut chunk_list: Vec<(MerkleHash, u64)> = Vec::new();
        let mut total = 0u64;
        for (s, v) in f.segments.iter().zip(&f.verification) {
            let rec = records
                .get(&s.xorb)
                .ok_or_else(|| bad(format!("xorb {} is not stored here; upload it first", to_hex(&s.xorb))))?;
            let (a, z) = (s.start as usize, s.end as usize);
            if a >= z || z > rec.chunks.len() {
                return Err(bad("segment chunk range out of bounds"));
            }
            let hs: Vec<MerkleHash> = rec.chunks[a..z].iter().map(|c| c.0).collect();
            if range_hash(&hs) != *v {
                return Err(bad("segment verification hash mismatch"));
            }
            let (from, to) = (rec.start(a), rec.chunks[z - 1].1 as u64);
            if to - from != s.bytes as u64 {
                return Err(bad("segment length mismatch"));
            }
            for i in a..z {
                chunk_list.push((rec.chunks[i].0, rec.chunks[i].1 as u64 - rec.start(i)));
            }
            total += to - from;
            plan.push((rec.content, from..to));
        }
        if file_hash(&chunk_list) != f.hash {
            return Err(bad("file hash does not match its chunks"));
        }
        // Rebuild the file into a core object while hashing it.
        let mut src = Concat { store: hf.git_repo(), plan: plan.into_iter(), cur: None, sha: Sha256::new(), n: 0 };
        let content = hf.put_content(repo, &mut src)?;
        // The shard stores sha256 as a DataHash built with `from_be_bytes`: its word-wise hex
        // is the ordinary sha256 hex.
        let (want, got) = (to_hex(&sha), crate::git::hex(&src.sha.finalize()));
        if got != want || src.n != total {
            return Err(bad(format!("file content is sha256 {got}, shard says {want}")));
        }
        hf.register_lfs(repo, &want, content, total)?;
        // For Xet downloads: the file's chunk list, next to its content.
        let mut ends = Vec::with_capacity(chunk_list.len());
        let mut off = 0u64;
        for (h, l) in &chunk_list {
            off += l;
            ends.push((*h, off));
        }
        let rec = FileRecord { content, file_hash: f.hash, sha256: sha_bytes(&want)?, chunks: ends };
        let rh = hf.git_repo().put(&rec.encode())?;
        let v = crate::segmap::Value { hash: rh, aux: total };
        hf.xetfile.insert_blocking(vec![(f.hash, v)])?;
        hf.xetsha.insert_blocking(vec![(rec.sha256, v)])?;
    }
    Ok(files.len())
}

fn sha_bytes(hex64: &str) -> Result<[u8; 32]> {
    crate::hf::sha_key(hex64).ok_or_else(|| bad("bad sha256"))
}

// ---------------------------------------------------------------------------
// Downloads: file records and virtual xorbs
// ---------------------------------------------------------------------------

/// A file uploaded through Xet: its content object, ids, and Xet chunks (hash, end offset).
pub(crate) struct FileRecord {
    pub content: Hash,
    pub file_hash: MerkleHash,
    /// Plain sha256 bytes.
    pub sha256: [u8; 32],
    pub chunks: Vec<(MerkleHash, u64)>,
}

impl FileRecord {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(FILE_RECORD_HEAD + 40 * self.chunks.len());
        v.extend_from_slice(FILE_RECORD_MAGIC);
        v.extend_from_slice(self.content.as_bytes());
        v.extend_from_slice(&self.file_hash);
        v.extend_from_slice(&self.sha256);
        v.extend_from_slice(&(self.chunks.len() as u32).to_le_bytes());
        for (h, end) in &self.chunks {
            v.extend_from_slice(h);
            v.extend_from_slice(&end.to_le_bytes());
        }
        v
    }

    pub(crate) fn decode(b: &[u8]) -> Option<FileRecord> {
        if !b.starts_with(FILE_RECORD_MAGIC) || b.len() < FILE_RECORD_HEAD {
            return None;
        }
        let n = u32::from_le_bytes(b[104..108].try_into().ok()?) as usize;
        if b.len() != FILE_RECORD_HEAD + 40 * n {
            return None;
        }
        let chunks = b[FILE_RECORD_HEAD..]
            .as_chunks::<40>()
            .0
            .iter()
            .map(|c| {
                let (h, e) = c.split_at(32);
                (h.try_into().unwrap_or([0; 32]), u64::from_le_bytes(e.try_into().unwrap_or([0; 8])))
            })
            .collect();
        Some(FileRecord {
            content: Hash(b[8..40].try_into().ok()?),
            file_hash: b[40..72].try_into().ok()?,
            sha256: b[72..104].try_into().ok()?,
            chunks,
        })
    }

    fn start(&self, i: usize) -> u64 {
        if i == 0 { 0 } else { self.chunks[i - 1].1 }
    }

    /// Virtual xorbs: consecutive chunk index ranges within the xorb limits.
    pub(crate) fn groups(&self) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut a = 0;
        while a < self.chunks.len() {
            let mut b = a + 1;
            while b < self.chunks.len() && b - a < VXORB_CHUNKS && self.chunks[b].1 - self.start(a) <= VXORB_BYTES {
                b += 1;
            }
            out.push(a..b);
            a = b;
        }
        out
    }

    /// The Merkle hash of a virtual xorb (what a real xorb of these chunks would be called).
    pub(crate) fn group_hash(&self, g: &std::ops::Range<usize>) -> MerkleHash {
        let hs: Vec<(MerkleHash, u64)> =
            g.clone().map(|i| (self.chunks[i].0, self.chunks[i].1 - self.start(i))).collect();
        xorb_hash(&hs)
    }

    /// Byte offset of chunk `k` (relative to the group start) in the group's serialization:
    /// every chunk is an 8-byte header plus its uncompressed bytes.
    fn ser_offset(&self, g: &std::ops::Range<usize>, k: usize) -> u64 {
        (g.start..g.start + k).map(|i| 8 + self.chunks[i].1 - self.start(i)).sum()
    }

    /// V1 reconstruction for bytes `[from, to)` of the file. `url(group index)` names the fetch URL.
    pub(crate) fn reconstruction(&self, from: u64, to: u64, url: &dyn Fn(usize) -> String) -> serde_json::Value {
        let mut terms = Vec::new();
        let mut fetch = serde_json::Map::new();
        let mut offset_into_first = 0;
        for (gi, g) in self.groups().into_iter().enumerate() {
            let (gs, ge) = (self.start(g.start), self.chunks[g.end - 1].1);
            if ge <= from || gs >= to {
                continue;
            }
            // Chunks of this group overlapping the range.
            let a = (g.start..g.end).find(|&i| self.chunks[i].1 > from).unwrap_or(g.start);
            let b = (a..g.end).find(|&i| self.chunks[i].1 >= to).map_or(g.end, |i| i + 1);
            if terms.is_empty() {
                offset_into_first = from.saturating_sub(self.start(a));
            }
            let hash = to_hex(&self.group_hash(&g));
            let (la, lb) = (a - g.start, b - g.start);
            terms.push(serde_json::json!({
                "hash": hash, "unpacked_length": self.chunks[b - 1].1 - self.start(a),
                "range": { "start": la, "end": lb },
            }));
            let (bs, be) = (self.ser_offset(&g, la), self.ser_offset(&g, lb));
            fetch.entry(hash).or_insert_with(|| serde_json::json!([])).as_array_mut().into_iter().for_each(|v| {
                v.push(serde_json::json!({
                    "range": { "start": la, "end": lb }, "url": url(gi),
                    "url_range": { "start": bs, "end": be - 1 },
                }))
            });
        }
        serde_json::json!({ "offset_into_first_range": offset_into_first, "terms": terms, "fetch_info": fetch })
    }

    /// Bytes `[bs, be)` of virtual xorb `gi`'s serialization (chunk aligned, as we hand out).
    pub(crate) fn serialize_range(
        &self,
        store: &dyn pico2peta_core::Repository,
        gi: usize,
        bs: u64,
        be: u64,
    ) -> Result<Vec<u8>> {
        let groups = self.groups();
        let g = groups.get(gi).ok_or_else(|| bad("no such xorb"))?.clone();
        let mut out = Vec::with_capacity((be - bs) as usize);
        let mut pos = 0u64;
        let mut first = None;
        let mut last = g.start;
        for i in g.clone() {
            let len = self.chunks[i].1 - self.start(i);
            if pos + 8 + len > bs && pos < be {
                first.get_or_insert(i);
                last = i + 1;
            }
            pos += 8 + len;
        }
        let Some(first) = first else { return Ok(out) };
        let data = store.get(&self.content, Some(self.start(first)..self.chunks[last - 1].1))?;
        let base = self.start(first);
        let mut ser_pos = self.ser_offset(&g, first - g.start);
        for i in first..last {
            let (s, e) = (self.start(i) - base, self.chunks[i].1 - base);
            let len = (e - s) as u32;
            let l = len.to_le_bytes();
            let mut piece = vec![0u8, l[0], l[1], l[2], 0, l[0], l[1], l[2]];
            piece.extend_from_slice(&data[s as usize..e as usize]);
            // Clip to the requested range (normally chunk aligned, so whole pieces).
            let (ps, pe) = (ser_pos, ser_pos + piece.len() as u64);
            let (cs, ce) = (bs.max(ps) - ps, be.min(pe) - ps);
            out.extend_from_slice(&piece[cs as usize..ce as usize]);
            ser_pos = pe;
        }
        Ok(out)
    }
}

/// Reads byte ranges of core objects back to back, hashing them with sha256.
struct Concat<'a, I> {
    store: &'a dyn pico2peta_core::Repository,
    plan: I,
    cur: Option<Box<dyn Read + Send>>,
    sha: Sha256,
    n: u64,
}

impl<I: Iterator<Item = (Hash, std::ops::Range<u64>)>> Read for Concat<'_, I> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.cur.is_none() {
                let Some((h, r)) = self.plan.next() else { return Ok(0) };
                self.cur = Some(self.store.open(&h, Some(r)).map_err(|e| std::io::Error::other(e.to_string()))?);
            }
            let k = self.cur.as_mut().map_or(Ok(0), |r| r.read(buf))?;
            if k == 0 {
                self.cur = None;
                continue;
            }
            self.sha.update(&buf[..k]);
            self.n += k as u64;
            return Ok(k);
        }
    }
}

/// Fuzzing: every parser of untrusted Xet bytes (must never panic).
pub fn fuzz_parsers(data: &[u8]) {
    if let Ok(chunks) = parse_xorb(data) {
        let hs: Vec<(MerkleHash, u64)> = chunks.iter().map(|c| (chunk_hash(c), c.len() as u64)).collect();
        let _ = (xorb_hash(&hs), file_hash(&hs));
    }
    let _ = parse_shard(data);
    let _ = XorbRecord::decode(data);
    if let Some(r) = FileRecord::decode(data) {
        // Records come from our own store, but decoding must stay safe on any bytes.
        if r.chunks.windows(2).all(|w| w[0].1 < w[1].1) && r.chunks.first().is_none_or(|c| c.1 > 0) {
            let size = r.chunks.last().map_or(0, |c| c.1);
            let _ = r.reconstruction(0, size, &|g| format!("u{g}"));
        }
    }
    let _ = from_hex(std::str::from_utf8(data).unwrap_or(""));
    let _ = bg4_regroup(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip_is_word_wise() {
        let mut h = [0u8; 32];
        h[0] = 1; // low byte of word 0
        let s = to_hex(&h);
        assert_eq!(&s[..16], "0000000000000001");
        assert_eq!(from_hex(&s), Some(h));
    }

    #[test]
    fn bg4_regroup_inverts_split() {
        for n in 0..23usize {
            let data: Vec<u8> = (0..n as u8).collect();
            // Xet's split: plane k = bytes k, k+4, ...
            let mut g = Vec::new();
            for k in 0..4 {
                g.extend(data.iter().skip(k).step_by(4));
            }
            assert_eq!(bg4_regroup(&g), data, "n={n}");
        }
    }

    #[test]
    fn xorb_expansion_is_bounded() {
        // 600 LZ4 chunks of 128 KiB zeros: a small body, ~76 MiB decompressed.
        let zeros = vec![0u8; MAX_CHUNK];
        let mut w = lz4_flex::frame::FrameEncoder::new(Vec::new());
        std::io::Write::write_all(&mut w, &zeros).unwrap();
        let z = w.finish().unwrap();
        let mut x = Vec::new();
        for _ in 0..600 {
            let cl = (z.len() as u32).to_le_bytes();
            let ul = (zeros.len() as u32).to_le_bytes();
            x.extend_from_slice(&[0, cl[0], cl[1], cl[2], 1, ul[0], ul[1], ul[2]]);
            x.extend_from_slice(&z);
        }
        assert!(x.len() < 1 << 20, "the body stays small");
        assert!(parse_xorb(&x).unwrap_err().to_string().contains("64 MiB"));
    }

    #[test]
    fn xorb_parse_and_hash() {
        let a = vec![7u8; 1000];
        let b: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut x = Vec::new();
        for (c, scheme) in [(&a, 0u8), (&b, 1u8)] {
            let body = if scheme == 1 {
                let mut w = lz4_flex::frame::FrameEncoder::new(Vec::new());
                std::io::Write::write_all(&mut w, c).unwrap();
                w.finish().unwrap()
            } else {
                c.clone()
            };
            let cl = (body.len() as u32).to_le_bytes();
            let ul = (c.len() as u32).to_le_bytes();
            x.extend_from_slice(&[0, cl[0], cl[1], cl[2], scheme, ul[0], ul[1], ul[2]]);
            x.extend_from_slice(&body);
        }
        x.extend_from_slice(b"XETBLOB\x01footer...");
        let chunks = parse_xorb(&x).unwrap();
        assert_eq!(chunks, vec![a.clone(), b.clone()]);
        // Two nodes merge into one internal node.
        let hs = [(chunk_hash(&a), 1000), (chunk_hash(&b), 3000)];
        assert_eq!(xorb_hash(&hs), merged(&hs).0);
        assert_eq!(xorb_hash(&hs[..1]), hs[0].0, "a single chunk is its own xorb hash");
        assert!(parse_xorb(&x[..10]).is_err());
    }
}
