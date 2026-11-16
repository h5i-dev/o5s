#![no_main]
use libfuzzer_sys::fuzz_target;
use o5s_pack::format::{IdxV3View, MidxView};

fuzz_target!(|data: &[u8]| {
    if let Ok(v) = MidxView::new(data) {
        let _ = v.find_all(&o5s_core::Hash::of(b"probe"));
    }
    if let Ok(v) = IdxV3View::new(data) {
        let _ = v.find(&o5s_core::Hash::of(b"probe"));
        for i in 0..v.len().min(64) {
            let _ = v.entry(i);
        }
    }
});
