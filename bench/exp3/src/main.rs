//! Experiment 3 harness (docs/PLAN.md §6.6): generic similarity ordering and
//! framing vs Exp1 V2, on Exp1's extracted object set.
//!
//! ```text
//! exp3 run <exp1-work-dir> <a0|a1|a2|a3|a4|r0> [threads] [samples]   # one JSON line on stdout
//! exp3 spill-files <work-dir> tensors <file.safetensors>...         # generalization corpora:
//! exp3 spill-files <work-dir> dirs <dir>...                         #   same work-dir format
//! ```
//! `spill-files` writes Exp1's work-dir format with one partition per input
//! (in the order given), objects = the safetensors' tensors (by offset) or the
//! directory's files (by path), sha = SHA-1 of the bytes.
//! Input is `exp1 extract`'s output (meta.tsv + spill.bin: canonical Git
//! payloads in V2 partition order). Pass 1 streams the spill, computes
//! features and writes a random-access re-spill (one zstd-1 frame per object).
//! Pass 2 orders, frames, and compresses frames at level 19 in parallel. All of
//! it is inside the measured build time.
//!
//! Size = payload + 48 B/object modeled index (as Exp1) + 32 B per frame that
//! has a reference prefix (its reference's id).

use o5s_order::{Frame, FrameConfig, Hint, Item, features, frames, order};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const LEVEL: i32 = 19; // pre-registered
const V2_CAP: u64 = 4 << 20; // V2's fixed frame size (pre-registered)
const INDEX_BYTES_PER_OBJECT: u64 = 48; // pre-registered model
const REF_BYTES_PER_FRAME: u64 = 32;
const SPILL_WINDOW_LOG: u32 = 26;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let res = match args.get(1).map(|s| s.as_str()) {
        Some("run") if args.len() >= 4 => {
            let threads = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);
            let samples = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            run(Path::new(&args[2]), &args[3], threads, samples)
        }
        Some("spill-files") if args.len() >= 5 => spill_files(Path::new(&args[2]), &args[3], &args[4..]),
        _ => {
            eprintln!(
                "usage: exp3 run <exp1-work-dir> <a0|a1|a2|a3|a4> [threads] [samples] | exp3 spill-files <work-dir> <tensors|dirs> <inputs>..."
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("exp3: {e}");
        std::process::exit(1);
    }
}

fn spill_files(out: &Path, mode: &str, inputs: &[String]) -> R<()> {
    std::fs::create_dir_all(out)?;
    let mut spill = File::create(out.join("spill.bin"))?;
    let mut meta = File::create(out.join("meta.tsv"))?;
    let mut off = 0u64;
    let (mut n_obj, mut raw_all) = (0usize, 0u64);
    for (pid, input) in inputs.iter().enumerate() {
        // (name, bytes) of every object of this input, in order.
        let mut objs: Vec<(String, Vec<u8>)> = Vec::new();
        match mode {
            "tensors" => {
                let f = std::fs::read(input)?;
                let layout = o5s_weights::parse_layout(&f).ok_or_else(|| format!("{input}: not safetensors"))?;
                let mut ts: Vec<(String, (u64, u64))> = layout.tensors.into_iter().collect();
                ts.sort_by_key(|t| t.1);
                for (name, (a, b)) in ts {
                    let d = f.get(a as usize..b as usize).ok_or("tensor out of range")?.to_vec();
                    objs.push((name, d));
                }
            }
            "dirs" => {
                let root = Path::new(input);
                let mut stack = vec![root.to_path_buf()];
                let mut files = Vec::new();
                while let Some(d) = stack.pop() {
                    for e in std::fs::read_dir(&d)? {
                        let p = e?.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if p.is_file() {
                            files.push(p);
                        }
                    }
                }
                files.sort();
                for p in files {
                    let rel = p.strip_prefix(root)?.to_string_lossy().replace(['\t', '\n'], " ");
                    objs.push((rel, std::fs::read(&p)?));
                }
            }
            _ => return Err(format!("unknown mode {mode} (tensors or dirs)").into()),
        }
        let mut enc = zstd::stream::Encoder::new(Vec::new(), 3)?;
        enc.window_log(SPILL_WINDOW_LOG)?;
        let mut lines = String::new();
        let mut raw = 0u64;
        for (name, d) in &objs {
            enc.write_all(d)?;
            raw += d.len() as u64;
            lines.push_str(&format!("O\t{pid}\t{}\t{}\t{name}\n", sha1_smol::Sha1::from(d).digest(), d.len()));
        }
        let buf = enc.finish()?;
        spill.write_all(&buf)?;
        writeln!(meta, "P\t{pid}\tblob\t{}\t{off}\t{}\t{}\t{raw}", pid, buf.len(), objs.len())?;
        meta.write_all(lines.as_bytes())?;
        off += buf.len() as u64;
        n_obj += objs.len();
        raw_all += raw;
    }
    spill.sync_all()?;
    eprintln!("spilled {n_obj} objects ({raw_all} bytes) from {} inputs", inputs.len());
    Ok(())
}

struct Part {
    spill_off: u64,
    spill_len: u64,
    objs: Vec<(String, u64)>,
    paths: Vec<String>,
}

fn load_meta(dir: &Path) -> R<Vec<Part>> {
    let mut parts: Vec<Part> = Vec::new();
    for line in BufReader::new(File::open(dir.join("meta.tsv"))?).lines() {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "P" => parts.push(Part {
                spill_off: f[4].parse()?,
                spill_len: f[5].parse()?,
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

fn vm_hwm_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Global object g: (sha, len, partition, path).
struct Obj {
    sha: String,
    len: u64,
    part: u32,
}

/// Random-access re-spill: one zstd-1 frame per object.
struct Respill {
    file: File,
    loc: Vec<(u64, u32)>,
}

impl Respill {
    fn get(&self, g: usize, len: u64) -> R<Vec<u8>> {
        let (off, clen) = self.loc[g];
        let mut c = vec![0u8; clen as usize];
        self.file.read_exact_at(&mut c, off)?;
        Ok(zstd::bulk::decompress(&c, len as usize)?)
    }
}

fn window_log(n: usize) -> u32 {
    let n = n.max(2) as u64;
    (64 - (n - 1).leading_zeros()).clamp(20, 27)
}

/// zstd level `LEVEL` over `data`, with `prefix` as reference prefix if given.
fn compress(prefix: Option<&[u8]>, data: &[u8]) -> R<Vec<u8>> {
    use zstd::zstd_safe::{CCtx, CParameter, ResetDirective};
    let zerr = |c: usize| format!("zstd: {}", zstd::zstd_safe::get_error_name(c));
    let mut cctx = CCtx::create();
    cctx.reset(ResetDirective::SessionAndParameters).map_err(zerr)?;
    cctx.set_parameter(CParameter::CompressionLevel(LEVEL)).map_err(zerr)?;
    cctx.set_parameter(CParameter::WindowLog(window_log(data.len() + prefix.map_or(0, |p| p.len())))).map_err(zerr)?;
    cctx.set_pledged_src_size(Some(data.len() as u64)).map_err(zerr)?;
    if let Some(p) = prefix {
        cctx.ref_prefix(p).map_err(zerr)?;
    }
    let mut out: Vec<u8> = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    cctx.compress2(&mut out, data).map_err(zerr)?;
    Ok(out)
}

/// Decodes the first `need` bytes of a frame (streaming; stops early, so an
/// object near the start of a frame doesn't pay for the whole frame).
fn decompress_prefix(prefix: Option<&[u8]>, c: &[u8], need: usize) -> R<Vec<u8>> {
    use zstd::zstd_safe::{DCtx, DParameter, InBuffer, OutBuffer};
    let zerr = |e: usize| format!("zstd: {}", zstd::zstd_safe::get_error_name(e));
    let mut d = DCtx::create();
    d.set_parameter(DParameter::WindowLogMax(27)).map_err(zerr)?;
    if let Some(p) = prefix {
        d.ref_prefix(p).map_err(zerr)?;
    }
    let mut out: Vec<u8> = vec![0u8; need];
    let mut inb = InBuffer::around(c);
    let mut pos = 0;
    while pos < need {
        let mut ob = OutBuffer::around_pos(&mut out[..], pos);
        let hint = d.decompress_stream(&mut ob, &mut inb).map_err(zerr)?;
        let new = ob.pos();
        if new == pos && (hint == 0 || inb.pos() == c.len()) {
            return Err("frame ended early".into());
        }
        pos = new;
    }
    Ok(out)
}

/// Where one frame lives in the output.
#[derive(Clone, Copy, Default)]
struct FrameLoc {
    off: u64,
    clen: u64,
    raw: u64,
}

struct Layout<'a> {
    objs: &'a [Obj],
    frames: &'a [Frame],
    locs: &'a [FrameLoc],
    /// Per object: (frame, offset inside the frame).
    at: &'a [(u32, u64)],
    out: &'a File,
}

impl Layout<'_> {
    /// Reads object g, decoding its frame and (recursively) its reference.
    fn get(&self, g: usize) -> R<Vec<u8>> {
        let (fi, inner) = self.at[g];
        let f = &self.frames[fi as usize];
        let l = self.locs[fi as usize];
        let prefix = match f.reference {
            Some(r) => Some(self.get(r)?),
            None => None,
        };
        let mut c = vec![0u8; l.clen as usize];
        self.out.read_exact_at(&mut c, l.off)?;
        let len = self.objs[g].len as usize;
        let end = inner as usize + len;
        if end as u64 > l.raw {
            return Err("frame shorter than index".into());
        }
        let raw = decompress_prefix(prefix.as_deref(), &c, end)?;
        Ok(raw[inner as usize..end].to_vec())
    }
}

fn run(dir: &Path, variant: &str, threads: usize, samples: usize) -> R<()> {
    let t0 = Instant::now();
    let parts = load_meta(dir)?;
    let mut objs: Vec<Obj> = Vec::new();
    let mut hints: Vec<Hint> = Vec::new();
    for (pi, p) in parts.iter().enumerate() {
        for (oi, (sha, len)) in p.objs.iter().enumerate() {
            objs.push(Obj { sha: sha.clone(), len: *len, part: pi as u32 });
            hints.push(Hint { key: p.paths.get(oi).cloned().unwrap_or_default(), pos: objs.len() as u64 });
        }
    }
    let n = objs.len();
    let want_feats = variant != "a0" && variant != "r0";
    // Pass 1: features + random-access re-spill, partitions in parallel.
    let respill_path = dir.join(format!("exp3-respill-{}.bin", std::process::id()));
    let respill_file =
        std::fs::OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&respill_path)?;
    let w = Mutex::new((respill_file.try_clone()?, 0u64, vec![(0u64, 0u32); n]));
    let feats: Vec<Mutex<Option<o5s_order::Features>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let starts: Vec<usize> = parts
        .iter()
        .scan(0, |acc, p| {
            let s = *acc;
            *acc += p.objs.len();
            Some(s)
        })
        .collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| -> R<()> {
        let mut hs = Vec::new();
        for _ in 0..threads {
            hs.push(sc.spawn(|| -> R<()> {
                loop {
                    let pi = next.fetch_add(1, Ordering::Relaxed);
                    if pi >= parts.len() {
                        return Ok(());
                    }
                    let base = starts[pi];
                    for_each_obj(dir, &parts[pi], |i, b| {
                        if want_feats {
                            *feats[base + i].lock().map_err(|_| "poisoned")? = features(b);
                        }
                        let c = zstd::bulk::compress(b, 1)?;
                        let mut g = w.lock().map_err(|_| "poisoned")?;
                        let off = g.1;
                        g.0.write_all(&c)?;
                        g.1 += c.len() as u64;
                        g.2[base + i] = (off, c.len() as u32);
                        Ok(())
                    })?;
                }
            }));
        }
        for h in hs {
            h.join().map_err(|_| "worker panicked")??;
        }
        Ok(())
    })?;
    let (_, _, rloc) = w.into_inner().map_err(|_| "poisoned")?;
    let respill = Respill { file: respill_file, loc: rloc };
    let items: Vec<Item> = (0..n)
        .map(|g| Item {
            len: objs[g].len,
            feats: feats[g].lock().ok().and_then(|mut f| f.take()),
            hint: Some(hints[g].clone()),
        })
        .collect();
    let t_feat = t0.elapsed().as_secs_f64();
    // Order.
    let ord: Vec<usize> = match variant {
        // r0: the generalization corpora's reference (PLAN §6.6): input order,
        // fixed 4 MiB frames, no partition cuts.
        "a0" | "a2" | "r0" => (0..n).collect(),
        "a1" | "a3" => order(&items, true),
        "a4" => order(&items, false),
        _ => return Err(format!("unknown variant {variant}").into()),
    };
    let t_order = t0.elapsed().as_secs_f64() - t_feat;
    // Frames.
    let fixed = FrameConfig { cap: V2_CAP, fixed: true, ..FrameConfig::default() };
    // Dev-set tuning knobs (PLAN §6.6: constants tunable on ripgrep/jq only;
    // the frozen values are recorded before the official run).
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
    let d = FrameConfig::default();
    let sim = FrameConfig {
        min: env("EXP3_MIN").unwrap_or(d.min),
        cap: env("EXP3_CAP").unwrap_or(d.cap),
        lookback: env("EXP3_LOOKBACK").map_or(d.lookback, |v| v as usize),
        max_ref_depth: env("EXP3_DEPTH").map_or(d.max_ref_depth, |v| v as u32),
        fixed: false,
    };
    let part_boundary = |a: usize, b: usize| objs[a].part != objs[b].part;
    let fr: Vec<Frame> = match variant {
        "a0" => frames(&items, &ord, &fixed, &part_boundary),
        "a1" | "r0" => frames(&items, &ord, &fixed, &|_, _| false),
        _ => frames(&items, &ord, &sim, &|_, _| false),
    };
    // Per object: frame and inner offset.
    let mut at = vec![(0u32, 0u64); n];
    for (fi, f) in fr.iter().enumerate() {
        let mut off = 0u64;
        for &m in &f.members {
            at[m] = (fi as u32, off);
            off += objs[m].len;
        }
    }
    // Compress frames in parallel; write in frame order.
    let out_path = dir.join(format!("exp3-{variant}-{}.bin", std::process::id()));
    let mut out = File::create(&out_path)?;
    let mut locs = vec![FrameLoc::default(); fr.len()];
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Vec<u8>>>> = (0..fr.len()).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|sc| -> R<()> {
        let mut hs = Vec::new();
        for _ in 0..threads {
            hs.push(sc.spawn(|| -> R<()> {
                loop {
                    let fi = next.fetch_add(1, Ordering::Relaxed);
                    if fi >= fr.len() {
                        return Ok(());
                    }
                    let f = &fr[fi];
                    let mut raw = Vec::new();
                    for &m in &f.members {
                        raw.extend_from_slice(&respill.get(m, objs[m].len)?);
                    }
                    let prefix = match f.reference {
                        Some(r) => Some(respill.get(r, objs[r].len)?),
                        None => None,
                    };
                    let c = compress(prefix.as_deref(), &raw)?;
                    *results[fi].lock().map_err(|_| "poisoned")? = Some(c);
                }
            }));
        }
        for h in hs {
            h.join().map_err(|_| "worker panicked")??;
        }
        Ok(())
    })?;
    let mut off = 0u64;
    for (fi, f) in fr.iter().enumerate() {
        let c = results[fi].lock().map_err(|_| "poisoned")?.take().ok_or("missing frame")?;
        out.write_all(&c)?;
        let raw: u64 = f.members.iter().map(|&m| objs[m].len).sum();
        locs[fi] = FrameLoc { off, clen: c.len() as u64, raw };
        off += c.len() as u64;
    }
    out.sync_all()?;
    drop(out);
    let build_secs = t0.elapsed().as_secs_f64();
    let rss_build_kb = vm_hwm_kb();
    let payload = off;
    let with_ref = fr.iter().filter(|f| f.reference.is_some()).count() as u64;
    let total = payload + INDEX_BYTES_PER_OBJECT * n as u64 + REF_BYTES_PER_FRAME * with_ref;
    let raw_total: u64 = objs.iter().map(|o| o.len).sum();
    // Reads: `samples` uniform random objects, `threads` concurrent readers, warm.
    let out_file = File::open(&out_path)?;
    let lay = Layout { objs: &objs, frames: &fr, locs: &locs, at: &at, out: &out_file };
    // Warm the page cache.
    let mut sink = Vec::new();
    File::open(&out_path)?.read_to_end(&mut sink)?;
    drop(sink);
    let lat: Mutex<Vec<f64>> = Mutex::new(Vec::with_capacity(samples));
    let bad = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for t in 0..threads {
            let (lay, lat, bad, objs) = (&lay, &lat, &bad, &objs);
            sc.spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
                let mut mine = Vec::new();
                for _ in 0..samples / threads {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let g = (x % n as u64) as usize;
                    let t = Instant::now();
                    let ok = lay.get(g).is_ok_and(|d| sha1_smol::Sha1::from(&d).digest().to_string() == objs[g].sha);
                    mine.push(t.elapsed().as_secs_f64() * 1e3);
                    if !ok {
                        bad.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if let Ok(mut l) = lat.lock() {
                    l.extend(mine);
                }
            });
        }
    });
    let mut lat = lat.into_inner().map_err(|_| "poisoned")?;
    lat.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| lat.get(((lat.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(f64::NAN);
    let bad = bad.load(Ordering::Relaxed);
    let max_depth = fr.iter().map(|f| f.depth).max().unwrap_or(0);
    let mean_frame = raw_total as f64 / fr.len().max(1) as f64;
    println!(
        "{{\"variant\":\"{variant}\",\"objects\":{n},\"raw_bytes\":{raw_total},\"payload_bytes\":{payload},\"frames\":{},\"frames_with_ref\":{with_ref},\"mean_frame_raw\":{mean_frame:.0},\"max_ref_depth\":{max_depth},\"total_bytes\":{total},\"build_secs\":{build_secs:.2},\"feature_secs\":{t_feat:.2},\"order_secs\":{t_order:.2},\"threads\":{threads},\"peak_rss_kb\":{rss_build_kb},\"read_p50_ms\":{:.4},\"read_p99_ms\":{:.4},\"verified\":{},\"failed\":{bad}}}",
        fr.len(),
        q(0.5),
        q(0.99),
        lat.len() - bad,
    );
    std::fs::remove_file(&out_path)?;
    std::fs::remove_file(&respill_path)?;
    if bad > 0 {
        return Err(format!("{bad} reads failed verification").into());
    }
    Ok(())
}
