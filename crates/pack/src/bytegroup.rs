//! Byte grouping ("byte-plane shuffle") for fixed-width numeric data, the
//! ZipNN observation (docs/RESEARCH_SPEEDUP.md §2).
//!
//! In a tensor of `width`-byte elements, bytes at the same position within
//! each element (for example all bf16 high bytes: sign, exponent, top mantissa
//! bits) are far more alike than neighbouring bytes are. [`group`] lays them
//! out as `width` contiguous planes. Plane `k` holds byte `k` of every element,
//! so a general-purpose compressor such as zstd sees long, low-entropy runs.
//! Trailing bytes (`len % width`) are appended unchanged after the planes.
//! [`ungroup`] is the exact inverse.
//!
//! This transform is part of the on-disk format: record codec 2
//! (`GroupedZstd`) stores `width | zstd(group(raw, width))`. Its exact output
//! is pinned by golden vectors in `tests/decoders.rs`; changing it is a format
//! change. (Written by the net session in `o5s-weights`; moved here when
//! it became format, and re-exported there.)

/// Widths supported (bytes per element). 2 = bf16/fp16, 4 = fp32; others are
/// accepted for completeness.
pub const MAX_WIDTH: usize = 16;

/// Invalid input to [`group`] / [`ungroup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupError {
    /// `width` is 0 or larger than [`MAX_WIDTH`].
    BadWidth(usize),
}

impl std::fmt::Display for GroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupError::BadWidth(w) => write!(f, "byte-group width {w} not in 1..={MAX_WIDTH}"),
        }
    }
}

impl std::error::Error for GroupError {}

fn check(width: usize) -> Result<(), GroupError> {
    if width == 0 || width > MAX_WIDTH { Err(GroupError::BadWidth(width)) } else { Ok(()) }
}

/// Splits `bytes` into `width` byte planes, followed by the `len % width` tail.
pub fn group(bytes: &[u8], width: usize) -> Result<Vec<u8>, GroupError> {
    check(width)?;
    let n = bytes.len() / width;
    let mut out = vec![0u8; bytes.len()];
    match width {
        // Specialized hot paths for the common dtypes.
        2 => {
            let (lo, hi) = out[..2 * n].split_at_mut(n);
            for (i, e) in bytes[..2 * n].as_chunks::<2>().0.iter().enumerate() {
                lo[i] = e[0];
                hi[i] = e[1];
            }
        }
        4 => {
            let planes = &mut out[..4 * n];
            for (i, e) in bytes[..4 * n].as_chunks::<4>().0.iter().enumerate() {
                planes[i] = e[0];
                planes[n + i] = e[1];
                planes[2 * n + i] = e[2];
                planes[3 * n + i] = e[3];
            }
        }
        _ => {
            for (i, e) in bytes[..width * n].chunks_exact(width).enumerate() {
                for (k, &b) in e.iter().enumerate() {
                    out[k * n + i] = b;
                }
            }
        }
    }
    out[width * n..].copy_from_slice(&bytes[width * n..]);
    Ok(out)
}

/// Inverse of [`group`]: `ungroup(&group(b, w)?, w)? == b` for every input.
pub fn ungroup(grouped: &[u8], width: usize) -> Result<Vec<u8>, GroupError> {
    check(width)?;
    let n = grouped.len() / width;
    let mut out = vec![0u8; grouped.len()];
    match width {
        2 => {
            let (lo, hi) = grouped[..2 * n].split_at(n);
            for (i, e) in out[..2 * n].as_chunks_mut::<2>().0.iter_mut().enumerate() {
                e[0] = lo[i];
                e[1] = hi[i];
            }
        }
        4 => {
            let p = &grouped[..4 * n];
            for (i, e) in out[..4 * n].as_chunks_mut::<4>().0.iter_mut().enumerate() {
                e[0] = p[i];
                e[1] = p[n + i];
                e[2] = p[2 * n + i];
                e[3] = p[3 * n + i];
            }
        }
        _ => {
            for (i, e) in out[..width * n].chunks_exact_mut(width).enumerate() {
                for (k, b) in e.iter_mut().enumerate() {
                    *b = grouped[k * n + i];
                }
            }
        }
    }
    out[width * n..].copy_from_slice(&grouped[width * n..]);
    Ok(out)
}

/// The natural grouping width for a safetensors dtype string, if any.
pub fn width_for_dtype(dtype: &str) -> Option<usize> {
    Some(match dtype {
        "BF16" | "F16" | "I16" | "U16" => 2,
        "F32" | "I32" | "U32" => 4,
        "F64" | "I64" | "U64" => 8,
        _ => return None, // 1-byte types gain nothing
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_byte_planes() {
        let b = [1u8, 2, 3, 4, 5, 6, 7];
        assert_eq!(group(&b, 2).unwrap(), vec![1, 3, 5, 2, 4, 6, 7]);
        assert_eq!(group(&b, 4).unwrap(), vec![1, 2, 3, 4, 5, 6, 7], "one element + 3-byte tail");
        assert_eq!(group(&[1, 2, 3, 4, 5, 6, 7, 8], 4).unwrap(), vec![1, 5, 2, 6, 3, 7, 4, 8]);
        assert_eq!(group(&b, 3).unwrap(), vec![1, 4, 2, 5, 3, 6, 7]);
        assert_eq!(group(&[], 2).unwrap(), Vec::<u8>::new());
        assert_eq!(group(&b, 1).unwrap(), b.to_vec());
    }

    #[test]
    fn bad_widths_are_errors_not_panics() {
        for w in [0, 17, usize::MAX] {
            assert_eq!(group(b"abc", w), Err(GroupError::BadWidth(w)));
            assert_eq!(ungroup(b"abc", w), Err(GroupError::BadWidth(w)));
        }
    }

    #[test]
    fn dtype_widths() {
        assert_eq!(width_for_dtype("BF16"), Some(2));
        assert_eq!(width_for_dtype("F32"), Some(4));
        assert_eq!(width_for_dtype("U8"), None);
        assert_eq!(width_for_dtype("nonsense"), None);
    }
}
