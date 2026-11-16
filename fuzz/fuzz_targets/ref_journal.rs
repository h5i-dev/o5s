#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = o5s_refs::parse_records(data);
    let _ = o5s_refs::decode_payload(data);
    // Full open path on a journal file with this content (after the magic).
    let d = tempfile::tempdir().unwrap();
    let mut j = b"O5SREFJ1".to_vec();
    j.extend_from_slice(data);
    std::fs::write(d.path().join("journal"), &j).unwrap();
    if let Ok(r) = o5s_refs::RefStore::open(d.path()) {
        let _ = r.list("");
    }
});
