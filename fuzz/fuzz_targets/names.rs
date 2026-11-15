#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = pico2peta_core::Name::new(s);
        let _ = s.parse::<pico2peta_core::Hash>();
        let _ = s.parse::<pico2peta_core::Rule>();
        let _ = pico2peta_core::backend::validate_key(s);
    }
});
