use pico2peta_core::{Hash, Name, ObjectStore, Observed, Repository};
use pico2peta_prov::{DEFAULT_LOG_REF, LogNode, ProvTracer, Provenance, Record, record};
use pico2peta_repo::{GcOptions, LocalRepo, OpaqueObjects, PackConfig};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::time::Duration;

fn log() -> Name {
    Name::new(DEFAULT_LOG_REF).unwrap()
}

fn rec(op: &str, ins: &[(&str, Hash)], outs: &[(&str, Hash)]) -> Record {
    Record {
        op: op.into(),
        params: None,
        inputs: ins.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        outputs: outs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        meta: BTreeMap::new(),
    }
}

#[test]
fn lineage_impact_and_regeneration_aware_gc() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let data = r.put(b"dataset v1").unwrap();
    let code = r.put(b"train.py").unwrap();
    let model = r.put(b"weights from dataset v1").unwrap();
    let quant = r.put(b"int8 weights").unwrap();
    record(&r, &log(), &[rec("train", &[("data", data), ("code", code)], &[("model", model)])]).unwrap();
    record(&r, &log(), &[rec("quantize", &[("model", model)], &[("q", quant)])]).unwrap();
    // Recording the same derivation twice is a no-op in the view.
    record(&r, &log(), &[rec("quantize", &[("model", model)], &[("q", quant)])]).unwrap();

    let p = Provenance::load(&r, &log()).unwrap();
    assert_eq!(p.len(), 3, "len counts recordings (the duplicate is recorded twice)");
    assert_eq!(p.producers(&quant).unwrap().len(), 1, "but queries see one distinct record");
    assert_eq!(p.lineage(&quant).unwrap(), [data, code, model].into_iter().collect());
    assert_eq!(p.impact(&data).unwrap(), [model, quant].into_iter().collect());
    assert!(p.regenerable(&r, &quant).unwrap().is_some());

    // Refs: the published quantized model and the provenance log. `model` is
    // referenced by nothing except provenance as an input, so it stays.
    let rel = Name::new("models/releases/q1").unwrap();
    r.replace(&rel, &[(rel.clone(), Observed::Absent)], quant).unwrap();
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, ..GcOptions::default() };
    let tracer = ProvTracer { inner: OpaqueObjects };
    for _ in 0..3 {
        r.gc(&opts, &tracer).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    for h in [data, code, model, quant] {
        assert!(fresh.contains(&h).unwrap(), "inputs and referenced outputs survive GC");
    }

    // Unpublish (point the head elsewhere) and add a derived artifact that only
    // provenance mentions as an *output*: GC may drop it, and it's regenerable.
    let scratch = r.put(b"scratch eval report").unwrap();
    record(&r, &log(), &[rec("eval", &[("q", quant)], &[("report", scratch)])]).unwrap();
    for _ in 0..3 {
        r.gc(&opts, &tracer).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    assert!(!fresh.contains(&scratch).unwrap(), "outputs alone are not kept alive by provenance");
    let p = Provenance::load(&fresh, &log()).unwrap();
    assert!(p.regenerable(&fresh, &scratch).unwrap().is_some(), "but they are regenerable");
}

#[test]
fn dangling_records_refused() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let x = r.put(b"x").unwrap();
    let missing = Hash::of(b"never stored");
    assert!(record(&r, &log(), &[rec("op", &[("in", missing)], &[("out", x)])]).is_err());
}

#[test]
fn concurrent_recorders_all_land() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let base = r.put(b"base").unwrap();
    r.flush().unwrap();
    let root = d.path().to_path_buf();
    std::thread::scope(|sc| {
        for t in 0..4 {
            let root = root.clone();
            sc.spawn(move || {
                let w = LocalRepo::open(&root).unwrap();
                for i in 0..10 {
                    let out = w.put(format!("out {t} {i}").as_bytes()).unwrap();
                    record(&w, &log(), &[rec("op", &[("in", base)], &[("out", out)])]).unwrap();
                }
            });
        }
    });
    let p = Provenance::load(&r, &log()).unwrap();
    assert_eq!(p.len(), 40);
    assert!(!p.is_empty());
    assert_eq!(p.impact(&base).unwrap().len(), 40);
}

proptest! {
    #[test]
    fn record_codec_roundtrip_and_total(op in "[a-z@.0-9]{0,12}",
                                        ins in proptest::collection::btree_map("[a-z]{1,4}", any::<[u8; 32]>(), 0..4),
                                        meta in proptest::collection::btree_map("[a-z]{1,4}", ".{0,8}", 0..3),
                                        junk in proptest::collection::vec(any::<u8>(), 0..64)) {
        let r = Record { op, params: None, inputs: ins.into_iter().map(|(k, v)| (k, Hash(v))).collect(), outputs: BTreeMap::new(), meta };
        prop_assert_eq!(Record::decode(&r.encode()).unwrap(), r.clone());
        let mut b = r.encode();
        b.extend_from_slice(&junk);
        if !junk.is_empty() { prop_assert!(Record::decode(&b).is_err()); }
        let _ = Record::decode(&junk);
        let _ = LogNode::decode(&junk);
    }
}

/// Thousands of records: checkpoints are created, old records are found via the
/// index (ranged binary search), recent ones from memory, and results match.
#[test]
fn checkpointed_log_scales_and_matches() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let root = r.put(b"root dataset").unwrap();
    let mut prev = root;
    let mut chain = Vec::new();
    // 3000 derivations in batches of 50: a linear chain root -> o1 -> o2 -> ...
    for batch in 0..60 {
        let mut recs = Vec::new();
        for i in 0..50 {
            let out = r.put(format!("out {batch} {i}").as_bytes()).unwrap();
            recs.push(rec("step", &[("in", prev)], &[("out", out)]));
            chain.push(out);
            prev = out;
        }
        record(&r, &log(), &recs).unwrap();
    }
    let p = Provenance::load(&r, &log()).unwrap();
    assert_eq!(p.len(), 3000);
    // Oldest link (in a checkpoint) and newest (recent) both resolve.
    assert_eq!(p.producers(&chain[0]).unwrap().len(), 1);
    assert_eq!(p.producers(&chain[2999]).unwrap().len(), 1);
    assert_eq!(p.consumers(&root).unwrap().len(), 1);
    let lin = p.lineage(&chain[1500]).unwrap();
    assert_eq!(lin.len(), 1501, "root plus 1500 predecessors");
    assert_eq!(p.impact(&chain[2990]).unwrap().len(), 9);
    // The head node's walk stops at a checkpoint: memory holds only recent records.
    let head = r.resolve(&log()).unwrap().unwrap().hash;
    let mut n = LogNode::decode(&r.get(&head, None).unwrap()).unwrap();
    let mut walked = 0;
    while n.checkpoint.is_none() {
        walked += 1;
        n = LogNode::decode(&r.get(&n.prev.unwrap(), None).unwrap()).unwrap();
    }
    assert!(walked < 20, "checkpoints keep the recent tail short ({walked} nodes)");
    // GC with the provenance tracer keeps everything referenced from the log, including checkpoints.
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, ..GcOptions::default() };
    for _ in 0..3 {
        r.gc(&opts, &ProvTracer { inner: OpaqueObjects }).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    let p = Provenance::load(&fresh, &log()).unwrap();
    assert_eq!(p.lineage(&chain[100]).unwrap().len(), 101);
}
