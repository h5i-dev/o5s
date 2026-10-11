//! Conservative conflict detection in immutable, zero-based line coordinates.
//! Replacement ranges are half-open; insertion at a replacement boundary is
//! treated as a conflict so no insertion is silently reordered across a delete.
pub fn conflict(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    if a_start == a_end {
        b_start <= a_start && a_start <= b_end
    } else if b_start == b_end {
        a_start <= b_start && b_start <= a_end
    } else {
        a_start < b_end && b_start < a_end
    }
}

/// Byte-level interpretation of one replacement. Callers supply UTF-8 line
/// boundaries; the proof establishes preservation of both untouched slices.
pub fn splice(bytes: &Vec<u8>, start: usize, end: usize, replacement: &Vec<u8>) -> Vec<u8> {
    let before = h5i_app_std::bytes::slice(bytes, 0, start);
    let after = h5i_app_std::bytes::slice(bytes, end, bytes.len());
    let prefix = h5i_app_std::bytes::concat(&before, replacement);
    h5i_app_std::bytes::concat(&prefix, &after)
}

/// Append a slice without cloning the accumulated prefix. Used by the native
/// multi-range renderer to keep output work linear in emitted bytes.
pub fn append_range(mut out: Vec<u8>, bytes: &Vec<u8>, start: usize, end: usize) -> Vec<u8> {
    let mut i = start;
    while i < end {
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Sorted byte intervals on one immutable snapshot. The native adapter checks
/// coordinates before entering the checked, extracted rendering loop.
pub fn render_chunks(bytes: &Vec<u8>, chunks: &Vec<(usize, usize, Vec<u8>)>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let mut i = 0;
    while i < chunks.len() {
        let chunk = &chunks[i];
        out = append_range(out, bytes, cursor, chunk.0);
        out = append_range(out, &chunk.2, 0, chunk.2.len());
        cursor = chunk.1;
        i += 1;
    }
    append_range(out, bytes, cursor, bytes.len())
}

struct Validation {
    valid: bool,
    cursor: usize,
    capacity: usize,
}
fn validation_step(
    state: Validation,
    bytes_len: usize,
    start: usize,
    end: usize,
    replacement_len: usize,
) -> Validation {
    let ordered = state.valid && state.cursor <= start && start <= end && end <= bytes_len;
    if replacement_len <= usize::MAX - state.capacity {
        Validation {
            valid: ordered,
            cursor: end,
            capacity: state.capacity + replacement_len,
        }
    } else {
        Validation {
            valid: false,
            cursor: end,
            capacity: state.capacity,
        }
    }
}

/// Validate sorted intervals and a conservative output-capacity bound inside
/// the extractable kernel, so renderer safety does not rely on native checks.
pub fn valid_chunks(bytes_len: usize, chunks: &Vec<(usize, usize, Vec<u8>)>) -> bool {
    let mut state = Validation {
        valid: true,
        cursor: 0,
        capacity: bytes_len,
    };
    let mut i = 0;
    while i < chunks.len() {
        let chunk = &chunks[i];
        state = validation_step(state, bytes_len, chunk.0, chunk.1, chunk.2.len());
        i += 1;
    }
    state.valid
}

pub fn checked_render(bytes: &Vec<u8>, chunks: &Vec<(usize, usize, Vec<u8>)>) -> Option<Vec<u8>> {
    if valid_chunks(bytes.len(), chunks) {
        Some(render_chunks(bytes, chunks))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_renderer_rejects_invalid_coordinates_and_preserves_slices() {
        for n in 0..8 {
            let bytes = (0..n as u8).collect::<Vec<_>>();
            for a in 0..10 {
                for b in 0..10 {
                    for c in 0..10 {
                        for d in 0..10 {
                            let chunks = vec![(a, b, vec![101]), (c, d, vec![102, 103])];
                            let actual = checked_render(&bytes, &chunks);
                            if a <= b && b <= c && c <= d && d <= n {
                                let mut expected = bytes[..a].to_vec();
                                expected.push(101);
                                expected.extend_from_slice(&bytes[b..c]);
                                expected.extend_from_slice(&[102, 103]);
                                expected.extend_from_slice(&bytes[d..]);
                                assert_eq!(actual, Some(expected));
                            } else {
                                assert_eq!(actual, None);
                            }
                        }
                    }
                }
            }
        }
        assert!(valid_chunks(usize::MAX, &vec![]));
        assert!(!valid_chunks(usize::MAX, &vec![(0, 0, vec![1])]));
        assert!(!valid_chunks(usize::MAX - 1, &vec![(0, 0, vec![1, 2])]));
        assert!(!valid_chunks(3, &vec![(2, 1, vec![]), (1, 2, vec![])]));
    }
}
