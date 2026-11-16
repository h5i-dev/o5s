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
use o5s_core::Hash;

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

/// BLAKE3 of a stream of buffers whose total length is `len`, with every
/// segment eligible for the worker threads (see [`hash_stream_with`]).
pub fn hash_stream<I: IntoIterator<Item = std::sync::Arc<Vec<u8>>>>(bufs: I, len: u64, workers: usize) -> Option<Hash> {
    hash_stream_with(bufs, len, workers, || true)
}

/// BLAKE3 of a stream of buffers whose total length is `len`. The buffers are
/// cut into [`SEGMENT`]s. When a segment starts, `offload()` decides how it is
/// hashed: on the calling thread as its bytes arrive (nothing is held), or by
/// one of up to `workers` threads, from slices of the buffers (no copy). Either
/// way the chaining values are combined in input order, so the result is always
/// `blake3::hash` of the concatenation, whatever `offload` answers. `None` if
/// the buffers do not add up to `len` bytes (the caller then reports a mismatch,
/// as the serial hash of other bytes would). The workers start on first use.
///
/// `offload` lets a reader use the extra cores only when hashing is what it is
/// waiting for, not when they would compete with decompression.
pub fn hash_stream_with<I, F>(bufs: I, len: u64, workers: usize, mut offload: F) -> Option<Hash>
where
    I: IntoIterator<Item = std::sync::Arc<Vec<u8>>>,
    F: FnMut() -> bool,
{
    if !parallel_pays(len) || workers <= 1 {
        let mut h = blake3::Hasher::new();
        let mut n = 0u64;
        for b in bufs {
            n += b.len() as u64;
            h.update(&b);
        }
        return (n == len).then(|| Hash(*h.finalize().as_bytes()));
    }
    let mut st = Stream { workers, jobs: None, pending: Default::default(), combiner: Combiner::default() };
    let mut offset = 0u64;
    let mut cur = Cur::start(0, offload());
    let mut seg_len = 0usize;
    for b in bufs {
        let mut from = 0usize;
        while from < b.len() {
            let take = (SEGMENT - seg_len).min(b.len() - from);
            match &mut cur {
                Cur::Inline(h) => {
                    h.update(&b[from..from + take]);
                }
                Cur::Collect(p) => p.push((b.clone(), from, from + take)),
            }
            seg_len += take;
            from += take;
            if seg_len == SEGMENT {
                if offset + SEGMENT as u64 > len {
                    return None; // more bytes than declared
                }
                let next = Cur::start(offset + SEGMENT as u64, offload());
                st.finish_segment(offset, std::mem::replace(&mut cur, next))?;
                seg_len = 0;
                offset += SEGMENT as u64;
            }
        }
    }
    if offset + seg_len as u64 != len {
        return None;
    }
    if seg_len > 0 {
        st.finish_segment(offset, cur)?;
    }
    st.drain(true)?;
    st.combiner.finish()
}

/// The segment being read.
enum Cur {
    Inline(Box<blake3::Hasher>),
    Collect(Pieces),
}

impl Cur {
    fn start(offset: u64, offload: bool) -> Cur {
        if offload {
            Cur::Collect(Vec::new())
        } else {
            let mut h = Box::new(blake3::Hasher::new());
            h.set_input_offset(offset);
            Cur::Inline(h)
        }
    }
}

/// A chaining value in input order: computed, or due from a worker.
enum Slot {
    Ready(ChainingValue),
    Wait(std::sync::mpsc::Receiver<ChainingValue>),
}

type Job = (u64, Pieces, std::sync::mpsc::SyncSender<ChainingValue>);

struct Stream {
    workers: usize,
    jobs: Option<std::sync::mpsc::Sender<Job>>,
    pending: std::collections::VecDeque<Slot>,
    combiner: Combiner,
}

impl Stream {
    fn finish_segment(&mut self, offset: u64, seg: Cur) -> Option<()> {
        match seg {
            Cur::Inline(h) => self.pending.push_back(Slot::Ready(h.finalize_non_root())),
            Cur::Collect(p) => self.offload(offset, p)?,
        }
        self.drain(false)
    }

    /// Hands a segment to a worker (starting them on first use), keeping at most twice the
    /// workers in flight; hashes it here if no worker can take it.
    fn offload(&mut self, offset: u64, seg: Pieces) -> Option<()> {
        if self.jobs.is_none() {
            self.jobs = start_workers(self.workers);
        }
        let Some(tx) = &self.jobs else {
            self.pending.push_back(Slot::Ready(pieces_cv(offset, &seg)));
            return Some(());
        };
        if self.pending.len() >= 2 * self.workers {
            match self.pending.pop_front()? {
                Slot::Ready(cv) => self.combiner.push(cv),
                Slot::Wait(r) => self.combiner.push(r.recv().ok()?),
            }
        }
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        match tx.send((offset, seg, reply)) {
            Ok(()) => self.pending.push_back(Slot::Wait(result)),
            Err(std::sync::mpsc::SendError((offset, seg, _))) => {
                self.pending.push_back(Slot::Ready(pieces_cv(offset, &seg)));
            }
        }
        Some(())
    }

    /// Moves finished chaining values from the front into the combiner, in order; `block`
    /// waits for each one.
    fn drain(&mut self, block: bool) -> Option<()> {
        while let Some(front) = self.pending.front() {
            let cv = match front {
                Slot::Ready(cv) => *cv,
                Slot::Wait(r) if block => r.recv().ok()?,
                Slot::Wait(r) => match r.try_recv() {
                    Ok(cv) => cv,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Some(()),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
                },
            };
            self.pending.pop_front();
            self.combiner.push(cv);
        }
        Some(())
    }
}

fn start_workers(n: usize) -> Option<std::sync::mpsc::Sender<Job>> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    let mut any = false;
    for _ in 0..n {
        let rx = rx.clone();
        let spawned = std::thread::Builder::new().name("o5s-verify-seg".into()).spawn(move || {
            loop {
                let job = match rx.lock() {
                    Ok(r) => r.recv(),
                    Err(_) => return,
                };
                let Ok((offset, seg, reply)) = job else { return };
                let _ = reply.send(pieces_cv(offset, &seg));
            }
        });
        any |= spawned.is_ok();
    }
    any.then_some(tx)
}
