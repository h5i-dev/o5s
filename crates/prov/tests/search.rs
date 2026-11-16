//! Search over names, metadata and provenance, kept current incrementally.
use o5s_core::{Name, ObjectStore, Observed, Repository};
use o5s_prov::Record;
use o5s_prov::search::{SearchIndex, attach_meta, tokens};
use o5s_repo::{LocalRepo, PackConfig};

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
    o5s_prov::record(
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

/// Provenance across fronts: a Git commit is built into a wheel, the wheel is
/// installed into an OCI image layer, the image trains a model. Lineage of the
/// model reaches the commit; impact of the commit reaches the model. All by
/// name, through one API.
#[test]
fn lineage_and_impact_across_domains_by_name() {
    use o5s_prov::search::{impact_named, lineage_named};
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let log = n("_prov/log");
    let put_named = |name: &str, data: &[u8]| {
        let h = r.put(data).unwrap();
        r.replace(&n(name), &[(n(name), Observed::Absent)], h).unwrap();
        h
    };
    let commit = put_named("git/acme/lib/refs/heads/main", b"commit tree");
    let wheel = put_named("pypi/acmelib/1.0/acmelib-1.0-py3-none-any.whl", b"wheel");
    let layer = put_named("oci/acme/trainer/blobs/sha256-abc", b"layer");
    let model = put_named("hf/models/acme/m/main", b"checkpoint");
    let unrelated = put_named("npm/other/1.0.0", b"tarball");
    let step = |op: &str, i: (&str, o5s_core::Hash), o: (&str, o5s_core::Hash)| Record {
        op: op.into(),
        inputs: [(i.0.to_string(), i.1)].into_iter().collect(),
        outputs: [(o.0.to_string(), o.1)].into_iter().collect(),
        ..Default::default()
    };
    o5s_prov::record(
        &r,
        &log,
        &[
            step("build", ("source", commit), ("wheel", wheel)),
            step("image", ("wheel", wheel), ("layer", layer)),
            step("train", ("image", layer), ("checkpoint", model)),
        ],
    )
    .unwrap();
    let lin = lineage_named(&r, &log, &n("hf/models/acme/m/main")).unwrap();
    let names: Vec<String> = lin.iter().flat_map(|h| h.names.clone()).collect();
    for want in [
        "git/acme/lib/refs/heads/main",
        "pypi/acmelib/1.0/acmelib-1.0-py3-none-any.whl",
        "oci/acme/trainer/blobs/sha256-abc",
    ] {
        assert!(names.iter().any(|x| x == want), "lineage misses {want}: {names:?}");
    }
    assert!(!lin.iter().any(|h| h.hash == unrelated));
    let imp = impact_named(&r, &log, &n("git/acme/lib/refs/heads/main")).unwrap();
    assert!(imp.iter().any(|h| h.hash == model), "a bad commit impacts the model");
    assert!(imp.iter().any(|h| h.hash == layer) && imp.iter().any(|h| h.hash == wheel));
    assert!(lineage_named(&r, &log, &n("nope/x")).is_err());
}
