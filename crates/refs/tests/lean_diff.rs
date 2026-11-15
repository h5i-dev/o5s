//! Differential test (Cedar-style): random operation sequences run through the
//! Rust `RefStore` and through the executable Lean model
//! (`lean/.lake/build/bin/p2p-model`, built with `cd lean && lake build p2p-model`).
//! Every outcome must agree. Skipped (with a notice) when the model binary is
//! absent. Set `P2P_REQUIRE_LEAN=1` to make absence a failure (CI).
use pico2peta_core::{Error, Hash, Name, Observed, ReplaceOutcome, Rule};
use pico2peta_refs::RefStore;
use proptest::prelude::*;
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

const NAMES: &[&str] = &["a", "a/b", "p/x", "p/y/z", "m/heads/main", "m/releases/v1", "m/other", "m", "m/heads"];
const PREFIXES: &[&str] = &["a", "p", "p/y", "m", "m/heads"];

#[derive(Debug, Clone)]
enum Op {
    Put(u8),
    /// (entries: name index, Some(hash token) or None=delete), fresh read set?
    Txn(Vec<(usize, Option<u8>)>, bool),
    Delete(usize, Option<u8>, bool),
    Replace(usize, u8, Vec<(usize, Option<u8>)>),
    Rule(usize, u8),
    /// promote(src, dst): `crates/attest::promote` without its policy.
    Promote(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0u8..5).prop_map(Op::Put),
        6 => (0..NAMES.len(), 0u8..5, proptest::collection::vec((0..NAMES.len(), proptest::option::of(0u8..4)), 0..3))
            .prop_map(|(t, h, rs)| Op::Replace(t, h, rs)),
        2 => (0..PREFIXES.len(), 0u8..3).prop_map(|(p, r)| Op::Rule(p, r)),
        2 => (0..NAMES.len(), proptest::option::of(0u8..4), any::<bool>()).prop_map(|(t, o, f)| Op::Delete(t, o, f)),
        3 => (proptest::collection::vec((0..NAMES.len(), proptest::option::of(0u8..5)), 0..4), any::<bool>()).prop_map(|(e, f)| Op::Txn(e, f)),
        2 => (0..NAMES.len(), 0..NAMES.len()).prop_map(|(a, b)| Op::Promote(a, b)),
    ]
}

fn model_bin() -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lean/.lake/build/bin/p2p-model");
    if p.exists() {
        Some(p)
    } else if std::env::var("P2P_REQUIRE_LEAN").is_ok() {
        panic!("Lean model binary missing at {}", p.display());
    } else {
        None
    }
}

fn obs_str(o: Option<u8>) -> String {
    o.map_or("-".into(), |v| v.to_string())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]
    #[test]
    fn rust_matches_lean_model(ops in proptest::collection::vec(op(), 1..50), use_log in any::<bool>()) {
        let Some(bin) = model_bin() else {
            eprintln!("lean_diff: skipped (build lean/ with `lake build p2p-model`)");
            return Ok(());
        };
        // Bias read sets toward truth: the true observation of the target is
        // usually included (otherwise nearly everything is a conflict).
        let d = tempfile::tempdir().unwrap();
        // Both ref engines are checked against the Lean model.
        let r = if use_log {
            let b: std::sync::Arc<dyn pico2peta_core::backend::Backend> =
                std::sync::Arc::new(pico2peta_core::backend::LocalDisk::new(d.path()).unwrap());
            let r = RefStore::open_backend(b, "refs/").unwrap();
            r.set_compact_threshold(5);
            r
        } else {
            let r = RefStore::open(d.path()).unwrap();
            r.set_compact_threshold(400);
            r
        };
        let mut objects: HashSet<Hash> = HashSet::new();
        let tok = |i: u8| Hash::of(format!("h{i}").as_bytes());
        let mut script = String::new();
        let mut rust_out = Vec::new();
        for o in &ops {
            match o {
                Op::Put(i) => {
                    objects.insert(tok(*i));
                    script.push_str(&format!("P h{i}\n"));
                    rust_out.push("ok 0".to_string());
                }
                Op::Replace(t, h, extra) => {
                    let target = Name::new(NAMES[*t]).unwrap();
                    let mut rs: Vec<(Name, Observed)> = Vec::new();
                    let mut line = format!("R {} h{h}", NAMES[*t]);
                    if !extra.iter().any(|(i, _)| i == t) || *h % 2 == 0 {
                        let cur = r.resolve(&target).unwrap().map(|v| v.version as u8);
                        rs.push((target.clone(), cur.map_or(Observed::Absent, |v| Observed::Version(v as u64))));
                        line.push_str(&format!(" {} {}", NAMES[*t], obs_str(cur)));
                    }
                    for (i, ob) in extra {
                        rs.push((Name::new(NAMES[*i]).unwrap(), ob.map_or(Observed::Absent, |v| Observed::Version(v as u64))));
                        line.push_str(&format!(" {} {}", NAMES[*i], obs_str(*ob)));
                    }
                    script.push_str(&line);
                    script.push('\n');
                    let objs = &objects;
                    let res = r.replace(&target, &rs, tok(*h), &|x| Ok(objs.contains(x)));
                    rust_out.push(match res {
                        Ok(ReplaceOutcome::Ok { version }) => format!("ok {version}"),
                        Ok(ReplaceOutcome::Conflict { .. }) => "conflict".into(),
                        Err(e) => format!("err {}", e.code()),
                    });
                }
                Op::Txn(entries, fresh) => {
                    // Read set: each listed name, observed truly (fresh) or as "absent".
                    let mut rs: Vec<(Name, Observed)> = Vec::new();
                    let mut rs_txt = String::new();
                    let mut seen = std::collections::BTreeSet::new();
                    for (i, _) in entries {
                        if !seen.insert(*i) { continue; }
                        let nm = Name::new(NAMES[*i]).unwrap();
                        let ob = if *fresh { r.resolve(&nm).unwrap().map(|v| v.version as u8) } else { None };
                        rs.push((nm, ob.map_or(Observed::Absent, |v| Observed::Version(v as u64))));
                        rs_txt.push_str(&format!(" {} {}", NAMES[*i], obs_str(ob)));
                    }
                    let ups: Vec<(Name, Option<Hash>)> =
                        entries.iter().map(|(i, h)| (Name::new(NAMES[*i]).unwrap(), h.map(tok))).collect();
                    let mut line = format!("U {}", entries.len());
                    for (i, h) in entries {
                        line.push_str(&format!(" {} {}", NAMES[*i], h.map_or("-".to_string(), |h| format!("h{h}"))));
                    }
                    script.push_str(&line);
                    script.push_str(&rs_txt);
                    script.push('\n');
                    let objs = &objects;
                    rust_out.push(match r.update_many(&ups, &rs, &|x| Ok(objs.contains(x))) {
                        Ok(pico2peta_core::ReplaceManyOutcome::Ok { versions }) => {
                            format!("ok {}", versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","))
                        }
                        Ok(pico2peta_core::ReplaceManyOutcome::Conflict { .. }) => "conflict".into(),
                        Err(e) => format!("err {}", e.code()),
                    });
                }
                Op::Delete(t, guess, fresh) => {
                    let target = Name::new(NAMES[*t]).unwrap();
                    let ob = if *fresh { r.resolve(&target).unwrap().map(|v| v.version as u8) } else { *guess };
                    let rs = vec![(target.clone(), ob.map_or(Observed::Absent, |v| Observed::Version(v as u64)))];
                    script.push_str(&format!("D {} {} {}\n", NAMES[*t], NAMES[*t], obs_str(ob)));
                    rust_out.push(match r.delete(&target, &rs) {
                        Ok(ReplaceOutcome::Ok { version }) => format!("ok {version}"),
                        Ok(ReplaceOutcome::Conflict { .. }) => "conflict".into(),
                        Err(e) => format!("err {}", e.code()),
                    });
                }
                Op::Promote(a, b) => {
                    let (src, dst) = (Name::new(NAMES[*a]).unwrap(), Name::new(NAMES[*b]).unwrap());
                    script.push_str(&format!("M {} {}\n", NAMES[*a], NAMES[*b]));
                    rust_out.push(match r.resolve(&src).unwrap() {
                        None => "err parse".into(), // the model: promoting an absent src is invalid
                        Some(sv) => {
                            let rs = vec![
                                (src.clone(), Observed::Version(sv.version)),
                                (dst.clone(), pico2peta_core::RefValue::observed(r.resolve(&dst).unwrap())),
                            ];
                            let objs = &objects;
                            match r.update_many(&[(dst.clone(), Some(sv.hash))], &rs, &|x| Ok(objs.contains(x))) {
                                Ok(pico2peta_core::ReplaceManyOutcome::Ok { versions }) => format!(
                                    "ok {}",
                                    versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
                                ),
                                Ok(pico2peta_core::ReplaceManyOutcome::Conflict { .. }) => "conflict".into(),
                                Err(e) => format!("err {}", e.code()),
                            }
                        }
                    });
                }
                Op::Rule(p, rc) => {
                    let rule = Rule::from_code(*rc).unwrap();
                    let c = ["m", "i", "h"][*rc as usize];
                    script.push_str(&format!("S {} {c}\n", PREFIXES[*p]));
                    rust_out.push(match r.set_rule(PREFIXES[*p], rule) {
                        Ok(()) => "ok 0".into(),
                        Err(e) => format!("err {}", e.code()),
                    });
                }
            }
        }
        let mut child = Command::new(&bin).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let lean_out: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(|l| l.to_string()).collect();
        prop_assert_eq!(lean_out.len(), rust_out.len());
        for (i, (a, b)) in rust_out.iter().zip(&lean_out).enumerate() {
            prop_assert_eq!(a, b, "op {} diverged: {:?}\nscript:\n{}", i, ops[i], script);
        }
        let _ = Error::Locked;
    }
}
