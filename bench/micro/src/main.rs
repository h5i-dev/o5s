//! `p2p-bench <scratch-dir>`: local-repository benchmarks. Prints a Markdown
//! table. Deterministic inputs (seeded xorshift), so runs are reproducible.
//! Run with `--release`. Record results in docs/BENCH.md with the host description.

use pico2peta_core::{Name, ObjectStore, Observed, RefValue, ReplaceOutcome, Repository};
use pico2peta_repo::{LocalRepo, PackConfig};
use std::io::Read;
use std::time::Instant;

struct Gen(u64);
impl Read for Gen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // 8 bytes per xorshift step, so the generator never bottlenecks a run.
        for c in buf.chunks_mut(8) {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let b = self.0.to_le_bytes();
            c.copy_from_slice(&b[..c.len()]);
        }
        Ok(buf.len())
    }
}

fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let _ = Gen(seed.wrapping_mul(2).wrapping_add(1)).read(&mut v); // distinct odd seed per input
    v
}

fn hwm_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

/// Model-weights versioning: a synthetic safetensors-like checkpoint
/// (8-byte header length + JSON header + f16 tensors), stored in successive
/// versions. Compares repository growth to full copies (Git LFS / plain
/// object storage store every version whole).
fn weights(dir: &str, mib: usize) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(dir).join(format!("weights-{}", std::process::id()));
    let repo = LocalRepo::init(&root, &PackConfig::default())?;
    let tensors = 64usize;
    let tsize = (mib << 20) / tensors;
    let mut ts: Vec<Vec<u8>> = (0..tensors).map(|i| bytes(1000 + i as u64, tsize)).collect();
    // A real safetensors file: tensors written in `order`, named by their index in `ts`.
    let build_ordered = |ts: &Vec<Vec<u8>>, order: &[usize], meta: &str| -> Vec<u8> {
        let mut parts = Vec::new();
        let mut off = 0usize;
        for &i in order {
            let len = ts[i].len();
            parts.push(format!(
                "\"t{i}\":{{\"dtype\":\"F16\",\"shape\":[{}],\"data_offsets\":[{off},{}]}}",
                len / 2,
                off + len
            ));
            off += len;
        }
        parts.push(format!("\"__metadata__\":{{\"note\":\"{meta}\"}}"));
        let header = format!("{{{}}}", parts.join(",")).into_bytes();
        let mut v = (header.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(&header);
        for &i in order {
            v.extend_from_slice(&ts[i]);
        }
        v
    };
    let identity: Vec<usize> = (0..tensors).collect();
    let build = |ts: &Vec<Vec<u8>>| -> Vec<u8> { build_ordered(ts, &identity, "v") };
    let disk = |p: &std::path::Path| -> u64 {
        fn walk(p: &std::path::Path) -> u64 {
            std::fs::read_dir(p).map_or(0, |rd| {
                rd.flatten()
                    .map(|e| if e.path().is_dir() { walk(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
                    .sum()
            })
        }
        walk(&p.join("objects"))
    };
    println!("| version | change | logical MiB | full copies MiB | pico2peta MiB | ratio |");
    println!("|---|---|---|---|---|---|");
    let mut full = 0u64;
    let mut prev_blob: Vec<u8> = Vec::new();
    type Step<'a> = (&'a str, &'a dyn Fn(&mut Vec<Vec<u8>>));
    let steps: [Step; 5] = [
        ("v1", &|_| {}),
        ("v2: 4 of 64 tensors retrained", &|ts| {
            for (i, t) in ts.iter_mut().enumerate().take(4) {
                *t = bytes(2000 + i as u64, tsize);
            }
        }),
        ("v3: one tensor resized (+1 KiB, shifts later bytes)", &|ts| {
            ts[10].extend_from_slice(&[7u8; 1024]);
        }),
        ("v4: metadata/header edit only", &|_| {}),
        ("v5: full fine-tune (pseudo-random noise in the low 4 mantissa bits of every f16)", &|ts| {
            let mut x = 0x2545F4914F6CDD1Du64;
            for t in ts.iter_mut() {
                for b in t.iter_mut().step_by(2) {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    *b ^= (x & 0x0f) as u8; // little-endian f16: low byte = low mantissa bits
                }
            }
        }),
    ];
    let steps_len = steps.len();
    for (k, (name, f)) in steps.iter().enumerate() {
        f(&mut ts);
        let mut blob = build(&ts);
        if k == 3 {
            blob[9] ^= 1; // touch the header
        }
        full += blob.len() as u64;
        repo.put_stream(&mut &blob[..])?;
        repo.flush()?;
        if k + 1 == steps_len {
            // Also store the full fine-tune as an XOR delta against v4, in a second repo.
            let r2root = root.with_extension("delta");
            let r2 = LocalRepo::init(&r2root, &PackConfig::default())?;
            let hb = r2.put_stream(&mut &prev_blob[..])?;
            r2.flush()?;
            let base_cost = disk(&r2root);
            r2.put_delta(&hb, &mut &blob[..])?;
            r2.flush()?;
            println!(
                "| {} | ...same, stored with `put_delta` against v4 | {:.0} | – | +{:.1} (delta only) | – |",
                k + 1,
                blob.len() as f64 / 1048576.0,
                (disk(&r2root) - base_cost) as f64 / 1048576.0
            );
            std::fs::remove_dir_all(&r2root)?;
        }
        prev_blob = blob.clone();
        let used = disk(&root);
        println!(
            "| {} | {name} | {:.0} | {:.0} | {:.0} | {:.2} |",
            k + 1,
            blob.len() as f64 / 1048576.0,
            full as f64 / 1048576.0,
            used as f64 / 1048576.0,
            used as f64 / full as f64
        );
    }
    // v6: the same tensors, written in a different order with an edited header (a typical
    // re-export by another tool). Offset-aligned delta vs tensor-aligned delta (pico2peta-weights).
    let mut perm: Vec<usize> = (0..tensors).rev().collect();
    perm.swap(0, tensors / 2);
    let v6 = build_ordered(&ts, &perm, "re-exported");
    let v5 = prev_blob.clone();
    for (label, aligned) in
        [("offset-aligned `put_delta`", false), ("tensor-aligned `put --delta-*` (pico2peta-weights)", true)]
    {
        let r3root = root.with_extension(if aligned { "t" } else { "o" });
        let r3 = LocalRepo::init(&r3root, &PackConfig::default())?;
        let hb = r3.put_stream(&mut &v5[..])?;
        r3.flush()?;
        let before = disk(&r3root);
        if aligned {
            pico2peta_weights::put_checkpoint(&r3, &hb, &mut &v6[..])?;
        } else {
            r3.put_delta(&hb, &mut &v6[..])?;
        }
        r3.flush()?;
        println!(
            "| 6 | same tensors reordered + header edit, {label} | {:.0} | – | +{:.1} | – |",
            v6.len() as f64 / 1048576.0,
            (disk(&r3root) - before) as f64 / 1048576.0
        );
        std::fs::remove_dir_all(&r3root)?;
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// Deterministic code-like text (compresses roughly like source code, ratio ~0.3 at zstd-3).
struct TextGen {
    x: u64,
    pending: Vec<u8>,
    pos: usize,
}
impl Read for TextGen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        const WORDS: &[&str] = &[
            "fn", "let", "mut", "self", "return", "if", "else", "match", "for", "in", "while", "struct", "impl", "pub",
            "use", "Result", "Option", "Some", "None", "Ok", "Err", "Vec", "String", "u64", "usize", "&", "->", "=>",
            "{", "}", "(", ")", ";", ",", ".", "::", "+=", "==", "0", "1", "len", "buf", "data", "hash", "store",
            "index", "offset", "write", "read", "chunk", "manifest", "repo", "name", "value",
        ];
        // Refill in large blocks and serve by offset (no per-read shifting).
        if self.pos == self.pending.len() {
            self.pending.clear();
            self.pos = 0;
            while self.pending.len() < (1 << 20) {
                self.x ^= self.x << 13;
                self.x ^= self.x >> 7;
                self.x ^= self.x << 17;
                let w = WORDS[(self.x % WORDS.len() as u64) as usize];
                self.pending.extend_from_slice(w.as_bytes());
                self.pending.push(if self.x.is_multiple_of(11) { b'\n' } else { b' ' });
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// `p2p-bench stream <dir> <MiB>`: put_stream throughput only, on incompressible
/// and code-like input (for profiling the put pipeline).
fn stream(dir: &str, mib: usize) -> Result<(), Box<dyn std::error::Error>> {
    let total = (mib << 20) as u64;
    // Inputs are generated once into files in tmpfs (P2P_BENCH_INPUTS, default
    // /dev/shm) and streamed back, like a real `put` of a cached file: the
    // source then costs about a memcpy, doesn't count toward peak RSS, and its
    // writeback can't compete with the repo's writes.
    let base = std::env::var("P2P_BENCH_INPUTS").unwrap_or_else(|_| "/dev/shm".into());
    let inputs = std::path::Path::new(&base).join(format!("p2p-inputs-{}", std::process::id()));
    std::fs::create_dir_all(&inputs)?;
    let cfg = PackConfig {
        compress_probe: std::env::var_os("P2P_BENCH_NO_PROBE").is_none(), // ablation
        ..PackConfig::default()
    };
    println!("| input | put_stream {mib} MiB | re-put (dedup) | open+read (verified) |");
    println!("|---|---|---|---|");
    let only = std::env::var("P2P_BENCH_ONLY").ok(); // "random" or "text", for profiling
    for (label, text) in [("incompressible", false), ("code-like text", true)] {
        if only.as_deref().is_some_and(|o| o != if text { "text" } else { "random" }) {
            continue;
        }
        let input = inputs.join(if text { "text" } else { "random" });
        {
            let mut g: Box<dyn Read> = if text {
                Box::new(TextGen { x: 7, pending: Vec::new(), pos: 0 }.take(total))
            } else {
                Box::new(Gen(42).take(total))
            };
            std::io::copy(&mut g, &mut std::io::BufWriter::new(std::fs::File::create(&input)?))?;
        }
        let root = std::path::Path::new(dir).join(format!("stream-{}-{text}", std::process::id()));
        let repo = LocalRepo::init(&root, &cfg)?;
        let src = || std::fs::File::open(&input);
        let t = Instant::now();
        let h = repo.put_stream(&mut src()?)?;
        repo.flush()?;
        let put = total as f64 / t.elapsed().as_secs_f64() / 1e6;
        let t = Instant::now();
        repo.put_stream(&mut src()?)?;
        let reput = total as f64 / t.elapsed().as_secs_f64() / 1e6;
        let t = Instant::now();
        let mut r = repo.open(&h, None)?;
        let mut b = vec![0u8; 1 << 20];
        while r.read(&mut b)? > 0 {}
        let read = total as f64 / t.elapsed().as_secs_f64() / 1e6;
        println!("| {label} | {put:.0} MB/s | {reput:.0} MB/s | {read:.0} MB/s |");
        std::fs::remove_dir_all(&root)?;
        std::fs::remove_file(&input)?;
    }
    std::fs::remove_dir_all(&inputs)?;
    println!("\npeak RSS: {:.0} MB", hwm_mb());
    Ok(())
}

/// `p2p-bench chunkers <A> <B>`: chunk counts, sizes and dedup of B against A
/// for each chunker (sizes only, no timing). Also reports B' = A with 777
/// bytes inserted at 1 MiB, the classic boundary-shift case.
fn chunkers(a: &str, b: &str) -> Result<(), Box<dyn std::error::Error>> {
    use pico2peta_chunk::{Chunker, FastCdcChunker, XetChunker, chunk_lengths};
    use std::collections::HashSet;
    let a = std::fs::read(a)?;
    let b = std::fs::read(b)?;
    let mut shifted = a.clone();
    let at = (1 << 20).min(shifted.len());
    shifted.splice(at..at, std::iter::repeat_n(0xA5u8, 777));
    let ids = |data: &[u8], c: &mut dyn Chunker| -> Vec<(blake3::Hash, usize)> {
        let mut off = 0;
        chunk_lengths(c, data)
            .into_iter()
            .map(|l| {
                let h = blake3::hash(&data[off..off + l]);
                off += l;
                (h, l)
            })
            .collect()
    };
    println!(
        "| chunker | chunks in A | mean / max chunk | B bytes deduped vs A | A+777 B bytes deduped vs A | index+manifest overhead for A |"
    );
    println!("|---|---|---|---|---|---|");
    let mut fast = FastCdcChunker::pack_default();
    let mut xet = XetChunker::new();
    for (name, c) in [("fastcdc 16/64/256K", &mut fast as &mut dyn Chunker), ("xet 8/64/128K", &mut xet)] {
        let ca = ids(&a, c);
        let have: HashSet<_> = ca.iter().map(|(h, _)| *h).collect();
        let dedup = |d: &[u8], c: &mut dyn Chunker| -> f64 {
            let v = ids(d, c);
            let hit: usize = v.iter().filter(|(h, _)| have.contains(h)).map(|(_, l)| l).sum();
            100.0 * hit as f64 / d.len() as f64
        };
        let db = dedup(&b, c);
        let ds = dedup(&shifted, c);
        let max = ca.iter().map(|(_, l)| *l).max().unwrap_or(0);
        // 44 B per index entry + 40 B per manifest entry, per chunk.
        let over = 84 * ca.len();
        println!(
            "| {name} | {} | {:.1} KiB / {} KiB | {db:.2}% | {ds:.2}% | {:.1} KiB ({:.3}%) |",
            ca.len(),
            a.len() as f64 / ca.len() as f64 / 1024.0,
            max / 1024,
            over as f64 / 1024.0,
            100.0 * over as f64 / a.len() as f64
        );
    }
    Ok(())
}

/// `p2p-bench bytegroup-gran <safetensors>`: zstd-3 size of the tensor data
/// in 64 KiB blocks, (a) plain, (b) byte-grouped per block (what the
/// GroupedZstd record codec does per chunk), (c) grouped per whole tensor and
/// then cut into blocks (planes span the tensor; not record-local).
fn bytegroup_gran(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    use pico2peta_weights::bytegroup::group;
    let f = std::fs::read(path)?;
    let w = pico2peta_weights::group_width(&f).ok_or("not a groupable safetensors file")? as usize;
    let layout = pico2peta_weights::parse_layout(&f).ok_or("bad header")?;
    const B: usize = 64 << 10;
    let z = |b: &[u8]| zstd::bulk::compress(b, 3).map_or(b.len(), |c| c.len().min(b.len()));
    let (mut raw, mut a, mut b, mut c) = (0usize, 0usize, 0usize, 0usize);
    let mut ranges: Vec<_> = layout.tensors.values().copied().collect();
    ranges.sort();
    for (s, e) in ranges {
        let t = &f[s as usize..e as usize];
        raw += t.len();
        for blk in t.chunks(B) {
            a += z(blk);
            b += z(&group(blk, w)?);
        }
        for blk in group(t, w)?.chunks(B) {
            c += z(blk);
        }
    }
    let pct = |x: usize| (1.0 - x as f64 / a as f64) * 100.0;
    println!("tensor data {:.1} MB, width {w}", raw as f64 / 1e6);
    println!("| layout (zstd-3, 64 KiB blocks) | bytes | vs plain |");
    println!("|---|---|---|");
    println!("| plain | {a} | |");
    println!("| grouped per block (record-local; the codec) | {b} | {:+.1}% |", -pct(b));
    println!("| grouped per tensor, then blocks (not record-local) | {c} | {:+.1}% |", -pct(c));
    Ok(())
}

/// `p2p-bench group-levels <safetensors> [MiB]`: per-64 KiB-block grouped zstd
/// size and single-thread compress speed by level, over the first MiB of tensor
/// data (what codec 2 stores at each `pack.zstd_level`).
fn group_levels(path: &str, mib: usize) -> Result<(), Box<dyn std::error::Error>> {
    use pico2peta_weights::bytegroup::group;
    let f = std::fs::read(path)?;
    let w = pico2peta_weights::group_width(&f).ok_or("not a groupable safetensors file")? as usize;
    let layout = pico2peta_weights::parse_layout(&f).ok_or("bad header")?;
    let start = layout.data_start as usize;
    let data = &f[start..(start + (mib << 20)).min(f.len())];
    let blocks: Vec<Vec<u8>> = data.chunks(64 << 10).map(|b| group(b, w)).collect::<Result<_, _>>()?;
    let plain_blocks: Vec<&[u8]> = data.chunks(64 << 10).collect();
    println!("{:.0} MB of tensor data, width {w}, 64 KiB blocks, 1 thread", data.len() as f64 / 1e6);
    println!("| zstd level | plain | grouped | grouped vs plain-L3 | grouped MB/s (1 core) |");
    println!("|---|---|---|---|---|");
    let size = |bs: &[&[u8]], level: i32| -> usize {
        bs.iter().map(|b| zstd::bulk::compress(b, level).map_or(b.len(), |c| c.len().min(b.len()))).sum()
    };
    let gref: Vec<&[u8]> = blocks.iter().map(|b| b.as_slice()).collect();
    let p3 = size(&plain_blocks, 3);
    for level in [1, 3, 6, 9, 12, 15, 19] {
        let p = size(&plain_blocks, level);
        let t = Instant::now();
        let g = size(&gref, level);
        let secs = t.elapsed().as_secs_f64();
        println!(
            "| {level} | {:.3} | {:.3} | {:+.1}% | {:.0} |",
            p as f64 / data.len() as f64,
            g as f64 / data.len() as f64,
            (g as f64 / p3 as f64 - 1.0) * 100.0,
            data.len() as f64 / secs / 1e6
        );
    }
    Ok(())
}

/// `p2p-bench group-align <safetensors> <level>`: grouped zstd size of the
/// whole file under three cuttings: FastCDC over the whole file (what the
/// store does today), FastCDC restarted inside each tensor (tensor-aligned
/// CDC), and tensor-aligned fixed 64 KiB blocks. Header and gaps are counted
/// plain in the tensor-aligned cases.
fn group_align(path: &str, level: i32) -> Result<(), Box<dyn std::error::Error>> {
    use pico2peta_chunk::{FastCdcChunker, chunk_lengths};
    use pico2peta_weights::bytegroup::group;
    let f = std::fs::read(path)?;
    let w = pico2peta_weights::group_width(&f).ok_or("not a groupable safetensors file")? as usize;
    let layout = pico2peta_weights::parse_layout(&f).ok_or("bad header")?;
    let z = |b: &[u8], g: bool| -> usize {
        let src = if g { group(b, w).unwrap_or_else(|_| b.to_vec()) } else { b.to_vec() };
        zstd::bulk::compress(&src, level).map_or(b.len(), |c| c.len().min(b.len()))
    };
    let cut = |data: &[u8], lens: Vec<usize>, g: bool| -> (usize, usize) {
        let mut off = 0;
        let mut tot = 0;
        for l in &lens {
            tot += z(&data[off..off + l], g);
            off += l;
        }
        (tot, lens.len())
    };
    // Regions in file order: header, tensors, gaps.
    let mut ranges: Vec<(usize, usize)> = layout.tensors.values().map(|&(a, b)| (a as usize, b as usize)).collect();
    ranges.sort();
    let mut regions: Vec<(usize, usize, bool)> = vec![(0, layout.data_start as usize, false)];
    let mut pos = layout.data_start as usize;
    for (a, b) in ranges {
        if a > pos {
            regions.push((pos, a, false));
        }
        if b > a.max(pos) {
            regions.push((a.max(pos), b, true));
            pos = b;
        }
    }
    if pos < f.len() {
        regions.push((pos, f.len(), false));
    }
    let (plain_whole, n_plain) = cut(&f, chunk_lengths(&mut FastCdcChunker::pack_default(), &f), false);
    let (whole, n_whole) = cut(&f, chunk_lengths(&mut FastCdcChunker::pack_default(), &f), true);
    let (mut cdc_t, mut n_cdc_t, mut blk_t, mut n_blk_t) = (0, 0, 0, 0);
    for &(a, b, tensor) in &regions {
        let r = &f[a..b];
        let (t, n) = cut(r, chunk_lengths(&mut FastCdcChunker::pack_default(), r), tensor);
        cdc_t += t;
        n_cdc_t += n;
        let lens: Vec<usize> = r.chunks(64 << 10).map(|c| c.len()).collect();
        let (t, n) = cut(r, lens, tensor);
        blk_t += t;
        n_blk_t += n;
    }
    let pct = |x: usize| (x as f64 / plain_whole as f64 - 1.0) * 100.0;
    println!("{} ({:.1} MB), width {w}, zstd level {level}", path, f.len() as f64 / 1e6);
    println!("| cutting | chunks | bytes | vs plain whole-file CDC |");
    println!("|---|---|---|---|");
    println!("| plain, whole-file FastCDC | {n_plain} | {plain_whole} | |");
    println!("| grouped, whole-file FastCDC (store today) | {n_whole} | {whole} | {:+.1}% |", pct(whole));
    println!("| grouped, FastCDC inside each tensor | {n_cdc_t} | {cdc_t} | {:+.1}% |", pct(cdc_t));
    println!("| grouped, tensor-aligned 64 KiB blocks | {n_blk_t} | {blk_t} | {:+.1}% |", pct(blk_t));
    Ok(())
}

/// `p2p-bench small <dir> [n]`: n × 1 KiB distinct puts then one flush, and n
/// random gets (warm), single thread, plus 4-thread puts and gets.
fn small(dir: &str, n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(dir).join(format!("small-{}", std::process::id()));
    let repo = LocalRepo::init(&root, &PackConfig::default())?;
    let objs: Vec<Vec<u8>> = (0..n).map(|i| bytes(i as u64 + 1, 1024)).collect();
    let t = Instant::now();
    let hashes: Vec<_> = objs.iter().map(|o| repo.put(o)).collect::<Result<_, _>>()?;
    repo.flush()?;
    let put1 = n as f64 / t.elapsed().as_secs_f64();
    let mut x = 88172645463325252u64;
    let t = Instant::now();
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let h = &hashes[(x % n as u64) as usize];
        let _ = repo.get(h, None)?;
    }
    let get1 = n as f64 / t.elapsed().as_secs_f64();
    // Re-put of already-stored objects (all dedup hits), then ensure_live on each:
    // what a front does when the same content is pushed again.
    let t = Instant::now();
    for o in &objs {
        repo.put(o)?;
    }
    repo.flush()?;
    let reput = n as f64 / t.elapsed().as_secs_f64();
    let t = Instant::now();
    for h in &hashes {
        repo.ensure_live(h)?;
    }
    let live = n as f64 / t.elapsed().as_secs_f64();
    println!("| re-put (all dedup) | {:.1}k/s | |", reput / 1e3);
    println!("| ensure_live | {:.1}k/s | |", live / 1e3);
    // 4 threads: distinct objects per thread.
    let objs4: Vec<Vec<u8>> = (0..n).map(|i| bytes(10_000_000 + i as u64, 1024)).collect();
    let t = Instant::now();
    std::thread::scope(|sc| {
        for part in objs4.chunks(n.div_ceil(4)) {
            let repo = &repo;
            sc.spawn(move || part.iter().for_each(|o| drop(repo.put(o))));
        }
    });
    repo.flush()?;
    let put4 = n as f64 / t.elapsed().as_secs_f64();
    let t = Instant::now();
    std::thread::scope(|sc| {
        for k in 0..4u64 {
            let (repo, hashes) = (&repo, &hashes);
            sc.spawn(move || {
                let mut x = 0x9e3779b97f4a7c15u64 ^ k;
                for _ in 0..n / 4 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let _ = repo.get(&hashes[(x % n as u64) as usize], None);
                }
            });
        }
    });
    let get4 = n as f64 / t.elapsed().as_secs_f64();
    println!("| 1 KiB objects (n = {n}) | 1 thread | 4 threads |");
    println!("|---|---|---|");
    println!("| put (+ flush) | {:.1}k/s | {:.1}k/s |", put1 / 1e3, put4 / 1e3);
    println!("| get (warm, random) | {:.1}k/s | {:.1}k/s |", get1 / 1e3, get4 / 1e3);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// `p2p-bench commits <dir> [n]`: durable `replace` throughput on one repository
/// handle, with 1, 4 and 8 threads committing to distinct names (group commit
/// batches concurrent commits of one process into shared fsyncs).
fn commits(dir: &str, n: usize) -> Result<(), Box<dyn std::error::Error>> {
    println!("| threads (one handle, distinct names) | commits/s |");
    println!("|---|---|");
    for threads in [1usize, 4, 8] {
        let root = std::path::Path::new(dir).join(format!("commits-{}-{threads}", std::process::id()));
        let repo = LocalRepo::init(&root, &PackConfig::default())?;
        let h = repo.put(b"target")?;
        let t = Instant::now();
        std::thread::scope(|sc| {
            for k in 0..threads {
                let repo = &repo;
                sc.spawn(move || {
                    for i in 0..n / threads {
                        let name = Name::new(format!("b/{k}/{i}")).unwrap_or_else(|_| unreachable!());
                        let _ = repo.replace(&name, &[(name.clone(), Observed::Absent)], h);
                    }
                });
            }
        });
        let rate = n as f64 / t.elapsed().as_secs_f64();
        println!("| {threads} | {rate:.0} |");
        std::fs::remove_dir_all(&root)?;
    }
    Ok(())
}

/// `p2p-bench generic <dir> [MiB]`: the generic front's pre-registered metrics
/// (PLAN §6.4), each with its baseline measured here on the same volume:
/// durable write vs `dd conv=fsync`, verified read vs `cat`, 4 KiB range-read
/// p99, and storage of 10 versions (scattered 0.1 % edits; 1 MiB insertions).
/// Hold the bench lock. Prints one JSON line.
fn generic(dir: &str, mib: usize) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;
    let size = (mib as u64) << 20;
    let base = std::path::Path::new(dir).join(format!("generic-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    // Input: incompressible, generated into tmpfs (so the source isn't the bottleneck).
    let input = std::path::Path::new("/dev/shm").join(format!("p2p-generic-{}", std::process::id()));
    std::io::copy(&mut Gen(77).take(size), &mut std::io::BufWriter::new(std::fs::File::create(&input)?))?;
    let secs =
        |f: &mut dyn FnMut() -> Result<(), Box<dyn std::error::Error>>| -> Result<f64, Box<dyn std::error::Error>> {
            let t = Instant::now();
            f()?;
            Ok(t.elapsed().as_secs_f64())
        };
    // Baseline write: dd with fsync on the same volume.
    let ddout = base.join("dd.bin");
    let dd_s = secs(&mut || {
        let st = Command::new("dd")
            .arg(format!("if={}", input.display()))
            .arg(format!("of={}", ddout.display()))
            .args(["bs=1M", "conv=fsync", "status=none"])
            .status()?;
        if !st.success() {
            return Err("dd failed".into());
        }
        Ok(())
    })?;
    // Baseline read: cat (warm).
    let _ = std::fs::read(&ddout)?;
    let cat_s = secs(&mut || {
        let st = Command::new("sh").arg("-c").arg(format!("cat '{}' > /dev/null", ddout.display())).status()?;
        if !st.success() {
            return Err("cat failed".into());
        }
        Ok(())
    })?;
    std::fs::remove_file(&ddout)?;
    // Store: durable write (put_stream + flush) and verified read (warm).
    // Both read modes are measured (PLAN §6.4's read row), so eof reads are allowed here.
    let mut rcfg = pico2peta_repo::RepoConfig::new(&PackConfig::default());
    rcfg.read.allow_eof = true;
    let repo = LocalRepo::init_config(base.join("repo"), &rcfg)?;
    let mut h = None;
    let put_s = secs(&mut || {
        h = Some(repo.put_stream(&mut std::fs::File::open(&input)?)?);
        repo.flush()?;
        Ok(())
    })?;
    let h = h.ok_or("no hash")?;
    let mut sink = Vec::new();
    repo.open(&h, None)?.read_to_end(&mut sink)?; // warm
    drop(sink);
    let read_with = |mode: pico2peta_core::ReadVerify| -> Result<f64, Box<dyn std::error::Error>> {
        let t = Instant::now();
        let mut r = repo.open_verified(&h, None, mode)?;
        let mut b = vec![0u8; 1 << 20];
        while r.read(&mut b)? > 0 {}
        Ok(t.elapsed().as_secs_f64())
    };
    let get_s = read_with(pico2peta_core::ReadVerify::Strict)?;
    let get_eof_s = read_with(pico2peta_core::ReadVerify::Eof)?;
    // 4 KiB range reads, warm, uniform offsets.
    let mut lat = Vec::new();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..2000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let off = x % (size - 4096);
        let t = Instant::now();
        let _ = repo.get(&h, Some(off..off + 4096))?;
        lat.push(t.elapsed().as_secs_f64() * 1e3);
    }
    lat.sort_by(|a, b| a.total_cmp(b));
    let p99 = lat[(lat.len() as f64 * 0.99) as usize];
    // Storage: 10 versions, each derived from the previous one.
    let disk = |p: &std::path::Path| -> u64 {
        fn walk(p: &std::path::Path) -> u64 {
            std::fs::read_dir(p).map_or(0, |rd| {
                rd.flatten()
                    .map(|e| if e.path().is_dir() { walk(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
                    .sum()
            })
        }
        walk(p)
    };
    let versions = |name: &str, edit: &dyn Fn(&mut Vec<u8>, u64)| -> Result<u64, Box<dyn std::error::Error>> {
        let root = base.join(name);
        // The generic front's profile: new versions derive from the previous one
        // (chunk-anchored XOR), for any data format.
        let mut cfg = pico2peta_repo::RepoConfig::new(&PackConfig::default());
        cfg.profiles.insert(
            "generic".into(),
            pico2peta_repo::Profile { derive: pico2peta_repo::DerivePolicy::Previous, ..Default::default() },
        );
        cfg.namespaces.insert("data/".into(), "generic".into());
        let r = LocalRepo::init_config(&root, &cfg)?;
        let n = Name::new(format!("data/{name}"))?;
        let mut cur = std::fs::read(&input)?;
        for v in 0..10u64 {
            if v > 0 {
                edit(&mut cur, v);
            }
            let hv = r.put_named(&n, &mut &cur[..])?;
            let obs = RefValue::observed(r.resolve(&n)?);
            r.replace(&n, &[(n.clone(), obs)], hv)?;
            if std::env::var_os("P2P_BENCH_VERBOSE").is_some() {
                r.flush()?;
                eprintln!("{name} v{v}: {} bytes stored so far", disk(&root.join("objects")));
            }
        }
        r.flush()?;
        Ok(disk(&root.join("objects")))
    };
    let scattered = versions("scattered", &|b, v| {
        let mut y = 0x9E37_79B9_7F4A_7C15u64 ^ v;
        for _ in 0..b.len() / 1000 {
            y ^= y << 13;
            y ^= y >> 7;
            y ^= y << 17;
            let i = (y % b.len() as u64) as usize;
            b[i] = b[i].wrapping_add(1);
        }
    })?;
    let inserted = versions("inserted", &|b, v| {
        let at = ((v.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) % b.len() as u64) as usize;
        let ins: Vec<u8> = (0..1 << 20).map(|i: u32| (i.wrapping_mul(2654435761) >> 13) as u8 ^ v as u8).collect();
        b.splice(at..at, ins);
    })?;
    let mb = |s: f64| size as f64 / s / 1e6;
    println!(
        "{{\"mib\":{mib},\"dd_fsync_mb_s\":{:.0},\"put_mb_s\":{:.0},\"write_ratio\":{:.3},\"cat_mb_s\":{:.0},\"get_strict_mb_s\":{:.0},\"read_ratio_strict\":{:.3},\"get_eof_mb_s\":{:.0},\"read_ratio_eof\":{:.3},\"range4k_p99_ms\":{p99:.3},\"scattered_10v_bytes\":{scattered},\"inserted_10v_bytes\":{inserted},\"one_copy_bytes\":{size}}}",
        mb(dd_s),
        mb(put_s),
        dd_s / put_s,
        mb(cat_s),
        mb(get_s),
        cat_s / get_s,
        mb(get_eof_s),
        cat_s / get_eof_s,
    );
    std::fs::remove_dir_all(&base)?;
    std::fs::remove_file(&input)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("generic") {
        let dir = std::env::args().nth(2).ok_or("usage: p2p-bench generic <dir> [MiB]")?;
        let mib = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1024);
        return generic(&dir, mib);
    }
    if std::env::args().nth(1).as_deref() == Some("commits") {
        let dir = std::env::args().nth(2).ok_or("usage: p2p-bench commits <dir> [n]")?;
        let n = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(20_000);
        return commits(&dir, n);
    }
    if std::env::args().nth(1).as_deref() == Some("small") {
        let dir = std::env::args().nth(2).ok_or("usage: p2p-bench small <dir> [n]")?;
        let n = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(100_000);
        return small(&dir, n);
    }
    if std::env::args().nth(1).as_deref() == Some("group-align") {
        let f = std::env::args().nth(2).ok_or("usage: p2p-bench group-align <safetensors> <level>")?;
        let level = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(6);
        return group_align(&f, level);
    }
    if std::env::args().nth(1).as_deref() == Some("group-levels") {
        let f = std::env::args().nth(2).ok_or("usage: p2p-bench group-levels <safetensors> [MiB]")?;
        let mib = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(64);
        return group_levels(&f, mib);
    }
    if std::env::args().nth(1).as_deref() == Some("bytegroup-gran") {
        let f = std::env::args().nth(2).ok_or("usage: p2p-bench bytegroup-gran <safetensors>")?;
        return bytegroup_gran(&f);
    }
    if std::env::args().nth(1).as_deref() == Some("chunkers") {
        let a = std::env::args().nth(2).ok_or("usage: p2p-bench chunkers <A> <B>")?;
        let b = std::env::args().nth(3).ok_or("usage: p2p-bench chunkers <A> <B>")?;
        return chunkers(&a, &b);
    }
    if std::env::args().nth(1).as_deref() == Some("stream") {
        let dir = std::env::args().nth(2).ok_or("usage: p2p-bench stream <dir> [MiB]")?;
        let mib = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1024);
        return stream(&dir, mib);
    }
    if std::env::args().nth(1).as_deref() == Some("weights") {
        let dir = std::env::args().nth(2).ok_or("usage: p2p-bench weights <scratch-dir> [MiB]")?;
        let mib = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(256);
        return weights(&dir, mib);
    }
    let dir = std::env::args().nth(1).ok_or("usage: p2p-bench <scratch-dir> [large-MiB] | weights <dir> [MiB]")?;
    let large_mib: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1024);
    let root = std::path::Path::new(&dir).join(format!("bench-{}", std::process::id()));
    let repo = LocalRepo::init(&root, &PackConfig::default())?;
    let mut rows: Vec<(String, String)> = Vec::new();

    // 1. Small objects: 100k × 1 KiB (random, incompressible), then flush.
    let n = 100_000;
    let objs: Vec<Vec<u8>> = (0..n).map(|i| bytes(i as u64 + 1, 1024)).collect();
    let t = Instant::now();
    let hashes: Vec<_> = objs.iter().map(|o| repo.put(o)).collect::<Result<_, _>>()?;
    repo.flush()?;
    let s = t.elapsed().as_secs_f64();
    rows.push((
        "put 1 KiB objects (incl. flush)".into(),
        format!("{:.0} obj/s, {:.1} MB/s", n as f64 / s, n as f64 * 1024.0 / s / 1e6),
    ));

    // 2. Random small reads (warm).
    let t = Instant::now();
    let mut lat = Vec::with_capacity(20_000);
    for i in 0..20_000usize {
        let h = &hashes[(i * 7919) % n];
        let t1 = Instant::now();
        let d = repo.get(h, None)?;
        lat.push(t1.elapsed().as_secs_f64() * 1e6);
        assert_eq!(d.len(), 1024);
    }
    let s = t.elapsed().as_secs_f64();
    lat.sort_by(|a, b| a.total_cmp(b));
    rows.push((
        "get 1 KiB random (warm)".into(),
        format!("{:.0} obj/s, p50 {:.0} µs, p99 {:.0} µs", 20_000.0 / s, lat[10_000], lat[19_800]),
    ));

    // 3. Large streaming put: random data (incompressible, no dedup).
    let total = large_mib << 20;
    let t = Instant::now();
    let big = repo.put_stream(&mut Gen(42).take(total as u64))?;
    repo.flush()?;
    let s = t.elapsed().as_secs_f64();
    rows.push((
        format!("put_stream {large_mib} MiB"),
        format!("{:.0} MB/s (peak RSS so far {:.0} MB)", total as f64 / s / 1e6, hwm_mb()),
    ));

    // 4. Large streaming get with hash verification.
    let t = Instant::now();
    let mut r = repo.open(&big, None)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0usize;
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        got += k;
    }
    let s = t.elapsed().as_secs_f64();
    assert_eq!(got, total);
    rows.push((format!("open+read {large_mib} MiB (verified)"), format!("{:.0} MB/s", total as f64 / s / 1e6)));

    // 5. Ranged read of 4 KiB from the large object.
    let t = Instant::now();
    for i in 0..2000u64 {
        let off = (i * 104_729 * 4096) % (total as u64 - 4096);
        repo.get(&big, Some(off..off + 4096))?;
    }
    rows.push((
        "4 KiB range read in large object".into(),
        format!("{:.0} µs avg", t.elapsed().as_secs_f64() / 2000.0 * 1e6),
    ));

    // 6. Dedup: re-put the same large stream.
    let t = Instant::now();
    repo.put_stream(&mut Gen(42).take(total as u64))?;
    let s = t.elapsed().as_secs_f64();
    rows.push((format!("re-put identical {large_mib} MiB (dedup)"), format!("{:.0} MB/s", total as f64 / s / 1e6)));

    // 7. Uncontended replace (each durable: fsync).
    let name = Name::new("bench/counter")?;
    let t = Instant::now();
    let k = 2000;
    for i in 0..k {
        let obs = RefValue::observed(repo.resolve(&name)?);
        match repo.replace(&name, &[(name.clone(), obs)], hashes[i % n])? {
            ReplaceOutcome::Ok { .. } => {}
            ReplaceOutcome::Conflict { .. } => return Err("unexpected conflict".into()),
        }
    }
    let s = t.elapsed().as_secs_f64();
    rows.push(("replace, uncontended (durable)".into(), format!("{:.0} commits/s", k as f64 / s)));

    // 8. Contended replace: 4 handles on one ref.
    let t = Instant::now();
    let per = 300;
    let (wins, conflicts) = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|th| {
                let root = root.clone();
                let name = name.clone();
                let hs = &hashes;
                sc.spawn(move || -> (u64, u64) {
                    let Ok(r) = LocalRepo::open(&root) else { return (0, 0) };
                    let (mut w, mut c) = (0, 0);
                    while w < per {
                        let Ok(v) = r.resolve(&name) else { return (w, c) };
                        match r.replace(
                            &name,
                            &[(name.clone(), RefValue::observed(v))],
                            hs[(th * 1000 + w as usize) % n],
                        ) {
                            Ok(ReplaceOutcome::Ok { .. }) => w += 1,
                            Ok(ReplaceOutcome::Conflict { .. }) => c += 1,
                            Err(_) => return (w, c),
                        }
                    }
                    (w, c)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or((0, 0))).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    });
    let s = t.elapsed().as_secs_f64();
    rows.push((
        "replace, 4 contending handles".into(),
        format!(
            "{:.0} commits/s, {:.1}% conflict rate",
            wins as f64 / s,
            100.0 * conflicts as f64 / (wins + conflicts) as f64
        ),
    ));
    let _ = Observed::Absent;

    println!("| benchmark | result |\n|---|---|");
    for (a, b) in rows {
        println!("| {a} | {b} |");
    }
    println!("\npeak RSS: {:.0} MB", hwm_mb());
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
