#![no_main]
//! Frame records (Frame, Frame2, Frame3 with a reference), full and prefix decoding.
use libfuzzer_sys::fuzz_target;
use o5s_pack::format::{decode_frame_bodies, decode_frame_prefix, decode_record, frame_members, frame_ref};

fuzz_target!(|data: &[u8]| {
    let Ok((h, p)) = decode_record(data) else { return };
    let reference = &data[..data.len().min(4096)];
    let _ = frame_ref(&h, p);
    let _ = decode_frame_bodies(&h, p, None);
    let _ = decode_frame_bodies(&h, p, Some(reference));
    let _ = frame_members(&h, p, &|_| Ok(reference.to_vec()));
    for need in [0usize, 1, 4096, 1 << 20] {
        let _ = decode_frame_prefix(&h, p, Some(reference), need);
    }
});
