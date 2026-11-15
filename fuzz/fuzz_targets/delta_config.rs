#![no_main]
//! Delta records and repository config (config.toml, written by operators).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pico2peta_pack::format::decode_delta(data);
    if let Ok(s) = std::str::from_utf8(data) {
        let d = std::env::temp_dir().join(format!("p2p-fuzz-cfg-{}", std::process::id()));
        if std::fs::create_dir_all(&d).is_ok() && std::fs::write(d.join("config.toml"), s).is_ok() {
            let _ = pico2peta_repo::LocalRepo::read_config(&d);
        }
    }
});
