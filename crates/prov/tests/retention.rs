//! Retention per namespace profile, then GC.
use pico2peta_core::{Name, ObjectStore, Observed, Repository, Rule};
use pico2peta_prov::Record;
use pico2peta_prov::retention::{apply, version_cmp};
use pico2peta_repo::{GcOptions, LocalRepo, PackConfig, Profile, RepoConfig};
use std::cmp::Ordering;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

#[test]
fn version_order_is_numeric_aware() {
    assert_eq!(version_cmp("1.10", "1.9"), Ordering::Greater);
    assert_eq!(version_cmp("1.0.0", "1.0"), Ordering::Greater);
    assert_eq!(version_cmp("v2", "v10"), Ordering::Less);
    assert_eq!(version_cmp("1.0a", "1.0b"), Ordering::Less);
    assert_eq!(version_cmp("007", "7"), version_cmp("7", "7").then(Ordering::Less));
}

#[test]
fn keeps_newest_n_respects_rules_and_regenerability_then_gc_collects() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = RepoConfig::new(&PackConfig::tiny());
    cfg.profiles.insert("pkgs".into(), Profile { keep_versions: Some(2), ..Default::default() });
    cfg.profiles
        .insert("models".into(), Profile { keep_versions: Some(1), only_if_regenerable: true, ..Default::default() });
    cfg.namespaces.insert("pypi/".into(), "pkgs".into());
    cfg.namespaces.insert("models/".into(), "models".into());
    let r = LocalRepo::init_config(d.path(), &cfg).unwrap();
    let log = n("_prov/log");
    let set = |name: &str, data: &[u8]| {
        let h = r.put(data).unwrap();
        r.replace(&n(name), &[(n(name), Observed::Absent)], h).unwrap();
        h
    };
    let old_wheel = set("pypi/a/1.0", b"a 1.0 wheel bytes");
    set("pypi/a/1.9", b"a 1.9 wheel bytes");
    set("pypi/a/1.10", b"a 1.10 wheel bytes");
    set("pypi/a/2.0", b"a 2.0 wheel bytes");
    // Series pypi/b is published forever (immutable): its old versions stay.
    r.set_rule("pypi/b", Rule::ImmutableAfterPublish).unwrap();
    for v in ["1", "2", "3"] {
        set(&format!("pypi/b/{v}"), format!("b {v}").as_bytes());
    }
    // models/: two old checkpoints, only one regenerable from stored inputs.
    let data = r.put(b"training data").unwrap();
    let regen = set("models/m/1", b"checkpoint 1 (regenerable)");
    pico2peta_prov::record(
        &r,
        &log,
        &[Record {
            op: "train".into(),
            inputs: [("data".to_string(), data)].into_iter().collect(),
            outputs: [("ckpt".to_string(), regen)].into_iter().collect(),
            ..Default::default()
        }],
    )
    .unwrap();
    set("models/m/2", b"checkpoint 2 (no provenance)");
    set("models/m/3", b"checkpoint 3 (newest)");
    // Dry run: nothing changes.
    let dry = apply(&r, &log, true).unwrap();
    assert_eq!(dry.deleted.len(), 3, "{dry:?}");
    assert!(r.resolve(&n("pypi/a/1.0")).unwrap().is_some());
    let rep = apply(&r, &log, false).unwrap();
    let mut del = rep.deleted.clone();
    del.sort();
    assert_eq!(del, vec!["models/m/1", "pypi/a/1.0", "pypi/a/1.9"], "{rep:?}");
    assert_eq!(rep.kept_by_rule, vec!["pypi/b/1".to_string()]);
    assert_eq!(rep.kept_not_regenerable, vec!["models/m/2".to_string()]);
    for keep in ["pypi/a/1.10", "pypi/a/2.0", "pypi/b/1", "pypi/b/2", "pypi/b/3", "models/m/2", "models/m/3"] {
        assert!(r.resolve(&n(keep)).unwrap().is_some(), "{keep}");
    }
    // A second pass is a no-op.
    assert!(apply(&r, &log, false).unwrap().deleted.is_empty());
    // GC then collects the deleted versions' objects (nothing else references them).
    r.flush().unwrap();
    let opts = GcOptions { grace: std::time::Duration::ZERO, allow_short_grace: true, ..GcOptions::default() };
    let tracer = pico2peta_prov::ProvTracer { inner: pico2peta_repo::OpaqueObjects };
    for _ in 0..3 {
        r.gc(&opts, &tracer).unwrap();
    }
    let r = LocalRepo::open(d.path()).unwrap();
    assert!(!r.contains(&old_wheel).unwrap(), "collected after retention");
}
