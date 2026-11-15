use pico2peta_core::fault::Faults;
use pico2peta_core::{Error, Hash, Name, Observed, RefValue, ReplaceOutcome, Result, Rule};
use pico2peta_refs::RefStore;
use proptest::prelude::*;
use std::collections::BTreeMap;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}
fn h(i: u64) -> Hash {
    Hash::of(&i.to_le_bytes())
}
fn yes(_: &Hash) -> Result<bool> {
    Ok(true)
}

#[test]
fn basic_occ() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    let main = n("src/main");
    assert_eq!(
        r.replace(&main, &[(main.clone(), Observed::Absent)], h(1), &yes).unwrap(),
        ReplaceOutcome::Ok { version: 1 }
    );
    // Stale writer loses.
    let out = r.replace(&main, &[(main.clone(), Observed::Absent)], h(2), &yes).unwrap();
    assert_eq!(out, ReplaceOutcome::Conflict { stale: vec![(main.clone(), Observed::Version(1))] });
    assert_eq!(
        r.replace(&main, &[(main.clone(), Observed::Version(1))], h(2), &yes).unwrap(),
        ReplaceOutcome::Ok { version: 2 }
    );
    assert_eq!(r.resolve(&main).unwrap(), Some(RefValue { hash: h(2), version: 2 }));
    // A read-set entry on another name also guards.
    let other = n("src/other");
    let out = r
        .replace(&main, &[(main.clone(), Observed::Version(2)), (other.clone(), Observed::Version(9))], h(3), &yes)
        .unwrap();
    assert!(matches!(out, ReplaceOutcome::Conflict { .. }));
    // Target must be in the read set; object must exist.
    assert!(matches!(
        r.replace(&main, &[(other.clone(), Observed::Absent)], h(3), &yes),
        Err(Error::TargetNotInReadSet(_))
    ));
    assert!(matches!(
        r.replace(&main, &[(main.clone(), Observed::Version(2))], h(3), &|_| Ok(false)),
        Err(Error::MissingObject(_))
    ));
}

#[test]
fn rules() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap();
    r.set_rule("model", Rule::HeadMutableReleaseImmutable).unwrap();
    let p = n("pkg/foo/1.0");
    assert!(matches!(r.replace(&p, &[(p.clone(), Observed::Absent)], h(1), &yes).unwrap(), ReplaceOutcome::Ok { .. }));
    assert!(matches!(r.replace(&p, &[(p.clone(), Observed::Version(1))], h(2), &yes), Err(Error::Forbidden { .. })));
    // A stale read is a Conflict, not Forbidden (validation first).
    assert!(matches!(
        r.replace(&p, &[(p.clone(), Observed::Absent)], h(2), &yes).unwrap(),
        ReplaceOutcome::Conflict { .. }
    ));
    let head = n("model/heads/main");
    let rel = n("model/releases/v1");
    let bad = n("model/tmp");
    r.replace(&head, &[(head.clone(), Observed::Absent)], h(1), &yes).unwrap();
    r.replace(&head, &[(head.clone(), Observed::Version(1))], h(2), &yes).unwrap();
    r.replace(&rel, &[(rel.clone(), Observed::Absent)], h(2), &yes).unwrap();
    assert!(matches!(
        r.replace(&rel, &[(rel.clone(), Observed::Version(1))], h(3), &yes),
        Err(Error::Forbidden { .. })
    ));
    assert!(matches!(r.replace(&bad, &[(bad.clone(), Observed::Absent)], h(3), &yes), Err(Error::Forbidden { .. })));
    // Rules can't be changed (or newly scoped) over existing names.
    assert!(matches!(r.set_rule("pkg", Rule::Mutable), Err(Error::RuleChangeOnNonEmpty(_))));
    assert!(matches!(r.set_rule("pkg/foo", Rule::Mutable), Err(Error::RuleChangeOnNonEmpty(_))));
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap(); // idempotent
    r.set_rule("pkg/empty", Rule::Mutable).unwrap();
    // Persisted.
    drop(r);
    let r = RefStore::open(d.path()).unwrap();
    assert_eq!(r.rules().unwrap().len(), 3);
    assert_eq!(r.resolve(&head).unwrap().unwrap().version, 2);
}

#[test]
fn two_handles_see_each_other() {
    let d = tempfile::tempdir().unwrap();
    let a = RefStore::open(d.path()).unwrap();
    let b = RefStore::open(d.path()).unwrap();
    let x = n("x");
    a.replace(&x, &[(x.clone(), Observed::Absent)], h(1), &yes).unwrap();
    assert_eq!(b.resolve(&x).unwrap().unwrap().hash, h(1));
    // b's stale view is caught at commit time.
    assert!(matches!(
        b.replace(&x, &[(x.clone(), Observed::Absent)], h(2), &yes).unwrap(),
        ReplaceOutcome::Conflict { .. }
    ));
}

/// Contended OCC counter across independent handles (separate flocks):
/// no lost updates. Successes == final version, values form one chain.
#[test]
fn contention_no_lost_updates() {
    let d = tempfile::tempdir().unwrap();
    let c = n("counter");
    let per = 40u64;
    let threads = 6u64;
    let wins: u64 = std::thread::scope(|sc| {
        (0..threads)
            .map(|t| {
                let dir = d.path().to_path_buf();
                let c = c.clone();
                sc.spawn(move || {
                    let r = RefStore::open(&dir).unwrap();
                    let mut wins = 0;
                    let mut i = 0;
                    while i < per {
                        let obs = RefValue::observed(r.resolve(&c).unwrap());
                        match r.replace(&c, &[(c.clone(), obs)], h(t * 1000 + i), &yes).unwrap() {
                            ReplaceOutcome::Ok { .. } => {
                                wins += 1;
                                i += 1;
                            }
                            ReplaceOutcome::Conflict { .. } => {}
                        }
                    }
                    wins
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|j| j.join().unwrap())
            .sum()
    });
    let r = RefStore::open(d.path()).unwrap();
    assert_eq!(wins, threads * per);
    assert_eq!(r.resolve(&c).unwrap().unwrap().version, wins);
}

#[test]
fn crash_at_every_fault_point() {
    let names: Vec<Name> = (0..5).map(|i| n(&format!("r/{i}"))).collect();
    for budget in 0..12u64 {
        let d = tempfile::tempdir().unwrap();
        let mut committed: BTreeMap<Name, RefValue> = BTreeMap::new();
        {
            let r = RefStore::open_with_faults(d.path(), Faults::crash_after(budget)).unwrap();
            for (i, nm) in names.iter().enumerate() {
                match r.replace(nm, &[(nm.clone(), Observed::Absent)], h(i as u64), &yes) {
                    Ok(ReplaceOutcome::Ok { version }) => {
                        committed.insert(nm.clone(), RefValue { hash: h(i as u64), version });
                    }
                    Ok(c) => panic!("unexpected {c:?}"),
                    Err(_) => break,
                }
            }
        }
        let r = RefStore::open(d.path()).unwrap();
        let got: BTreeMap<Name, RefValue> = r.list("").unwrap().into_iter().collect();
        // Every acknowledged commit survives; at most one unacknowledged one
        // (written, crash before fsync returned) may appear.
        for (k, v) in &committed {
            assert_eq!(got.get(k), Some(v), "budget {budget}");
        }
        assert!(got.len() <= committed.len() + 1, "budget {budget}");
        // Still writable after recovery (also truncates any torn tail).
        let z = n("z");
        r.replace(&z, &[(z.clone(), Observed::Absent)], h(99), &yes).unwrap();
        drop(r);
        let r = RefStore::open(d.path()).unwrap();
        assert_eq!(r.resolve(&z).unwrap().unwrap().hash, h(99), "budget {budget}");
    }
}

#[test]
fn garbage_journal_rejected() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("journal"), b"not a journal at all").unwrap();
    assert!(matches!(RefStore::open(d.path()), Err(Error::Corrupt(_))));
}

// ---------------------------------------------------------------------------
// Differential test vs. an independent, naive model.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Model {
    refs: BTreeMap<String, (Hash, u64)>,
    rules: BTreeMap<String, Rule>,
}

impl Model {
    fn rule_for(&self, name: &str) -> (Rule, String) {
        let mut best: Option<(&String, Rule)> = None;
        for (p, r) in &self.rules {
            if (name == p || name.starts_with(&format!("{p}/"))) && best.is_none_or(|(b, _)| p.len() > b.len()) {
                best = Some((p, *r));
            }
        }
        best.map(|(p, r)| (r, p.clone())).unwrap_or((Rule::Mutable, String::new()))
    }
    fn obs(&self, n: &str) -> Observed {
        self.refs.get(n).map_or(Observed::Absent, |v| Observed::Version(v.1))
    }
    fn replace(&mut self, name: &str, rs: &[(String, Observed)], new: Hash) -> std::result::Result<bool, &'static str> {
        if !rs.iter().any(|(n, _)| n == name) {
            return Err("target_not_in_read_set");
        }
        if rs.iter().any(|(n, o)| self.obs(n) != *o) {
            return Ok(false);
        }
        let (rule, p) = self.rule_for(name);
        let rest = if p.is_empty() { name.to_string() } else { name[p.len()..].trim_start_matches('/').to_string() };
        let exists = self.refs.contains_key(name);
        let allowed = match rule {
            Rule::Mutable => true,
            Rule::ImmutableAfterPublish => !exists,
            Rule::HeadMutableReleaseImmutable => {
                (rest.starts_with("heads/") && rest.len() > 6)
                    || (rest.starts_with("releases/") && rest.len() > 9 && !exists)
            }
        };
        if !allowed {
            return Err("forbidden");
        }
        let v = self.refs.get(name).map_or(1, |v| v.1 + 1);
        self.refs.insert(name.to_string(), (new, v));
        Ok(true)
    }
    fn set_rule(&mut self, p: &str, r: Rule) -> std::result::Result<(), &'static str> {
        if self.rules.get(p) == Some(&r) {
            return Ok(());
        }
        if self.refs.keys().any(|n| n == p || n.starts_with(&format!("{p}/"))) {
            return Err("rule_change_on_non_empty");
        }
        self.rules.insert(p.to_string(), r);
        Ok(())
    }
}

const NAMES: &[&str] = &["a", "a/b", "p/x", "p/y/z", "m/heads/main", "m/releases/v1", "m/other", "m"];
const PREFIXES: &[&str] = &["a", "p", "p/y", "m"];

#[derive(Debug, Clone)]
enum Op {
    Replace(usize, Vec<(usize, Option<u8>)>, u8, bool),
    Rule(usize, u8),
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..NAMES.len(), proptest::collection::vec((0..NAMES.len(), proptest::option::of(0u8..4)), 0..3), any::<u8>(), any::<bool>())
            .prop_map(|(t, rs, v, fresh)| Op::Replace(t, rs, v, fresh)),
        2 => (0..PREFIXES.len(), 0u8..3).prop_map(|(p, r)| Op::Rule(p, r)),
        1 => Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]
    #[test]
    fn matches_model(ops in proptest::collection::vec(op(), 1..60)) {
        let d = tempfile::tempdir().unwrap();
        let mut r = RefStore::open(d.path()).unwrap();
        r.set_compact_threshold(300); // rotate every few records
        let mut m = Model::default();
        for o in ops {
            match o {
                Op::Replace(t, extra, v, fresh) => {
                    let target = NAMES[t];
                    // Usually include the target with its true observation (fresh) or a guess.
                    let mut rs: Vec<(String, Observed)> = Vec::new();
                    if fresh { rs.push((target.to_string(), m.obs(target))); }
                    for (i, o) in extra {
                        rs.push((NAMES[i].to_string(), o.map_or(Observed::Absent, |v| Observed::Version(v as u64))));
                    }
                    let expect = m.replace(target, &rs, h(v as u64));
                    let rs2: Vec<(Name, Observed)> = rs.iter().map(|(n2, o)| (n(n2), *o)).collect();
                    let got = r.replace(&n(target), &rs2, h(v as u64), &yes);
                    match (expect, got) {
                        (Ok(true), Ok(ReplaceOutcome::Ok { .. })) => {}
                        (Ok(false), Ok(ReplaceOutcome::Conflict { .. })) => {}
                        (Err(code), Err(e)) => prop_assert_eq!(code, e.code()),
                        (e, g) => prop_assert!(false, "model {:?} vs impl {:?}", e, g),
                    }
                }
                Op::Rule(p, rc) => {
                    let rule = Rule::from_code(rc).unwrap();
                    let expect = m.set_rule(PREFIXES[p], rule);
                    let got = r.set_rule(PREFIXES[p], rule).map_err(|e| e.code());
                    prop_assert_eq!(expect, got);
                }
                Op::Reopen => {
                    drop(r);
                    r = RefStore::open(d.path()).unwrap();
                    r.set_compact_threshold(300);
                }
            }
            let got: Vec<(String, Hash, u64)> = r.list("").unwrap().into_iter().map(|(k, v)| (k.to_string(), v.hash, v.version)).collect();
            let want: Vec<(String, Hash, u64)> = m.refs.iter().map(|(k, v)| (k.clone(), v.0, v.1)).collect();
            prop_assert_eq!(got, want);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]
    #[test]
    fn journal_decoder_total(raw in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = pico2peta_refs::parse_records(&raw);
        let _ = pico2peta_refs::decode_payload(&raw);
    }

    /// Opening a journal with arbitrary bytes after a valid prefix never panics.
    #[test]
    fn journal_open_total(tail in proptest::collection::vec(any::<u8>(), 0..400)) {
        let d = tempfile::tempdir().unwrap();
        {
            let r = RefStore::open(d.path()).unwrap();
            let x = n("x");
            r.replace(&x, &[(x.clone(), Observed::Absent)], h(1), &yes).unwrap();
        }
        let mut j = std::fs::read(d.path().join("journal")).unwrap();
        j.extend_from_slice(&tail);
        std::fs::write(d.path().join("journal"), &j).unwrap();
        if let Ok(r) = RefStore::open(d.path()) {
            // The valid prefix is intact.
            prop_assert_eq!(r.resolve(&n("x")).unwrap().map(|v| v.hash), Some(h(1)));
            let y = n("y");
            let _ = r.replace(&y, &[(y.clone(), Observed::Absent)], h(2), &yes);
        }
    }
}

#[test]
fn compaction_preserves_state_and_bounds_files() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    r.set_compact_threshold(1000);
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap();
    let lagging = RefStore::open(d.path()).unwrap(); // never commits; will lag many generations
    for i in 0..300u64 {
        let nm = n(&format!("b/{}", i % 7));
        let obs = RefValue::observed(r.resolve(&nm).unwrap());
        r.replace(&nm, &[(nm.clone(), obs)], h(i), &yes).unwrap();
    }
    let p = n("pkg/x");
    r.replace(&p, &[(p.clone(), Observed::Absent)], h(1), &yes).unwrap();
    assert!(r.generation().unwrap() >= 5, "should have rotated several times");
    let files = std::fs::read_dir(d.path()).unwrap().count();
    assert!(files <= 6, "old generations are cleaned up ({files} files)");
    let want = r.snapshot().unwrap();
    assert_eq!(lagging.snapshot().unwrap(), want, "lagging handle catches up across rotations");
    assert_eq!(RefStore::open(d.path()).unwrap().snapshot().unwrap(), want);
    // Rules survive compaction.
    assert!(matches!(r.replace(&p, &[(p.clone(), Observed::Version(1))], h(2), &yes), Err(Error::Forbidden { .. })));
    // The lagging handle can commit (on the newest generation).
    let z = n("z");
    lagging.replace(&z, &[(z.clone(), Observed::Absent)], h(9), &yes).unwrap();
    assert_eq!(r.resolve(&z).unwrap().unwrap().hash, h(9));
}

#[test]
fn contention_across_rotations() {
    let d = tempfile::tempdir().unwrap();
    let c = n("counter");
    let wins: u64 = std::thread::scope(|sc| {
        (0..4u64)
            .map(|t| {
                let dir = d.path().to_path_buf();
                let c = c.clone();
                sc.spawn(move || {
                    let r = RefStore::open(&dir).unwrap();
                    r.set_compact_threshold(500);
                    let mut wins = 0;
                    while wins < 50 {
                        let obs = RefValue::observed(r.resolve(&c).unwrap());
                        if let ReplaceOutcome::Ok { .. } =
                            r.replace(&c, &[(c.clone(), obs)], h(t * 1000 + wins), &yes).unwrap()
                        {
                            wins += 1;
                        }
                    }
                    wins
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|j| j.join().unwrap())
            .sum()
    });
    let r = RefStore::open(d.path()).unwrap();
    assert_eq!(r.resolve(&c).unwrap().unwrap().version, wins);
    assert!(r.generation().unwrap() > 3);
}

#[test]
fn crash_during_compaction_at_every_point() {
    for budget in 0..40u64 {
        let d = tempfile::tempdir().unwrap();
        let mut committed: BTreeMap<Name, RefValue> = BTreeMap::new();
        {
            let r = RefStore::open_with_faults(d.path(), Faults::crash_after(budget)).unwrap();
            r.set_compact_threshold(200);
            for i in 0..12u64 {
                let nm = n(&format!("r/{}", i % 4));
                let obs = match committed.get(&nm) {
                    Some(v) => Observed::Version(v.version),
                    None => Observed::Absent,
                };
                match r.replace(&nm, &[(nm.clone(), obs)], h(i), &yes) {
                    Ok(ReplaceOutcome::Ok { version }) => {
                        committed.insert(nm, RefValue { hash: h(i), version });
                    }
                    Ok(c) => panic!("budget {budget}: unexpected {c:?}"),
                    Err(_) => break,
                }
            }
        }
        let r = RefStore::open(d.path()).unwrap_or_else(|e| panic!("budget {budget}: reopen failed: {e}"));
        let got: BTreeMap<Name, RefValue> = r.list("").unwrap().into_iter().collect();
        for (k, v) in &committed {
            let g = got.get(k).unwrap_or_else(|| panic!("budget {budget}: lost {k}"));
            // At most one unacknowledged later commit may have landed.
            assert!(g.version == v.version || g.version == v.version + 1, "budget {budget}: {k} {g:?} vs {v:?}");
        }
        let z = n("z");
        r.replace(&z, &[(z.clone(), Observed::Absent)], h(99), &yes).unwrap();
        assert_eq!(RefStore::open(d.path()).unwrap().resolve(&z).unwrap().unwrap().hash, h(99), "budget {budget}");
    }
}
