#![no_main]
use libfuzzer_sys::fuzz_target;
use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::ObjectStore;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("staging")).unwrap();
    let mut f = b"P2PPACK1".to_vec();
    f.extend_from_slice(data);
    std::fs::write(d.path().join("staging/x.pack"), &f).unwrap();
    let backend: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path().join("objects")).unwrap());
    if let Ok(s) = pico2peta_pack::PackStore::open(d.path().join("staging"), backend, pico2peta_pack::PackConfig::tiny()) {
        for h in s.hashes().unwrap_or_default() {
            let _ = s.get(&h, None);
        }
    }
});
