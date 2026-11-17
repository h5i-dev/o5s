//! The file-path ↔ name encoding (`o5s_kernel::pathname`).

use o5s_kernel::names::{MAX_NAME, validate};
use o5s_kernel::pathname::{PathError, decode, encode};
use proptest::prelude::*;

fn enc(p: &str) -> Result<String, PathError> {
    encode(p.as_bytes()).map(|n| String::from_utf8(n).unwrap())
}

fn dec(n: &str) -> Result<Vec<u8>, PathError> {
    decode(n.as_bytes())
}

/// The reference implementation used in the SWE-bench experiment agrees wherever its rules
/// are the same (no `.`/`..` segment, canonical input).
#[test]
fn matches_the_reference_encoding() {
    for (p, n) in [
        ("src/a b.py", "src/a+20b.py"),
        ("x/[1]~%.txt", "x/+5B1+5D+7E+25.txt"),
        ("é/ü", "+C3+A9/+C3+BC"),
        ("a+b", "a+2Bb"),
        ("_init_.py", "_init_.py"),
        ("Dockerfile", "Dockerfile"),
    ] {
        assert_eq!(enc(p).unwrap(), n, "{p}");
        assert_eq!(dec(n).unwrap(), p.as_bytes(), "{n}");
    }
}

#[test]
fn dot_segments_are_escaped_and_other_dots_are_not() {
    assert_eq!(enc(".").unwrap(), "+2E");
    assert_eq!(enc("..").unwrap(), "+2E+2E");
    assert_eq!(enc("a/./b/../c").unwrap(), "a/+2E/b/+2E+2E/c");
    assert_eq!(enc(".../.x/x./.hidden").unwrap(), ".../.x/x./.hidden");
    assert_eq!(dec("a/+2E+2E/c").unwrap(), b"a/../c");
}

#[test]
fn encoding_errors() {
    assert_eq!(enc(""), Err(PathError::Empty));
    assert_eq!(enc("/a"), Err(PathError::EmptySegment));
    assert_eq!(enc("a/"), Err(PathError::EmptySegment));
    assert_eq!(enc("a//b"), Err(PathError::EmptySegment));
    assert_eq!(enc("/"), Err(PathError::EmptySegment));
    assert_eq!(encode(&vec![b'a'; MAX_NAME]).map(|n| n.len()), Ok(MAX_NAME));
    assert_eq!(encode(&vec![b'a'; MAX_NAME + 1]), Err(PathError::TooLong));
    // Escapes count: 342 spaces need 1026 bytes.
    assert_eq!(encode(&vec![b' '; 342]), Err(PathError::TooLong));
}

#[test]
fn decoding_is_strict() {
    assert_eq!(dec(""), Err(PathError::Empty));
    assert_eq!(dec("a+2bb"), Err(PathError::BadEscape), "lowercase hex");
    assert_eq!(dec("a+2"), Err(PathError::BadEscape), "truncated escape");
    assert_eq!(dec("a+G0"), Err(PathError::BadEscape));
    assert_eq!(dec("a b"), Err(PathError::BadByte));
    assert_eq!(dec("+61"), Err(PathError::NonCanonical), "escape of a plain byte");
    assert_eq!(dec("+2E/x"), Ok(b"./x".to_vec()));
    assert_eq!(dec("a/./b"), Err(PathError::NonCanonical), "a literal '.' segment");
    assert_eq!(dec("+2Ex"), Err(PathError::NonCanonical), "a dot that needn't be escaped");
    assert_eq!(dec("a+2Fb"), Err(PathError::NonCanonical), "an escaped '/'");
    assert_eq!(dec("a//b"), Err(PathError::NonCanonical));
}

fn path_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            4 => prop::sample::select(b"ab./-_+ %~[]".to_vec()),
            1 => any::<u8>(),
        ],
        0..60,
    )
}

/// Encoding fails exactly as the spec says.
fn expect_error(p: &[u8]) -> Option<PathError> {
    if p.is_empty() {
        return Some(PathError::Empty);
    }
    if p.split(|b| *b == b'/').any(|s| s.is_empty()) {
        return Some(PathError::EmptySegment);
    }
    None
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, .. ProptestConfig::default() })]

    #[test]
    fn encode_is_a_bijection_onto_what_decode_accepts(p in path_bytes()) {
        match encode(&p) {
            Ok(n) => {
                prop_assert!(expect_error(&p).is_none());
                prop_assert_eq!(validate(&n), Ok(()));
                prop_assert_eq!(decode(&n), Ok(p.clone()));
            }
            Err(e) => prop_assert_eq!(Some(e), expect_error(&p)),
        }
    }

    #[test]
    fn whatever_decodes_is_canonical(n in prop::collection::vec(prop::sample::select(b"ab./+2E0F5Bc".to_vec()), 0..40)) {
        if let Ok(p) = decode(&n) {
            prop_assert_eq!(encode(&p), Ok(n));
        }
    }
}
