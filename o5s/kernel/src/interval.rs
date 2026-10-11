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
