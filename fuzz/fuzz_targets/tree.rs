#![no_main]
//! Tree objects: the kernel decoder on arbitrary bytes (never panics; what it accepts re-encodes to the
//! same bytes, and the tracer returns exactly the entries' hashes), plus the shell's `Tree` wrapper.
use libfuzzer_sys::fuzz_target;
use o5s_kernel::tree as k;

fuzz_target!(|data: &[u8]| {
    let mut with_magic = b"O5STREE1".to_vec();
    with_magic.extend_from_slice(data);
    for b in [data, &with_magic[..]] {
        let dec = k::decode(b);
        let kids = k::children(b);
        match dec {
            Ok(es) => {
                assert_eq!(k::encode(&es).as_deref(), Ok(b));
                let hs: Vec<[u8; 32]> = es.iter().map(|e| e.hash).collect();
                assert_eq!(kids, Ok(hs));
                let t = o5s_tree::Tree::decode(b).unwrap();
                assert_eq!(t.encode().unwrap(), b);
            }
            Err(_) => {
                assert!(kids.is_err());
                assert!(o5s_tree::Tree::decode(b).is_err());
            }
        }
    }
});
