//! Attestations and promotion end to end on a local repository.
use pico2peta_attest::{
    Ed25519Signer, Envelope, INTOTO_PAYLOAD_TYPE, Policy, PromoteOutcome, Signer, Statement, Subject, Verifier, attest,
    attestations, pae, promote,
};
use pico2peta_core::{Hash, Name, ObjectStore, Observed, Repository, Rule};
use pico2peta_prov::Record;
use pico2peta_repo::{LocalRepo, PackConfig};
use serde_json::json;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

fn statement(name: &str, h: Hash) -> Statement {
    Statement {
        subjects: vec![Subject { name: name.into(), hash: h }],
        predicate_type: "https://slsa.dev/provenance/v1".into(),
        predicate: json!({"builder": {"id": "ci://test"}}),
    }
}

#[test]
fn envelope_round_trip_sign_verify_and_tamper() {
    let alice = Ed25519Signer::from_seed(&[1; 32]);
    let bob = Ed25519Signer::from_seed(&[2; 32]);
    let st = statement("a.whl", Hash::of(b"wheel"));
    let env = Envelope::sign(&st, &[&alice, &bob]);
    let back = Envelope::from_json(&env.to_json()).unwrap();
    assert_eq!(back, env);
    assert_eq!(back.statement().unwrap(), st);
    let (va, vb) = (alice.verifier(), bob.verifier());
    let mut ids = back.verified_by(&[&va, &vb]);
    ids.sort();
    let mut want = vec![alice.keyid(), bob.keyid()];
    want.sort();
    assert_eq!(ids, want);
    // A changed payload, or a signature moved to another key id, verifies for nobody.
    let mut t = back.clone();
    t.payload[10] ^= 1;
    assert!(t.verified_by(&[&va, &vb]).is_empty());
    let mut t = back.clone();
    t.signatures[0].0 = bob.keyid();
    t.signatures.truncate(1);
    assert!(t.verified_by(&[&vb]).is_empty());
    // PAE is the DSSE v1 encoding.
    assert_eq!(pae("t", b"ab"), b"DSSEv1 1 t 2 ab".to_vec());
    assert_eq!(env.payload_type, INTOTO_PAYLOAD_TYPE);
    // Malformed envelopes are errors, not panics.
    for bad in [&b"{}"[..], b"not json", b"{\"payloadType\":\"x\",\"payload\":\"!!\",\"signatures\":[]}"] {
        assert!(Envelope::from_json(bad).is_err());
    }
}

#[test]
fn promotion_is_policy_gated_occ_and_audited() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let log = n("_prov/log");
    r.set_rule("prod", Rule::ImmutableAfterPublish).unwrap();
    let artifact = r.put(b"wheel bytes v1").unwrap();
    let dev = n("dev/app");
    r.replace(&dev, &[(dev.clone(), Observed::Absent)], artifact).unwrap();
    let ci = Ed25519Signer::from_seed(&[7; 32]);
    let rogue = Ed25519Signer::from_seed(&[8; 32]);
    let policy = Policy { required_keys: vec![ci.keyid()], predicate_type: None, require_provenance: true };
    let civ = ci.verifier();
    let verifiers: [&dyn Verifier; 1] = [&civ];
    let prod = n("prod/app");
    // No provenance and no signature: refused, with both reasons.
    match promote(&r, &log, &dev, &prod, &policy, &verifiers, "alice").unwrap() {
        PromoteOutcome::Refused(why) => assert_eq!(why.len(), 2, "{why:?}"),
        other => panic!("{other:?}"),
    }
    // Provenance (built from a source commit) and a signature by an untrusted key: still refused.
    let src = r.put(b"source tree").unwrap();
    pico2peta_prov::record(
        &r,
        &log,
        &[Record {
            op: "build".into(),
            inputs: [("source".to_string(), src)].into_iter().collect(),
            outputs: [("wheel".to_string(), artifact)].into_iter().collect(),
            ..Default::default()
        }],
    )
    .unwrap();
    attest(&r, &log, &Envelope::sign(&statement("app.whl", artifact), &[&rogue])).unwrap();
    assert!(matches!(
        promote(&r, &log, &dev, &prod, &policy, &verifiers, "alice").unwrap(),
        PromoteOutcome::Refused(_)
    ));
    // The trusted CI key signs: promoted, and the promotion is on record.
    attest(&r, &log, &Envelope::sign(&statement("app.whl", artifact), &[&ci])).unwrap();
    assert_eq!(attestations(&r, &log, &artifact).unwrap().len(), 2);
    let PromoteOutcome::Promoted { object, record } =
        promote(&r, &log, &dev, &prod, &policy, &verifiers, "alice").unwrap()
    else {
        panic!("should promote")
    };
    assert_eq!(object, artifact);
    assert_eq!(r.resolve(&prod).unwrap().unwrap().hash, artifact);
    let p = pico2peta_prov::Provenance::load(&r, &log).unwrap();
    let audit = p.get(&record).unwrap();
    assert_eq!(audit.op, "promote");
    assert_eq!(audit.meta.get("actor").map(String::as_str), Some("alice"));
    assert_eq!(audit.meta.get("dst").map(String::as_str), Some("prod/app"));
    // Cross-domain lineage: the promoted artifact traces back to its source.
    assert!(p.lineage(&artifact).unwrap().contains(&src));
    // prod/ is immutable: a second promotion of a new dev build is refused by the name rule.
    let v2 = r.put(b"wheel bytes v2").unwrap();
    let cur = r.resolve(&dev).unwrap();
    r.replace(&dev, &[(dev.clone(), pico2peta_core::RefValue::observed(cur))], v2).unwrap();
    attest(&r, &log, &Envelope::sign(&statement("app.whl", v2), &[&ci])).unwrap();
    let relaxed = Policy { require_provenance: false, ..policy.clone() };
    let out = promote(&r, &log, &dev, &prod, &relaxed, &verifiers, "alice");
    assert!(out.is_err() || matches!(out.unwrap(), PromoteOutcome::Refused(_)), "immutable prod/ keeps v1");
    assert_eq!(r.resolve(&prod).unwrap().unwrap().hash, artifact);
}
