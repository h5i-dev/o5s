//! LF-delimited line coordinates, computed directly from immutable bytes.
//! Keep the cursor in the scan state so extraction exposes its relation to
//! processed input and vector capacity. A trailing LF is one line ending,
//! rather than an additional empty line.
pub struct Scan {
    pub offsets: Vec<usize>,
    pub position: usize,
}

pub fn step(mut state: Scan, byte: u8, length: usize) -> Scan {
    state.position += 1;
    if byte == b'\n' && state.position < length {
        state.offsets.push(state.position);
    }
    state
}

pub fn line_boundaries(bytes: &Vec<u8>) -> Vec<usize> {
    let mut offsets = Vec::new();
    offsets.push(0);
    let mut state = Scan {
        offsets,
        position: 0,
    };
    let mut i = 0;
    while i < bytes.len() {
        state = step(state, bytes[i], bytes.len());
        i += 1;
    }
    if bytes.len() > 0 {
        state.offsets.push(bytes.len());
    }
    state.offsets
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offsets_match_utf8_line_slices_including_trailing_lf_and_crlf() {
        let alphabet = ["a", "\n", "\r", "あ", "🦀"];
        for n in 0..=6u32 {
            for mut code in 0..alphabet.len().pow(n) {
                let mut text = String::new();
                for _ in 0..n {
                    text.push_str(alphabet[code % alphabet.len()]);
                    code /= alphabet.len();
                }
                let mut expected = vec![0];
                for line in text.split_inclusive('\n') {
                    expected.push(expected.last().unwrap() + line.len());
                }
                let offsets = line_boundaries(&text.as_bytes().to_vec());
                assert_eq!(offsets, expected);
                assert!(offsets.iter().all(|i| text.is_char_boundary(*i)));
                assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
            }
        }
    }
}
