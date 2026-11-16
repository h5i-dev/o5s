//! Peak memory under large streaming transfers. Its own test binary so that
//! no other test shares the process (VmHWM is process-wide).

mod common;
use common::*;
use pico2peta_core::ObjectStore;
use std::io::Read;

/// Peak RSS of this process in KiB (Linux only).
fn vm_hwm_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok())
}

/// A reader producing `len` pseudo-random bytes without holding them in memory.
struct Gen {
    left: u64,
    x: u64,
}

impl Read for Gen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        for b in &mut buf[..n] {
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            *b = (self.x >> 24) as u8;
        }
        self.left -= n as u64;
        Ok(n)
    }
}

#[test]
fn large_upload_and_download_use_bounded_memory() {
    let s = TestServer::start(|_| {});
    let c = s.client(ALICE);
    let before = vm_hwm_kib();
    let len: u64 = 512 << 20;
    let h = c.put_stream(&mut Gen { left: len, x: 12345 }).unwrap();
    keep(&c, "alice", &h);
    assert_eq!(c.size(&h).unwrap(), len);
    let mut r = c.open(&h, None).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let k = r.read(&mut buf).unwrap();
        if k == 0 {
            break;
        }
        total += k as u64;
    }
    assert_eq!(total, len); // also verified the BLAKE3 hash at EOF
    if let (Some(b), Some(a)) = (before, vm_hwm_kib()) {
        let grew_mib = (a.saturating_sub(b)) / 1024;
        eprintln!("peak RSS growth: {grew_mib} MiB (before {} MiB)", b / 1024);
        // Client + server + repo in one process, moving 512 MiB each way.
        assert!(grew_mib < 96, "peak RSS grew by {grew_mib} MiB");
    }
}
