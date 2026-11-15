#![no_main]
use libfuzzer_sys::fuzz_target;
use pico2peta_pack::format::{decode_record, parse_header, unpack_payload};

fuzz_target!(|data: &[u8]| {
    let _ = parse_header(data);
    if let Ok((h, p)) = decode_record(data) {
        let _ = unpack_payload(&h, p);
    }
});
