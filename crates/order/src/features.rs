//! Odess-style content-defined-sampling features (Zhang et al., FAST 2021),
//! with the exact parameters pre-registered for Experiment 1c (PLAN §4.9):
//! - a Gear rolling hash `h = (h << 1) + G[byte]`, with G from splitmix64
//!   seeded `0x0DE55_5EED`;
//! - a position is sampled when `h >> 57 == 0` (rate 1/128);
//! - 12 linear transforms `m_i·h + a_i`, each feature being the max over samples;
//! - 3 super-features, `FNV-1a-64` of 4 features each.
//!
//! Objects over [`WINDOWED_MIN`] get one feature set per [`WINDOW`]-byte
//! window, and their super-features are the union (so a large object is
//! similar to anything sharing any window).

/// Objects at least this large are featurized per window.
pub const WINDOWED_MIN: usize = 1 << 20;
/// Window size for large objects.
pub const WINDOW: usize = 64 << 10;

/// Features of one object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Features {
    /// Coarse sketch: SimHash (64 random ±1 projections) of the byte
    /// histogram. Near for content with a similar byte distribution (same
    /// language, same encoding), even when nothing is shared verbatim.
    pub coarse: u64,
    /// Super-features (3 per window, sorted, deduplicated).
    pub sf: Vec<u64>,
    /// The 12 raw features of the first window (a finer tie-breaker).
    pub f: [u64; 12],
}

impl Features {
    /// Number of super-features shared with `other`.
    pub fn shared(&self, other: &Features) -> usize {
        let (mut i, mut j, mut n) = (0, 0, 0);
        while i < self.sf.len() && j < other.sf.len() {
            match self.sf[i].cmp(&other.sf[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    n += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        n
    }

    /// Number of equal raw features (0..=12).
    pub fn equal(&self, other: &Features) -> usize {
        self.f.iter().zip(&other.f).filter(|(a, b)| a == b).count()
    }
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Params {
    gear: [u64; 256],
    m: [u64; 12],
    a: [u64; 12],
    /// `proj[k]`: 256 random signs for SimHash bit k (as a bitmask per byte value).
    proj: [[bool; 256]; 64],
}

fn params() -> &'static Params {
    static P: std::sync::OnceLock<Params> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let mut st = 0x0DE5_55EEDu64;
        let mut gear = [0u64; 256];
        for g in gear.iter_mut() {
            *g = splitmix(&mut st);
        }
        let mut m = [0u64; 12];
        let mut a = [0u64; 12];
        for k in 0..12 {
            m[k] = splitmix(&mut st) | 1;
            a[k] = splitmix(&mut st);
        }
        // Separate stream, so the Exp1c parameters above are unchanged.
        let mut st2 = 0x0051_3EC0_A25E_u64;
        let mut proj = [[false; 256]; 64];
        for row in proj.iter_mut() {
            for v in row.iter_mut() {
                *v = splitmix(&mut st2) & 1 == 1;
            }
        }
        Params { gear, m, a, proj }
    })
}

/// The 12 raw features of `b`, or `None` if no position is sampled.
pub fn raw_features(b: &[u8]) -> Option<[u64; 12]> {
    let p = params();
    let mut h = 0u64;
    let mut f = [0u64; 12];
    let mut any = false;
    for &x in b {
        h = (h << 1).wrapping_add(p.gear[x as usize]);
        if h >> 57 == 0 {
            any = true;
            for k in 0..12 {
                let t = p.m[k].wrapping_mul(h).wrapping_add(p.a[k]);
                if t > f[k] {
                    f[k] = t;
                }
            }
        }
    }
    any.then_some(f)
}

/// The 3 super-features of a raw feature vector.
pub fn super_features(f: &[u64; 12]) -> [u64; 3] {
    let mut sf = [0u64; 3];
    for (j, s) in sf.iter_mut().enumerate() {
        let mut x = 0xcbf2_9ce4_8422_2325u64;
        for v in &f[4 * j..4 * j + 4] {
            for byte in v.to_le_bytes() {
                x ^= byte as u64;
                x = x.wrapping_mul(0x0100_0000_01b3);
            }
        }
        *s = x;
    }
    sf
}

/// SimHash of the byte histogram (see [`Features::coarse`]).
pub fn coarse(b: &[u8]) -> u64 {
    let mut hist = [0u64; 256];
    for &x in b {
        hist[x as usize] += 1;
    }
    let p = params();
    let mut out = 0u64;
    for (k, row) in p.proj.iter().enumerate() {
        let mut acc = 0i64;
        for (v, &c) in hist.iter().enumerate() {
            if c != 0 {
                acc += if row[v] { c as i64 } else { -(c as i64) };
            }
        }
        if acc > 0 {
            out |= 1 << k;
        }
    }
    out
}

/// Features of an object, or `None` if no position is sampled (in practice
/// objects under ~256 bytes).
pub fn features(b: &[u8]) -> Option<Features> {
    if b.len() < WINDOWED_MIN {
        let f = raw_features(b)?;
        return Some(Features { coarse: coarse(b), sf: super_features(&f).to_vec(), f });
    }
    let mut sf = Vec::new();
    let mut first = None;
    for w in b.chunks(WINDOW) {
        if let Some(f) = raw_features(w) {
            first.get_or_insert(f);
            sf.extend_from_slice(&super_features(&f));
        }
    }
    sf.sort_unstable();
    sf.dedup();
    first.map(|f| Features { coarse: coarse(b), sf, f })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, seed: u64) -> Vec<u8> {
        let mut st = seed;
        (0..n).map(|_| splitmix(&mut st) as u8).collect()
    }

    #[test]
    fn resemblance_is_captured() {
        let a = data(64 << 10, 1);
        let mut b = a.clone();
        b[1000] ^= 1; // one byte edit
        let c = data(64 << 10, 2);
        let (fa, fb, fc) = (features(&a).unwrap(), features(&b).unwrap(), features(&c).unwrap());
        assert!(fa.shared(&fb) >= 2, "near-duplicates share super-features");
        assert_eq!(fa.shared(&fc), 0);
        assert!(features(b"tiny").is_none());
    }

    /// Exactly Exp1c's extractor (bench/exp1c, PLAN §4.9). The expected values
    /// were computed by compiling bench/exp1c's own `super_features` verbatim.
    #[test]
    fn matches_exp1c_extractor() {
        let b: Vec<u8> = (0..10_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        let raw = raw_features(&b).unwrap();
        assert_eq!(super_features(&raw), [0x9df652c74fe0f761, 0x92adb82d5a2e646c, 0x98efb3232e635d87]);
    }

    #[test]
    fn large_objects_union_windows() {
        let a = data(3 << 20, 5);
        let fa = features(&a).unwrap();
        assert!(fa.sf.len() > 3 * 40, "{}", fa.sf.len());
        // A copy of one window inside an unrelated object is found.
        let mut b = data(2 << 20, 6);
        b[..WINDOW].copy_from_slice(&a[WINDOW..2 * WINDOW]);
        assert!(features(&b).unwrap().shared(&fa) >= 1);
    }
}
