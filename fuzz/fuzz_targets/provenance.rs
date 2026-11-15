#![no_main]
//! Provenance records and log nodes.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(r) = pico2peta_prov::Record::decode(data) {
        let _ = r.validate();
        assert_eq!(pico2peta_prov::Record::decode(&r.encode()).ok(), Some(r));
    }
    let _ = pico2peta_prov::LogNode::decode(data);
});
