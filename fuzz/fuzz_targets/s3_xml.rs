#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    pico2peta_backend_s3::fuzzing::xml(data);
});
