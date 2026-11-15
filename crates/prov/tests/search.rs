//! Search over names, metadata and provenance, kept current incrementally.
use pico2peta_core::{Name, ObjectStore, Observed, Repository};
use pico2peta_prov::Record;
use pico2peta_prov::search::{SearchIndex, attach_meta, tokens};
use pico2peta_repo::{LocalRepo, PackConfig};

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

#[test]
fn names_metadata_and_provenance_are_searchable_and_refresh_incrementally() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let log = n("_prov/log");
    let wheel = r.put(b"wheel").unwrap();
    let model = r.put(b"model").unwrap();
    let commit = r.put(b"commit").unwrap();
    for (name, h) in [("pypi/torchlite/1.0/torchlite-1.0-py3-none-any.whl", wheel), ("hf/models/acme/tiny/main", model)]
    {
        r.replace(&n(name), &[(n(name), Observed::Absent)], h).unwrap();
    }
    attach_meta(&r, &log, wheel, [("summary".into(), "A tiny tensor library".into())].into_iter().collect()).unwrap();
    pico2peta_prov::record(
        &r,
        &log,
        &[Record {
            op: "train".into(),
            inputs: [("code".to_string(), commit)].into_iter().collect(),
            outputs: [("checkpoint".to_string(), model)].into_iter().collect(),
            meta: [("dataset".to_string(), "fineweb-edu sample".to_string())].into_iter().collect(),
            ..Default::default()
        }],
    )
    .unwrap();
    let mut s = SearchIndex::build(&r, &log).unwrap();
    assert_eq!(s.exact("hf/models/acme/tiny/main"), Some(model));
    assert_eq!(s.prefix("pypi/").len(), 1);
    assert!(s.prefix("npm/").is_empty());
    // Full text: name tokens, metadata tokens, AND across tokens, case-insensitive.
    let hits = s.text("Tensor library");
    assert_eq!(hits.iter().map(|h| h.hash).collect::<Vec<_>>(), vec![wheel]);
    assert_eq!(hits[0].names, vec!["pypi/torchlite/1.0/torchlite-1.0-py3-none-any.whl".to_string()]);
    assert_eq!(
        s.text("fineweb").iter().map(|h| h.hash).collect::<Vec<_>>(),
        vec![model],
        "a build's meta describes its output"
    );
    assert_eq!(s.text("acme tiny").len(), 1);
    assert!(s.text("tensor fineweb").is_empty());
    assert!(s.text("").is_empty());
    let indexed = s.records();
    // Incremental: new names and records appear after refresh, old ones aren't re-read.
    let pkg = r.put(b"npm tarball").unwrap();
    r.replace(&n("npm/leftpad/1.3.0"), &[(n("npm/leftpad/1.3.0"), Observed::Absent)], pkg).unwrap();
    attach_meta(&r, &log, pkg, [("keywords".into(), "string padding".into())].into_iter().collect()).unwrap();
    assert!(s.text("padding").is_empty(), "not visible before refresh");
    s.refresh(&r, &log).unwrap();
    assert_eq!(s.records(), indexed + 1, "only the new record was indexed");
    assert_eq!(s.text("padding")[0].hash, pkg);
    assert_eq!(s.prefix("npm/").len(), 1);
    assert!(s.metadata(&pkg).iter().any(|(op, k, _)| op == "meta" && k == "keywords"));
    assert_eq!(tokens("Torch-Lite_v2.0"), vec!["torch", "lite", "v2", "0"]);
}
