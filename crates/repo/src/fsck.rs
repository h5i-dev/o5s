//! Repository integrity check: every stored object is read back and verified
//! (record CRC and content hash, through the normal read path), and every ref
//! must point at a readable object.

use crate::LocalRepo;
use o5s_core::{Hash, ObjectStore, Repository, Result};
use std::io::Read;

/// Result of [`LocalRepo::fsck`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FsckReport {
    /// Addressable hashes verified (objects, chunks, manifest nodes).
    pub objects_checked: u64,
    /// Bytes read back and verified.
    pub bytes_checked: u64,
    /// Refs checked.
    pub refs_checked: u64,
    /// Problems found (at most 1000 are listed).
    pub errors: Vec<String>,
}

impl FsckReport {
    /// True if no problem was found.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
    fn err(&mut self, e: String) {
        if self.errors.len() < 1000 {
            self.errors.push(e);
        }
    }
}

impl LocalRepo {
    /// Verifies every object and ref. Cost is O(stored data): it reads everything.
    pub fn fsck(&self) -> Result<FsckReport> {
        let mut rep = FsckReport::default();
        self.packs().refresh()?;
        let mut buf = vec![0u8; 1 << 20];
        for h in self.packs().hashes()? {
            rep.objects_checked += 1;
            match self.open(&h, None) {
                Ok(mut r) => {
                    // Whole-object reads verify the content hash at EOF.
                    loop {
                        match r.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => rep.bytes_checked += n as u64,
                            Err(e) => {
                                rep.err(format!("object {h}: {e}"));
                                break;
                            }
                        }
                    }
                }
                Err(e) => rep.err(format!("object {h}: {e}")),
            }
        }
        for q in self.packs().quarantined()? {
            rep.err(format!("quarantined corrupt staging file {}", q.display()));
        }
        for name in self.packs().orphan_indexes()? {
            rep.err(format!("index without pack: packs/{name}.idx"));
        }
        for (name, v) in self.list_refs("")? {
            rep.refs_checked += 1;
            match self.size(&v.hash) {
                Ok(_) => {}
                Err(e) => rep.err(format!("ref {name} -> {}: {e}", short(&v.hash))),
            }
        }
        Ok(rep)
    }
}

fn short(h: &Hash) -> String {
    h.to_hex()[..16].to_string()
}
