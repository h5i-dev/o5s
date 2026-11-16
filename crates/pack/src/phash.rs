//! Multi-core whole-object BLAKE3, exactly equal to the serial hash.
//!
//! BLAKE3 is a tree over 1 KiB chunks. For an input of known length `L`, every
//! aligned block of [`SEGMENT`] bytes that is followed by more input is a
//! complete subtree, so its chaining value can be computed on any thread from
//! its bytes and its offset (`blake3::hazmat`). [`Combiner`] joins those
//! chaining values in input order with BLAKE3's own stack rule: equal-sized
//! siblings merge as soon as both exist, and the last piece is merged into the
//! remaining stack top-down with only the final merge as the root. The result
//! is the same 32 bytes as `blake3::hash` of the whole input (proptested
//! against it), so a verified read keeps exactly its semantics; only the work
//! is spread over cores.

use blake3::hazmat::{ChainingValue, HasherExt, Mode, merge_subtrees_non_root, merge_subtrees_root};
use pico2peta_core::Hash;

/// Segment size: a power-of-two multiple of BLAKE3's 1 KiB chunk.
pub const SEGMENT: usize = 1 << 20;

/// Inputs at most this long are hashed serially (one segment is one subtree; nothing to split).
pub fn parallel_pays(len: u64) -> bool {
    len > 2 * SEGMENT as u64
}

/// The chaining value of the segment starting at `offset` (a multiple of [`SEGMENT`]), for an
/// input that continues past it or ends inside it, but is longer than one segment overall.
pub fn segment_cv(offset: u64, bytes: &[u8]) -> ChainingValue {
    blake3::Hasher::new().set_input_offset(offset).update(bytes).finalize_non_root()
}

/// Joins segment chaining values, in input order, into the root hash.
#[derive(Default)]
pub struct Combiner {
    stack: Vec<ChainingValue>,
    /// Segments added to `stack` (all of them followed by more input).
    added: u64,
    /// The most recent segment: not merged until another one follows.
    held: Option<ChainingValue>,
}

impl Combiner {
    /// The next segment's chaining value.
    pub fn push(&mut self, cv: ChainingValue) {
        if let Some(prev) = self.held.replace(cv) {
            self.add(prev);
        }
    }

    fn add(&mut self, mut cv: ChainingValue) {
        self.added += 1;
        let mut n = self.added;
        while n & 1 == 0 {
            // Two complete equal-sized subtrees followed by more input: a complete parent.
            // One stack entry per set bit of `added - 1`, so a left sibling always exists.
            let Some(left) = self.stack.pop() else { break };
            cv = merge_subtrees_non_root(&left, &cv, Mode::Hash);
            n >>= 1;
        }
        self.stack.push(cv);
    }

    /// The root hash, once the last segment has been pushed. `None` if fewer than two segments
    /// were pushed (such inputs are hashed serially; see [`parallel_pays`]).
    pub fn finish(mut self) -> Option<Hash> {
        let mut out = self.held.take()?;
        let mut left = self.stack.pop()?;
        while let Some(next) = self.stack.pop() {
            out = merge_subtrees_non_root(&left, &out, Mode::Hash);
            left = next;
        }
        Some(Hash(*merge_subtrees_root(&left, &out, Mode::Hash).as_bytes()))
    }
}

/// BLAKE3 of `data`, using up to `threads` threads for inputs where that pays.
/// Always equal to `blake3::hash(data)`.
pub fn hash(data: &[u8], threads: usize) -> Hash {
    if !parallel_pays(data.len() as u64) || threads <= 1 {
        return Hash::of(data);
    }
    let segs: Vec<&[u8]> = data.chunks(SEGMENT).collect();
    let per = segs.len().div_ceil(threads);
    let cvs: Option<Vec<ChainingValue>> = std::thread::scope(|s| {
        let handles: Vec<_> = segs
            .chunks(per)
            .enumerate()
            .map(|(g, group)| {
                s.spawn(move || {
                    group
                        .iter()
                        .enumerate()
                        .map(|(i, seg)| segment_cv(((g * per + i) * SEGMENT) as u64, seg))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all = Vec::with_capacity(segs.len());
        for h in handles {
            all.extend(h.join().ok()?);
        }
        Some(all)
    });
    let Some(cvs) = cvs else { return Hash::of(data) };
    let mut c = Combiner::default();
    for cv in cvs {
        c.push(cv);
    }
    c.finish().unwrap_or_else(|| Hash::of(data))
}

/// One segment as slices of the stream's own buffers (no copy).
type Pieces = Vec<(std::sync::Arc<Vec<u8>>, usize, usize)>;

/// The chaining value of a segment given as pieces (see [`segment_cv`]).
fn pieces_cv(offset: u64, pieces: &Pieces) -> ChainingValue {
    let mut h = blake3::Hasher::new();
    h.set_input_offset(offset);
    for (b, from, to) in pieces {
        h.update(&b[*from..*to]);
    }
    h.finalize_non_root()
}

/// BLAKE3 of a stream of buffers whose total length is `len`, on up to
/// `workers` threads: the buffers are cut into [`SEGMENT`]s (as slices of the
/// buffers, not copies), hashed in parallel, and combined in order. `None` if
/// the buffers do not add up to `len` bytes (the caller then reports a
/// mismatch, as the serial hash of other bytes would). Always equal to
/// `blake3::hash` of the concatenation.
pub fn hash_stream<I: IntoIterator<Item = std::sync::Arc<Vec<u8>>>>(bufs: I, len: u64, workers: usize) -> Option<Hash> {
    use std::collections::VecDeque;
    use std::sync::mpsc;
    if !parallel_pays(len) || workers <= 1 {
        let mut h = blake3::Hasher::new();
        let mut n = 0u64;
        for b in bufs {
            n += b.len() as u64;
            h.update(&b);
        }
        return (n == len).then(|| Hash(*h.finalize().as_bytes()));
    }
    type Job = (u64, Pieces, mpsc::SyncSender<ChainingValue>);
    let (jobs, rx) = mpsc::channel::<Job>();
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    for _ in 0..workers {
        let rx = rx.clone();
        let spawned = std::thread::Builder::new().name("p2p-verify-seg".into()).spawn(move || {
            loop {
                let job = match rx.lock() {
                    Ok(r) => r.recv(),
                    Err(_) => return,
                };
                let Ok((offset, seg, reply)) = job else { return };
                let _ = reply.send(pieces_cv(offset, &seg));
            }
        });
        if spawned.is_err() {
            break; // fewer workers; with none, segments are hashed inline below
        }
    }
    let window = 2 * workers;
    let mut combiner = Combiner::default();
    let mut pending: VecDeque<mpsc::Receiver<ChainingValue>> = VecDeque::new();
    // Hands one segment to a worker (or hashes it here), keeping at most `window` in flight.
    let mut dispatch = |offset: u64, seg: Pieces, combiner: &mut Combiner| -> Option<()> {
        if pending.len() >= window {
            combiner.push(pending.pop_front()?.recv().ok()?);
        }
        let (reply, result) = mpsc::sync_channel(1);
        if let Err(mpsc::SendError((offset, seg, reply))) = jobs.send((offset, seg, reply)) {
            reply.send(pieces_cv(offset, &seg)).ok()?;
        }
        pending.push_back(result);
        Some(())
    };
    let mut seg: Pieces = Vec::new();
    let mut seg_len = 0usize;
    let mut offset = 0u64;
    for b in bufs {
        let mut from = 0usize;
        while from < b.len() {
            let take = (SEGMENT - seg_len).min(b.len() - from);
            seg.push((b.clone(), from, from + take));
            seg_len += take;
            from += take;
            if seg_len == SEGMENT {
                if offset + SEGMENT as u64 > len {
                    return None; // more bytes than declared
                }
                dispatch(offset, std::mem::take(&mut seg), &mut combiner)?;
                seg_len = 0;
                offset += SEGMENT as u64;
            }
        }
    }
    if offset + seg_len as u64 != len {
        return None;
    }
    if seg_len > 0 {
        dispatch(offset, seg, &mut combiner)?;
    }
    drop(dispatch);
    while let Some(r) = pending.pop_front() {
        combiner.push(r.recv().ok()?);
    }
    combiner.finish()
}
