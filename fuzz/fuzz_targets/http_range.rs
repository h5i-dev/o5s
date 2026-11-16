#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 8 {
        let size = u64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]));
        if let Ok(h) = std::str::from_utf8(&data[8..]) {
            o5s_server::fuzzing::range(h, size);
        }
    }
});
