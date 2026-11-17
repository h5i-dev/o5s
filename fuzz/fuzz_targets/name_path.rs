#![no_main]
//! The file-path ↔ name encoding: decoding never panics, and whatever it accepts is the canonical
//! encoding of its result; encoding never panics, and its output is a valid name that decodes back.
use libfuzzer_sys::fuzz_target;
use o5s_core::Name;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data)
        && let Ok(n) = Name::new(s)
        && let Ok(p) = n.to_path_bytes()
    {
        let back = Name::from_path_bytes(&p).expect("a decoded path encodes");
        assert_eq!(back.as_str(), s, "decode accepted a non-canonical name");
    }
    if let Ok(n) = Name::from_path_bytes(data) {
        assert!(Name::new(n.as_str()).is_ok());
        assert_eq!(n.to_path_bytes().expect("an encoding decodes"), data);
    }
});
