//! Measures zstd on safetensors data with and without byte grouping.
//!
//!   cargo run --release -p pico2peta-weights --example bytegroup_measure -- <base.safetensors> [new.safetensors]
//!
//! With two files, it measures the tensor-aligned XOR delta (new ^ base), which is what
//! `put_checkpoint` stores. With one file, it measures the raw tensors (a first upload). Each
//! case is compressed per 64 KiB block (the granularity of pack chunks) and per whole tensor,
//! at zstd levels 3 and 19. Grouping width comes from the tensor dtype (bf16/f16 2, f32 4).
//! Run timing-sensitive invocations under the shared bench lock.

use pico2peta_weights::bytegroup::{group, ungroup, width_for_dtype};
use std::collections::HashMap;
use std::time::Instant;

type Tensors = HashMap<String, (String, Vec<u8>)>;
/// (name, grouping width, bytes)
type Item = (String, usize, Vec<u8>);

fn load(path: &str) -> Result<Tensors, String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let n = u64::from_le_bytes(b.get(..8).ok_or("short file")?.try_into().map_err(|_| "short")?) as usize;
    let header: serde_json::Value =
        serde_json::from_slice(b.get(8..8 + n).ok_or("bad header length")?).map_err(|e| e.to_string())?;
    let data = &b[8 + n..];
    let mut out = HashMap::new();
    for (name, v) in header.as_object().ok_or("header not an object")? {
        if name == "__metadata__" {
            continue;
        }
        let dtype = v["dtype"].as_str().ok_or("dtype")?.to_string();
        let off = v["data_offsets"].as_array().ok_or("offsets")?;
        let (s, e) = (off[0].as_u64().ok_or("off")? as usize, off[1].as_u64().ok_or("off")? as usize);
        out.insert(name.clone(), (dtype, data.get(s..e).ok_or("tensor out of range")?.to_vec()));
    }
    Ok(out)
}

fn zsize(data: &[u8], level: i32, block: Option<usize>) -> usize {
    match block {
        Some(bs) => data.chunks(bs).map(|c| zstd::bulk::compress(c, level).map(|z| z.len()).unwrap_or(c.len())).sum(),
        None => zstd::bulk::compress(data, level).map(|z| z.len()).unwrap_or(data.len()),
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let base = load(args.first().ok_or("usage: bytegroup_measure <base> [new]")?)?;
    let (label, items): (&str, Vec<Item>) = match args.get(1) {
        Some(newp) => {
            let new = load(newp)?;
            let mut v = Vec::new();
            for (name, (dt, nb)) in &new {
                if let Some((bdt, bb)) = base.get(name)
                    && bdt == dt
                    && bb.len() == nb.len()
                {
                    let x: Vec<u8> = nb.iter().zip(bb).map(|(a, b)| a ^ b).collect();
                    v.push((name.clone(), width_for_dtype(dt).unwrap_or(1), x));
                }
            }
            ("XOR delta (new ^ base)", v)
        }
        None => (
            "raw tensors",
            base.iter().map(|(n, (dt, b))| (n.clone(), width_for_dtype(dt).unwrap_or(1), b.clone())).collect(),
        ),
    };
    let raw: usize = items.iter().map(|i| i.2.len()).sum();
    let zero_frac = items.iter().map(|i| i.2.iter().filter(|&&b| b == 0).count()).sum::<usize>() as f64 / raw as f64;
    println!("{label}: {} tensors, {:.1} MB, {:.1}% zero bytes", items.len(), raw as f64 / 1e6, zero_frac * 100.0);
    // Transform throughput and round-trip check.
    let t = Instant::now();
    let grouped: Vec<Vec<u8>> = items.iter().map(|(_, w, b)| group(b, *w).unwrap_or_else(|_| b.clone())).collect();
    let gsecs = t.elapsed().as_secs_f64();
    for ((_, w, b), g) in items.iter().zip(&grouped) {
        if ungroup(g, *w).as_deref() != Ok(b.as_slice()) {
            return Err("round-trip failed".into());
        }
    }
    println!("group: {:.2} GB/s (round-trip verified)", raw as f64 / gsecs / 1e9);
    println!(
        "Granularity: \"tensor-grouped\" groups each whole tensor and then compresses it (in 64 KiB blocks or whole)."
    );
    println!("\"per-block\" groups each 64 KiB block on its own, which is what a per-chunk record codec can do.");
    println!("| granularity | zstd level | plain | grouped | gain |");
    println!("|---|---|---|---|---|");
    for level in [3, 19] {
        let bs = 64usize << 10;
        let plain: usize = items.iter().map(|i| zsize(&i.2, level, Some(bs))).sum();
        let per_block: usize = items
            .iter()
            .map(|(_, w, b)| {
                b.chunks(bs).map(|c| zsize(&group(c, *w).unwrap_or_else(|_| c.to_vec()), level, None)).sum::<usize>()
            })
            .sum();
        println!(
            "| 64 KiB blocks, per-block grouping | {level} | {:.1} MB ({:.3}) | {:.1} MB ({:.3}) | {:+.1}% |",
            plain as f64 / 1e6,
            plain as f64 / raw as f64,
            per_block as f64 / 1e6,
            per_block as f64 / raw as f64,
            (1.0 - per_block as f64 / plain as f64) * 100.0
        );
    }
    for (gran, block) in
        [("64 KiB blocks, tensor-grouped", Some(64usize << 10)), ("whole tensor, tensor-grouped", None)]
    {
        for level in [3, 19] {
            let plain: usize = items.iter().map(|i| zsize(&i.2, level, block)).sum();
            let grp: usize = grouped.iter().map(|g| zsize(g, level, block)).sum();
            println!(
                "| {gran} | {level} | {:.1} MB ({:.3}) | {:.1} MB ({:.3}) | {:+.1}% |",
                plain as f64 / 1e6,
                plain as f64 / raw as f64,
                grp as f64 / 1e6,
                grp as f64 / raw as f64,
                (1.0 - grp as f64 / plain as f64) * 100.0
            );
        }
    }
    Ok(())
}
