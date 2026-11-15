#![no_main]
//! The search index and provenance queries over a log whose records and
//! checkpoint index are attacker bytes. The input is split on `\xff\xfe`:
//! every part but the last is stored as a record object, the last as the
//! checkpoint index; a valid log node links them (hashes cannot be guessed,
//! so the node itself is built here). Indexing, text/prefix search and
//! lineage/impact must return errors, never panic or loop.
use libfuzzer_sys::fuzz_target;
use pico2peta_core::{Name, Observed, ObjectStore, Repository};

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let repo = pico2peta_repo::LocalRepo::init(dir.path(), &Default::default()).unwrap();
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut rest = data;
    while let Some(i) = rest.windows(2).position(|w| w == b"\xff\xfe") {
        parts.push(&rest[..i]);
        rest = &rest[i + 2..];
    }
    parts.push(rest);
    if parts.len() > 32 {
        return;
    }
    let checkpoint_bytes = parts.pop().unwrap();
    let records: Vec<_> = parts.iter().map(|p| repo.put(p).unwrap()).collect();
    let checkpoint = (!checkpoint_bytes.is_empty()).then(|| repo.put(checkpoint_bytes).unwrap());
    let node = pico2peta_prov::LogNode { prev: None, records: records.clone(), checkpoint, total: records.len() as u64 };
    let head = repo.put(&node.encode()).unwrap();
    let log = Name::new(pico2peta_prov::DEFAULT_LOG_REF).unwrap();
    repo.replace(&log, &[(log.clone(), Observed::Absent)], head).unwrap();
    if let Ok(idx) = pico2peta_prov::search::SearchIndex::build(&repo, &log) {
        let q = String::from_utf8_lossy(data);
        let _ = idx.text(&q);
        let _ = idx.prefix(&q);
        for r in &records {
            let _ = idx.metadata(r);
        }
    }
    if let Ok(p) = pico2peta_prov::Provenance::load(&repo, &log) {
        for h in records.iter().chain([head].iter()) {
            let _ = p.lineage(h);
            let _ = p.impact(h);
        }
    }
});
