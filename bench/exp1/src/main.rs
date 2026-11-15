//! Experiment 1 harness: similarity-grouped zstd vs `git gc --aggressive`.
//! The protocol is pre-registered in docs/PLAN.md §4. This tool implements it.
//!
//! ```text
//! exp1 extract <git-dir> <work-dir>      # object set → partitioned spill + meta
//! exp1 run <work-dir> <v0|v1|v2|v3> [threads] [samples]   # → one JSON line on stdout
//! ```
//! `extract` writes every object reachable from all refs, in its canonical Git
//! form (`"<type> <size>\0<body>"`), already in partition order. Each partition
//! goes into `spill.bin` as one fast zstd stream (level 3, 64 MiB window), so
//! the spill fits on scratch. `run` streams the spill back. The spill decode
//! time is inside the measured build time, which works against us.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const FRAME_CAP: usize = 4 << 20; // V2 frame size (pre-registered: 4 MiB)
const LEVEL: i32 = 19; // pre-registered
const DICT_MAX: usize = 112 << 10; // pre-registered cap
const INDEX_BYTES_PER_OBJECT: u64 = 48; // pre-registered model
const SPILL_WINDOW_LOG: u32 = 26;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let res = match args.get(1).map(|s| s.as_str()) {
        Some("extract") if args.len() == 4 => extract(Path::new(&args[2]), Path::new(&args[3])),
        Some("run") if args.len() >= 4 => {
            let threads = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);
            let samples = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            run(Path::new(&args[2]), &args[3], threads, samples)
        }
        _ => {
            eprintln!(
                "usage: exp1 extract <git-dir> <work-dir> | exp1 run <work-dir> <v0|v1|v2|v3> [threads] [samples]"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("exp1: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

struct Obj {
    sha: String,
    typ: String,
    size: u64,
    path: String,
    /// Position in `git rev-list --objects --all` (newest first).
    rev_pos: usize,
}

fn partition_key(o: &Obj) -> (String, String) {
    if o.typ != "blob" {
        return (o.typ.clone(), String::new());
    }
    let base = o.path.rsplit('/').next().unwrap_or("");
    let ext = match base.rfind('.') {
        Some(i) if i > 0 => base[i + 1..].to_ascii_lowercase(),
        // No extension: group by basename (Makefile, README, ...).
        _ => format!("<{base}>"),
    };
    ("blob".into(), ext)
}

fn extract(git_dir: &Path, out: &Path) -> R<()> {
    std::fs::create_dir_all(out)?;
    let t0 = Instant::now();
    // 1. Reachable object set with paths, in rev-list order.
    let rl = Command::new("git").arg("--git-dir").arg(git_dir).args(["rev-list", "--objects", "--all"]).output()?;
    if !rl.status.success() {
        return Err(format!("rev-list failed: {}", String::from_utf8_lossy(&rl.stderr)).into());
    }
    let mut objs: Vec<Obj> = Vec::new();
    for (i, line) in rl.stdout.split(|b| *b == b'\n').enumerate() {
        if line.len() < 40 {
            continue;
        }
        let s = String::from_utf8_lossy(line);
        let (sha, path) = match s.split_once(' ') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (s.to_string(), String::new()),
        };
        objs.push(Obj { sha, typ: String::new(), size: 0, path, rev_pos: i });
    }
    // 2. Types and sizes.
    let mut check = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = check.stdin.take().ok_or("no stdin")?;
    let shas: Vec<String> = objs.iter().map(|o| o.sha.clone()).collect();
    let feeder = std::thread::spawn(move || -> std::io::Result<()> {
        let mut w = BufWriter::new(&mut stdin);
        for s in shas {
            writeln!(w, "{s}")?;
        }
        w.flush()
    });
    let rd = BufReader::new(check.stdout.take().ok_or("no stdout")?);
    for (o, line) in objs.iter_mut().zip(rd.lines()) {
        let line = line?;
        let mut it = line.split(' ');
        let (sha, typ, size) = (it.next(), it.next(), it.next());
        if sha != Some(o.sha.as_str()) {
            return Err(format!("batch-check out of sync at {}", o.sha).into());
        }
        o.typ = typ.ok_or("no type")?.to_string();
        o.size = size.ok_or("no size")?.parse()?;
    }
    feeder.join().map_err(|_| "feeder panicked")??;
    check.wait()?;

    // 3. Partition and order: (path, oldest first). Commits and tags: oldest first.
    let mut parts: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, o) in objs.iter().enumerate() {
        parts.entry(partition_key(o)).or_default().push(i);
    }
    let mut parts: Vec<((String, String), Vec<usize>)> = parts.into_iter().collect();
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, v) in parts.iter_mut() {
        v.sort_by(|&a, &b| {
            let (x, y) = (&objs[a], &objs[b]);
            x.path.cmp(&y.path).then(y.rev_pos.cmp(&x.rev_pos))
        });
    }

    // 4. Stream bodies in final order through `git cat-file --batch` into the spill.
    let order: Vec<String> = parts.iter().flat_map(|(_, v)| v.iter().map(|&i| objs[i].sha.clone())).collect();
    let mut cat = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = cat.stdin.take().ok_or("no stdin")?;
    let feeder = std::thread::spawn(move || -> std::io::Result<()> {
        let mut w = BufWriter::new(&mut stdin);
        for s in order {
            writeln!(w, "{s}")?;
        }
        w.flush()
    });
    let mut rd = BufReader::with_capacity(1 << 20, cat.stdout.take().ok_or("no stdout")?);
    let mut spill = BufWriter::new(File::create(out.join("spill.bin"))?);
    let mut meta = BufWriter::new(File::create(out.join("meta.tsv"))?);
    let mut spill_off = 0u64;
    let mut total_raw = 0u64;
    let mut hdr = String::new();
    for (pid, (key, members)) in parts.iter().enumerate() {
        let mut buf: Vec<u8> = Vec::new();
        let mut enc = zstd::stream::Encoder::new(&mut buf, 3)?;
        enc.window_log(SPILL_WINDOW_LOG)?;
        enc.long_distance_matching(true)?;
        let mut raw = 0u64;
        let mut lines = Vec::with_capacity(members.len());
        for &i in members {
            let o = &objs[i];
            hdr.clear();
            rd.read_line(&mut hdr)?;
            let mut it = hdr.trim_end().split(' ');
            if it.next() != Some(o.sha.as_str()) {
                return Err(format!("cat-file out of sync at {}: {hdr:?}", o.sha).into());
            }
            let mut body = vec![0u8; o.size as usize];
            rd.read_exact(&mut body)?;
            let mut nl = [0u8; 1];
            rd.read_exact(&mut nl)?;
            let canon_hdr = format!("{} {}\0", o.typ, o.size);
            enc.write_all(canon_hdr.as_bytes())?;
            enc.write_all(&body)?;
            let len = canon_hdr.len() as u64 + o.size;
            raw += len;
            let path = o.path.replace(['\t', '\n'], " ");
            lines.push(format!("O\t{pid}\t{}\t{len}\t{path}\n", o.sha));
        }
        enc.finish()?;
        spill.write_all(&buf)?;
        writeln!(meta, "P\t{pid}\t{}\t{}\t{spill_off}\t{}\t{}\t{raw}", key.0, key.1, buf.len(), members.len())?;
        for l in lines {
            meta.write_all(l.as_bytes())?;
        }
        spill_off += buf.len() as u64;
        total_raw += raw;
    }
    feeder.join().map_err(|_| "feeder panicked")??;
    cat.wait()?;
    spill.flush()?;
    meta.flush()?;
    eprintln!(
        "extracted {} objects in {} partitions, {:.1} MB canonical, spill {:.1} MB, {:.0}s",
        objs.len(),
        parts.len(),
        total_raw as f64 / 1e6,
        spill_off as f64 / 1e6,
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

struct Part {
    typ: String,
    key: String,
    spill_off: u64,
    spill_len: u64,
    raw: u64,
    objs: Vec<(String, u64)>, // (sha, canonical len)
    /// Path of each object (empty for commits/tags; absent in old metas).
    paths: Vec<String>,
}

fn load_meta(dir: &Path) -> R<Vec<Part>> {
    let mut parts: Vec<Part> = Vec::new();
    for line in BufReader::new(File::open(dir.join("meta.tsv"))?).lines() {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "P" => parts.push(Part {
                typ: f[2].to_string(),
                key: f[3].to_string(),
                spill_off: f[4].parse()?,
                spill_len: f[5].parse()?,
                raw: f[7].parse()?,
                objs: Vec::new(),
                paths: Vec::new(),
            }),
            "O" => {
                let p = parts.last_mut().ok_or("O before P")?;
                p.objs.push((f[2].to_string(), f[3].parse()?));
                p.paths.push(f.get(4).map(|s| s.to_string()).unwrap_or_default());
            }
            _ => return Err("bad meta line".into()),
        }
    }
    Ok(parts)
}

/// Streams a partition's objects from the spill.
fn for_each_obj(dir: &Path, p: &Part, mut f: impl FnMut(usize, &[u8]) -> R<()>) -> R<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(dir.join("spill.bin"))?;
    file.seek(SeekFrom::Start(p.spill_off))?;
    let src = BufReader::with_capacity(1 << 20, file.take(p.spill_len));
    let mut dec = zstd::stream::Decoder::with_buffer(src)?;
    dec.window_log_max(SPILL_WINDOW_LOG)?;
    let mut buf = Vec::new();
    for (i, (_, len)) in p.objs.iter().enumerate() {
        buf.resize(*len as usize, 0);
        dec.read_exact(&mut buf)?;
        f(i, &buf)?;
    }
    Ok(())
}

/// Where one object lives in the variant's output.
#[derive(Clone, Copy, Default)]
struct Loc {
    /// File offset of the record or frame.
    off: u64,
    /// Compressed length of the record or frame.
    clen: u64,
    /// Offset of the object inside the decompressed frame (V2 only).
    inner: u64,
    len: u64,
    part: u32,
    /// Exp1b: chain depth (0 = compressed alone) and predecessor index in the partition.
    depth: u32,
    pred: Option<u32>,
}

struct Out {
    file: Mutex<(File, u64)>,
}

impl Out {
    fn append(&self, bytes: &[u8]) -> R<u64> {
        let mut g = self.file.lock().map_err(|_| "poisoned")?;
        let off = g.1;
        g.0.write_all(bytes)?;
        g.1 += bytes.len() as u64;
        Ok(off)
    }
}

fn train_dict(dir: &Path, p: &Part) -> R<Option<Vec<u8>>> {
    // Pre-registered: one dictionary (≤ 112 KiB) per partition. Size it to ~5%
    // of the partition and skip tiny partitions, where training is meaningless.
    if p.objs.len() < 8 || p.raw < 16 << 10 {
        return Ok(None);
    }
    let dict_size = ((p.raw / 20) as usize).clamp(1 << 10, DICT_MAX);
    let budget = (dict_size * 100) as u64;
    let stride = (p.raw / budget).max(1) as usize;
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for_each_obj(dir, p, |i, b| {
        if i % stride == 0 {
            samples.push(b[..b.len().min(128 << 10)].to_vec());
        }
        Ok(())
    })?;
    if samples.len() < 8 {
        return Ok(None);
    }
    Ok(zstd::dict::from_samples(&samples, dict_size).ok())
}

fn vm_hwm_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn run(dir: &Path, variant: &str, threads: usize, samples: usize) -> R<()> {
    if !["v0", "v1", "v2", "v3", "v4", "v5"].contains(&variant) {
        return Err(format!("unknown variant {variant}").into());
    }
    let parts = load_meta(dir)?;
    let n_objs: usize = parts.iter().map(|p| p.objs.len()).sum();
    let raw_total: u64 = parts.iter().map(|p| p.raw).sum();
    let out_path: PathBuf = dir.join(format!("{variant}.out"));
    let out = Out { file: Mutex::new((File::create(&out_path)?, 0)) };
    // Largest partitions first, for load balance.
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].raw));
    let next = AtomicUsize::new(0);
    let locs: Mutex<Vec<Vec<Loc>>> = Mutex::new(vec![Vec::new(); parts.len()]);
    let dicts: Mutex<Vec<Option<Vec<u8>>>> = Mutex::new(vec![None; parts.len()]);
    let t0 = Instant::now();
    std::thread::scope(|sc| -> R<()> {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                sc.spawn(|| -> R<()> {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&pi) = order.get(k) else { return Ok(()) };
                        let p = &parts[pi];
                        let mut ls = vec![Loc::default(); p.objs.len()];
                        match variant {
                            "v0" | "v1" => {
                                let dict = if variant == "v1" { train_dict(dir, p)? } else { None };
                                let mut c = match &dict {
                                    Some(d) => zstd::bulk::Compressor::with_dictionary(LEVEL, d)?,
                                    None => zstd::bulk::Compressor::new(LEVEL)?,
                                };
                                // Buffer records (bounded at 64 MiB) and append them in batches.
                                let mut buf = Vec::new();
                                let mut pending: Vec<usize> = Vec::new();
                                let flush = |buf: &mut Vec<u8>, pending: &mut Vec<usize>, ls: &mut Vec<Loc>| -> R<()> {
                                    let base = out.append(buf)?;
                                    for &m in pending.iter() {
                                        ls[m].off += base;
                                    }
                                    buf.clear();
                                    pending.clear();
                                    Ok(())
                                };
                                for_each_obj(dir, p, |i, b| {
                                    let z = c.compress(b)?;
                                    ls[i] = Loc {
                                        off: buf.len() as u64,
                                        clen: z.len() as u64,
                                        inner: 0,
                                        len: b.len() as u64,
                                        part: pi as u32,
                                        depth: 0,
                                        pred: None,
                                    };
                                    buf.extend_from_slice(&z);
                                    pending.push(i);
                                    if buf.len() > 64 << 20 {
                                        flush(&mut buf, &mut pending, &mut ls)?;
                                    }
                                    Ok(())
                                })?;
                                flush(&mut buf, &mut pending, &mut ls)?;
                                dicts.lock().map_err(|_| "poisoned")?[pi] = dict;
                            }
                            "v4" | "v5" => {
                                // Exp1b (PLAN.md §4.8): patch-from delta against the predecessor (same path).
                                let maxd = if variant == "v4" { 50 } else { 10 };
                                let mut c = zstd::bulk::Compressor::new(LEVEL)?;
                                let mut buf = Vec::new();
                                let mut pending: Vec<usize> = Vec::new();
                                let flush = |buf: &mut Vec<u8>, pending: &mut Vec<usize>, ls: &mut Vec<Loc>| -> R<()> {
                                    let base = out.append(buf)?;
                                    for &m in pending.iter() {
                                        ls[m].off += base;
                                    }
                                    buf.clear();
                                    pending.clear();
                                    Ok(())
                                };
                                let mut prev: Option<Vec<u8>> = None;
                                let mut prev_depth = 0u32;
                                for_each_obj(dir, p, |i, b| {
                                    let same = i > 0 && p.paths.get(i) == p.paths.get(i - 1);
                                    let (z, depth, pred) = match prev.as_ref() {
                                        Some(pv) if same && prev_depth < maxd => {
                                            let need = (pv.len() + b.len()).max(2) as u64;
                                            let wl = (64 - (need - 1).leading_zeros()).clamp(23, 27);
                                            let mut enc = zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), LEVEL, pv)?;
                                            enc.window_log(wl)?;
                                            enc.write_all(b)?;
                                            (enc.finish()?, prev_depth + 1, Some((i - 1) as u32))
                                        }
                                        _ => (c.compress(b)?, 0, None),
                                    };
                                    ls[i] = Loc { off: buf.len() as u64, clen: z.len() as u64, inner: 0, len: b.len() as u64, part: pi as u32, depth, pred };
                                    buf.extend_from_slice(&z);
                                    pending.push(i);
                                    prev = Some(b.to_vec());
                                    prev_depth = depth;
                                    if buf.len() > 64 << 20 {
                                        flush(&mut buf, &mut pending, &mut ls)?;
                                    }
                                    Ok(())
                                })?;
                                flush(&mut buf, &mut pending, &mut ls)?;
                            }
                            "v2" => {
                                let mut c = zstd::bulk::Compressor::new(LEVEL)?;
                                let mut frame: Vec<u8> = Vec::new();
                                let mut members: Vec<usize> = Vec::new();
                                let mut flush =
                                    |frame: &mut Vec<u8>, members: &mut Vec<usize>, ls: &mut Vec<Loc>| -> R<()> {
                                        if members.is_empty() {
                                            return Ok(());
                                        }
                                        let z = c.compress(frame)?;
                                        let off = out.append(&z)?;
                                        for &m in members.iter() {
                                            ls[m].off = off;
                                            ls[m].clen = z.len() as u64;
                                        }
                                        frame.clear();
                                        members.clear();
                                        Ok(())
                                    };
                                for_each_obj(dir, p, |i, b| {
                                    if !frame.is_empty() && frame.len() + b.len() > FRAME_CAP {
                                        flush(&mut frame, &mut members, &mut ls)?;
                                    }
                                    ls[i] = Loc {
                                        off: 0,
                                        clen: 0,
                                        inner: frame.len() as u64,
                                        len: b.len() as u64,
                                        part: pi as u32,
                                        depth: 0,
                                        pred: None,
                                    };
                                    frame.extend_from_slice(b);
                                    members.push(i);
                                    Ok(())
                                })?;
                                flush(&mut frame, &mut members, &mut ls)?;
                            }
                            _ => {
                                // v3: one long-window stream per partition (ceiling; not random access).
                                let mut buf = Vec::new();
                                {
                                    let mut enc = zstd::stream::Encoder::new(&mut buf, LEVEL)?;
                                    enc.window_log(27)?;
                                    enc.long_distance_matching(true)?;
                                    for_each_obj(dir, p, |_, b| Ok(enc.write_all(b)?))?;
                                    enc.finish()?;
                                }
                                out.append(&buf)?;
                            }
                        }
                        locs.lock().map_err(|_| "poisoned")?[pi] = ls;
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().map_err(|_| "worker panicked")??;
        }
        Ok(())
    })?;
    let build_secs = t0.elapsed().as_secs_f64();
    {
        let g = out.file.lock().map_err(|_| "poisoned")?;
        g.0.sync_all()?;
    }
    let payload = std::fs::metadata(&out_path)?.len();
    let dicts = dicts.into_inner().map_err(|_| "poisoned")?;
    let dict_bytes: u64 = dicts.iter().flatten().map(|d| d.len() as u64).sum();
    // §4.4: 48 B/object; §4.8 (Exp1b): +4 B predecessor reference.
    let per_obj = if variant == "v4" || variant == "v5" { INDEX_BYTES_PER_OBJECT + 4 } else { INDEX_BYTES_PER_OBJECT };
    let index_bytes = per_obj * n_objs as u64;
    let total = payload + dict_bytes + index_bytes;
    let rss_build_kb = vm_hwm_kb();

    // Random-read latency (V0–V2) and SHA-1 round-trip verification.
    let locs = locs.into_inner().map_err(|_| "poisoned")?;
    let (mut p50, mut p99, mut verified) = (f64::NAN, f64::NAN, 0usize);
    if variant != "v3" {
        let f = File::open(&out_path)?;
        // Warm the page cache (pre-registered: warm-cache measurement).
        let mut sink = vec![0u8; 8 << 20];
        let mut off = 0;
        while off < payload {
            let n = sink.len().min((payload - off) as usize);
            f.read_exact_at(&mut sink[..n], off)?;
            off += n as u64;
        }
        // Dictionaries are preloaded, as a server would cache them.
        let ddicts: Vec<Option<zstd::dict::DecoderDictionary<'static>>> =
            dicts.iter().map(|d| d.as_ref().map(|d| zstd::dict::DecoderDictionary::copy(d))).collect();
        let all: Vec<(usize, usize)> =
            locs.iter().enumerate().flat_map(|(p, v)| (0..v.len()).map(move |i| (p, i))).collect();
        let mut rng = 0x9E3779B97F4A7C15u64;
        let mut times = Vec::with_capacity(samples);
        for _ in 0..samples.min(all.len().max(1) * 10) {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let (pi, oi) = all[(rng % all.len() as u64) as usize];
            let l = locs[pi][oi];
            let t = Instant::now();
            let mut comp = vec![0u8; l.clen as usize];
            f.read_exact_at(&mut comp, l.off)?;
            let data = match variant {
                "v4" | "v5" => {
                    // Decode the chain from its root (§4.8: the whole chain counts).
                    let mut chain = vec![oi];
                    let mut cur = l;
                    while let Some(pp) = cur.pred {
                        chain.push(pp as usize);
                        cur = locs[pi][pp as usize];
                    }
                    let mut prev: Vec<u8> = Vec::new();
                    for (k, &ci) in chain.iter().rev().enumerate() {
                        let cl = locs[pi][ci];
                        let mut cc = vec![0u8; cl.clen as usize];
                        f.read_exact_at(&mut cc, cl.off)?;
                        prev = if k == 0 {
                            zstd::bulk::decompress(&cc, cl.len as usize)?
                        } else {
                            let mut dec = zstd::stream::read::Decoder::with_ref_prefix(&cc[..], &prev)?;
                            dec.window_log_max(27)?;
                            let mut o = Vec::with_capacity(cl.len as usize);
                            dec.read_to_end(&mut o)?;
                            o
                        };
                    }
                    prev
                }
                "v2" => {
                    let fr = zstd::bulk::decompress(&comp, FRAME_CAP.max((l.inner + l.len) as usize))?;
                    fr[l.inner as usize..(l.inner + l.len) as usize].to_vec()
                }
                _ => match &ddicts[l.part as usize] {
                    Some(dd) => {
                        let mut d = zstd::bulk::Decompressor::with_prepared_dictionary(dd)?;
                        d.decompress(&comp, l.len as usize)?
                    }
                    None => zstd::bulk::decompress(&comp, l.len as usize)?,
                },
            };
            times.push(t.elapsed().as_secs_f64() * 1e3);
            // Integrity: SHA-1 of the canonical form must equal the Git object id.
            if data.len() as u64 != l.len || sha1_smol::Sha1::from(&data).digest().to_string() != parts[pi].objs[oi].0 {
                return Err(format!("round-trip mismatch for {}", parts[pi].objs[oi].0).into());
            }
            verified += 1;
        }
        times.sort_by(|a, b| a.total_cmp(b));
        let q = |x: f64| times[((times.len() as f64 - 1.0) * x).round() as usize];
        if !times.is_empty() {
            p50 = q(0.50);
            p99 = q(0.99);
        }
    }
    let _ = std::fs::remove_file(&out_path);
    let depths: Vec<u32> = locs.iter().flatten().map(|l| l.depth).collect();
    let max_depth = depths.iter().copied().max().unwrap_or(0);
    let mean_depth = if depths.is_empty() { 0.0 } else { depths.iter().map(|d| *d as f64).sum::<f64>() / depths.len() as f64 };
    let big: Vec<String> = {
        let mut v: Vec<&Part> = parts.iter().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.raw));
        v.iter().take(3).map(|p| format!("{}:{}", p.typ, p.key)).collect()
    };
    println!(
        "{{\"variant\":\"{variant}\",\"objects\":{n_objs},\"partitions\":{},\"raw_bytes\":{raw_total},\"payload_bytes\":{payload},\"dict_bytes\":{dict_bytes},\"index_bytes\":{index_bytes},\"total_bytes\":{total},\"build_secs\":{build_secs:.2},\"threads\":{threads},\"peak_rss_kb\":{rss_build_kb},\"read_p50_ms\":{},\"read_p99_ms\":{},\"verified\":{verified},\"max_depth\":{max_depth},\"mean_depth\":{mean_depth:.2},\"largest_partitions\":\"{}\"}}",
        parts.len(),
        if p50.is_nan() { "null".to_string() } else { format!("{p50:.4}") },
        if p99.is_nan() { "null".to_string() } else { format!("{p99:.4}") },
        big.join(",")
    );
    Ok(())
}
