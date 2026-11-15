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

/// Runs `f` on its own thread and fails the test if it takes longer than
/// `secs`, so a deadlock (e.g. a leaked journal flock) fails fast instead of
/// hanging the test run.
fn deadline(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(()) => t.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The body panicked: re-raise its panic.
            if let Err(p) = t.join() {
                std::panic::resume_unwind(p);
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("test body did not finish within {secs} s (deadlock?)")
        }
    }
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
    deadline(120, || {
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
    });
}

#[test]
fn crash_at_every_fault_point() {
    deadline(120, || {
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
    });
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
    tomb: BTreeMap<String, u64>,
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
        let v = self.refs.get(name).map_or(self.tomb.get(name).map_or(1, |t| t + 1), |v| v.1 + 1);
        self.tomb.remove(name);
        self.refs.insert(name.to_string(), (new, v));
        Ok(true)
    }
    fn delete(&mut self, name: &str, rs: &[(String, Observed)]) -> std::result::Result<bool, &'static str> {
        if !rs.iter().any(|(n, _)| n == name) {
            return Err("target_not_in_read_set");
        }
        if rs.iter().any(|(n, o)| self.obs(n) != *o) {
            return Ok(false);
        }
        let Some(cur) = self.refs.get(name).copied() else { return Ok(true) };
        let (rule, p) = self.rule_for(name);
        let rest = if p.is_empty() { name.to_string() } else { name[p.len()..].trim_start_matches('/').to_string() };
        let ok = match rule {
            Rule::Mutable => true,
            Rule::ImmutableAfterPublish => false,
            Rule::HeadMutableReleaseImmutable => rest.starts_with("heads/") && rest.len() > 6,
        };
        if !ok {
            return Err("forbidden");
        }
        self.refs.remove(name);
        self.tomb.insert(name.to_string(), cur.1);
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
    Delete(usize, bool),
    /// Unique targets (all in the read set, observed truly or by guess) + new values.
    Many(Vec<(usize, bool)>, u8),
    Replace(usize, Vec<(usize, Option<u8>)>, u8, bool),
    Rule(usize, u8),
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..NAMES.len(), proptest::collection::vec((0..NAMES.len(), proptest::option::of(0u8..4)), 0..3), any::<u8>(), any::<bool>())
            .prop_map(|(t, rs, v, fresh)| Op::Replace(t, rs, v, fresh)),
        2 => (0..PREFIXES.len(), 0u8..3).prop_map(|(p, r)| Op::Rule(p, r)),
        2 => (0..NAMES.len(), any::<bool>()).prop_map(|(t, f)| Op::Delete(t, f)),
        2 => (proptest::collection::vec((0..NAMES.len(), any::<bool>()), 1..4), any::<u8>()).prop_map(|(t, v)| Op::Many(t, v)),
        1 => Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]
    #[test]
    fn matches_model(ops in proptest::collection::vec(op(), 1..60), use_log in any::<bool>()) {
        let d = tempfile::tempdir().unwrap();
        // Both engines: the local journal and the create-only log on a Backend.
        let open = |d: &std::path::Path| -> RefStore {
            if use_log {
                let b: std::sync::Arc<dyn pico2peta_core::backend::Backend> =
                    std::sync::Arc::new(pico2peta_core::backend::LocalDisk::new(d.join("b")).unwrap());
                let r = RefStore::open_backend(b, "refs/").unwrap();
                r.set_compact_threshold(4);
                r
            } else {
                let r = RefStore::open(d).unwrap();
                r.set_compact_threshold(300); // rotate every few records
                r
            }
        };
        let mut r = open(d.path());
        let mut m = Model::default();
        for o in ops {
            match o {
                Op::Delete(t, fresh) => {
                    let nm = NAMES[t];
                    let rs = vec![(nm.to_string(), if fresh { m.obs(nm) } else { Observed::Version(3) })];
                    let expect = m.delete(nm, &rs);
                    let rs2: Vec<(Name, Observed)> = rs.iter().map(|(a, o)| (n(a), *o)).collect();
                    match (expect, r.delete(&n(nm), &rs2)) {
                        (Ok(true), Ok(ReplaceOutcome::Ok { .. })) => {}
                        (Ok(false), Ok(ReplaceOutcome::Conflict { .. })) => {}
                        (Err(code), Err(e)) => prop_assert_eq!(code, e.code()),
                        (e, g) => prop_assert!(false, "delete: model {:?} vs impl {:?}", e, g),
                    }
                }
                Op::Many(mut targets, v) => {
                    targets.sort_by_key(|t| t.0);
                    targets.dedup_by_key(|t| t.0);
                    let rs: Vec<(String, Observed)> = targets.iter().map(|(t, fresh)| {
                        let nm = NAMES[*t];
                        (nm.to_string(), if *fresh { m.obs(nm) } else { Observed::Version(7) })
                    }).collect();
                    // Model: conflict if any stale; else each rule in order; else apply all.
                    let expect: std::result::Result<bool, &str> = if rs.iter().any(|(n2, o)| m.obs(n2) != *o) {
                        Ok(false)
                    } else {
                        let mut trial = Model { refs: m.refs.clone(), rules: m.rules.clone(), tomb: m.tomb.clone() };
                        let mut res = Ok(true);
                        for (i, (t, _)) in targets.iter().enumerate() {
                            // Each update is judged against the pre-state.
                            let mut probe = Model { refs: m.refs.clone(), rules: m.rules.clone(), tomb: m.tomb.clone() };
                            if let Err(e) = probe.replace(NAMES[*t], &rs, h(v as u64 + i as u64)) { res = Err(e); break; }
                            let ver = m.refs.get(NAMES[*t]).map_or(m.tomb.get(NAMES[*t]).map_or(1, |t| t + 1), |x| x.1 + 1);
                            trial.tomb.remove(NAMES[*t]);
                            trial.refs.insert(NAMES[*t].to_string(), (h(v as u64 + i as u64), ver));
                        }
                        if res == Ok(true) { m.refs = trial.refs; m.tomb = trial.tomb; }
                        res
                    };
                    let ups: Vec<(Name, Hash)> = targets.iter().enumerate().map(|(i, (t, _))| (n(NAMES[*t]), h(v as u64 + i as u64))).collect();
                    let rs2: Vec<(Name, Observed)> = rs.iter().map(|(a, o)| (n(a), *o)).collect();
                    match (expect, r.replace_many(&ups, &rs2, &yes)) {
                        (Ok(true), Ok(pico2peta_core::ReplaceManyOutcome::Ok { .. })) => {}
                        (Ok(false), Ok(pico2peta_core::ReplaceManyOutcome::Conflict { .. })) => {}
                        (Err(code), Err(e)) => prop_assert_eq!(code, e.code()),
                        (e, g) => prop_assert!(false, "many: model {:?} vs impl {:?}", e, g),
                    }
                }
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
                    r = open(d.path());
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
    deadline(120, || {
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
    });
}

#[test]
fn crash_during_compaction_at_every_point() {
    deadline(120, || {
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
    });
}

#[test]
fn replace_many_is_atomic() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap();
    let (a, b, p) = (n("a"), n("b"), n("pkg/x"));
    let out = r
        .replace_many(
            &[(a.clone(), h(1)), (b.clone(), h(2))],
            &[(a.clone(), Observed::Absent), (b.clone(), Observed::Absent)],
            &yes,
        )
        .unwrap();
    assert_eq!(out, pico2peta_core::ReplaceManyOutcome::Ok { versions: vec![1, 1] });
    // One stale entry: nothing changes.
    let out = r
        .replace_many(
            &[(a.clone(), h(3)), (b.clone(), h(4))],
            &[(a.clone(), Observed::Version(1)), (b.clone(), Observed::Absent)],
            &yes,
        )
        .unwrap();
    assert!(matches!(out, pico2peta_core::ReplaceManyOutcome::Conflict { .. }));
    assert_eq!(r.resolve(&a).unwrap().unwrap().hash, h(1));
    // A rule violation on one name: nothing changes.
    r.replace(&p, &[(p.clone(), Observed::Absent)], h(5), &yes).unwrap();
    let e = r.replace_many(
        &[(a.clone(), h(3)), (p.clone(), h(6))],
        &[(a.clone(), Observed::Version(1)), (p.clone(), Observed::Version(1))],
        &yes,
    );
    assert!(matches!(e, Err(Error::Forbidden { .. })));
    assert_eq!(r.resolve(&a).unwrap().unwrap().hash, h(1));
    // Input validation.
    assert!(matches!(
        r.replace_many(&[(a.clone(), h(3))], &[(b.clone(), Observed::Version(1))], &yes),
        Err(Error::TargetNotInReadSet(_))
    ));
    assert!(
        r.replace_many(&[(a.clone(), h(3)), (a.clone(), h(4))], &[(a.clone(), Observed::Version(1))], &yes).is_err()
    );
    // Persisted as one record; survives reopen and compaction.
    r.compact().unwrap();
    let r2 = RefStore::open(d.path()).unwrap();
    assert_eq!(r2.resolve(&b).unwrap().unwrap(), RefValue { hash: h(2), version: 1 });
}

#[test]
fn delete_rules_tombstones_and_persistence() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap();
    r.set_rule("m", Rule::HeadMutableReleaseImmutable).unwrap();
    let (a, p, head, rel) = (n("a"), n("pkg/x"), n("m/heads/main"), n("m/releases/v1"));
    for nm in [&a, &p, &head, &rel] {
        r.replace(nm, &[(nm.clone(), Observed::Absent)], h(1), &yes).unwrap();
    }
    r.replace(&a, &[(a.clone(), Observed::Version(1))], h(2), &yes).unwrap();
    // Stale delete conflicts; fresh delete succeeds.
    assert!(matches!(r.delete(&a, &[(a.clone(), Observed::Version(1))]).unwrap(), ReplaceOutcome::Conflict { .. }));
    assert_eq!(r.delete(&a, &[(a.clone(), Observed::Version(2))]).unwrap(), ReplaceOutcome::Ok { version: 2 });
    assert_eq!(r.resolve(&a).unwrap(), None);
    // Immutable names can't be deleted; heads can.
    assert!(matches!(r.delete(&p, &[(p.clone(), Observed::Version(1))]), Err(Error::Forbidden { .. })));
    assert!(matches!(r.delete(&rel, &[(rel.clone(), Observed::Version(1))]), Err(Error::Forbidden { .. })));
    r.delete(&head, &[(head.clone(), Observed::Version(1))]).unwrap();
    // Re-create continues the version sequence (no ABA on versions).
    r.compact().unwrap(); // tombstones survive compaction
    let r = RefStore::open(d.path()).unwrap();
    assert_eq!(r.replace(&a, &[(a.clone(), Observed::Absent)], h(3), &yes).unwrap(), ReplaceOutcome::Ok { version: 3 });
    // Deleting an absent name is an idempotent no-op.
    let z = n("zz");
    assert_eq!(r.delete(&z, &[(z.clone(), Observed::Absent)]).unwrap(), ReplaceOutcome::Ok { version: 0 });
}

#[test]
fn update_many_mixes_sets_and_deletes_atomically() {
    let d = tempfile::tempdir().unwrap();
    let r = RefStore::open(d.path()).unwrap();
    r.set_rule("tags", Rule::ImmutableAfterPublish).unwrap();
    let (a, b, t) = (n("heads/a"), n("heads/b"), n("tags/v1"));
    r.replace(&a, &[(a.clone(), Observed::Absent)], h(1), &yes).unwrap();
    r.replace(&t, &[(t.clone(), Observed::Absent)], h(1), &yes).unwrap();
    // Delete a, create b, in one transaction.
    let out = r
        .update_many(
            &[(a.clone(), None), (b.clone(), Some(h(2)))],
            &[(a.clone(), Observed::Version(1)), (b.clone(), Observed::Absent)],
            &yes,
        )
        .unwrap();
    assert_eq!(out, pico2peta_core::ReplaceManyOutcome::Ok { versions: vec![1, 1] });
    assert_eq!(r.resolve(&a).unwrap(), None);
    // A forbidden delete of an immutable tag aborts the whole transaction.
    let e = r.update_many(
        &[(b.clone(), None), (t.clone(), None)],
        &[(b.clone(), Observed::Version(1)), (t.clone(), Observed::Version(1))],
        &yes,
    );
    assert!(matches!(e, Err(Error::Forbidden { .. })));
    assert!(r.resolve(&b).unwrap().is_some(), "nothing applied");
    // Persisted (and through compaction).
    r.compact().unwrap();
    let r2 = RefStore::open(d.path()).unwrap();
    assert_eq!(r2.resolve(&b).unwrap().unwrap().hash, h(2));
    assert_eq!(
        r2.replace(&a, &[(a.clone(), Observed::Absent)], h(3), &yes).unwrap(),
        ReplaceOutcome::Ok { version: 2 }
    );
}

/// Review finding 1: a multi-megabyte (valid) batch must be readable by every
/// other handle, and never be mistaken for a torn tail and truncated.
#[test]
fn large_batch_is_durable_and_visible_to_other_handles() {
    let d = tempfile::tempdir().unwrap();
    let a = RefStore::open(d.path()).unwrap();
    let b = RefStore::open(d.path()).unwrap();
    let long = "x".repeat(900);
    let names: Vec<Name> = (0..6000).map(|i| n(&format!("big/{i}/{long}"))).collect();
    let ups: Vec<(Name, Hash)> = names.iter().map(|nm| (nm.clone(), h(1))).collect();
    let rs: Vec<(Name, Observed)> = names.iter().map(|nm| (nm.clone(), Observed::Absent)).collect();
    assert!(matches!(a.replace_many(&ups, &rs, &yes).unwrap(), pico2peta_core::ReplaceManyOutcome::Ok { .. }));
    // Another handle sees it, and its next commit must not truncate it.
    assert_eq!(b.list("big").unwrap().len(), 6000);
    let z = n("z");
    b.replace(&z, &[(z.clone(), Observed::Absent)], h(2), &yes).unwrap();
    let c = RefStore::open(d.path()).unwrap();
    assert_eq!(c.list("big").unwrap().len(), 6000);
    assert!(a.resolve(&z).unwrap().is_some(), "the original writer keeps working");
}

/// Review finding 4: a corrupted record in the middle of the journal is an
/// error, never silently truncated together with all later records.
#[test]
fn mid_journal_corruption_is_an_error_not_a_truncation() {
    let d = tempfile::tempdir().unwrap();
    {
        let r = RefStore::open(d.path()).unwrap();
        for i in 0..5u64 {
            let nm = n(&format!("k{i}"));
            r.replace(&nm, &[(nm.clone(), Observed::Absent)], h(i), &yes).unwrap();
        }
    }
    let p = d.path().join("journal");
    let mut j = std::fs::read(&p).unwrap();
    j[8 + 12] ^= 0xff; // inside the first record's payload
    std::fs::write(&p, &j).unwrap();
    assert!(matches!(RefStore::open(d.path()), Err(Error::Corrupt(_))));
    assert_eq!(std::fs::read(&p).unwrap().len(), j.len(), "nothing was truncated");
    // A zero-filled tail (crash after size update) is still a torn tail.
    let mut j2 = j.clone();
    j2[8 + 12] ^= 0xff; // restore
    j2.extend_from_slice(&[0u8; 37]);
    std::fs::write(&p, &j2).unwrap();
    let r = RefStore::open(d.path()).unwrap();
    assert_eq!(r.list("").unwrap().len(), 5);
}

/// Group commit: many threads on ONE handle (so commits batch into shared
/// fsyncs). Every acknowledged commit is durable (visible after reopen), the
/// shared OCC counter loses no update, and per-thread names end at their last value.
#[test]
fn group_commit_one_handle_many_threads() {
    deadline(120, || {
        let d = tempfile::tempdir().unwrap();
        let r = RefStore::open(d.path()).unwrap();
        let c = n("counter");
        let (threads, per) = (8u64, 60u64);
        std::thread::scope(|sc| {
            for t in 0..threads {
                let (r, c) = (&r, c.clone());
                sc.spawn(move || {
                    let mine = n(&format!("t/{t}"));
                    for i in 0..per {
                        // A distinct name: never conflicts.
                        let obs = RefValue::observed(r.resolve(&mine).unwrap());
                        assert!(matches!(
                            r.replace(&mine, &[(mine.clone(), obs)], h(t * 1000 + i), &yes).unwrap(),
                            ReplaceOutcome::Ok { .. }
                        ));
                        // The shared counter: retried until it wins.
                        loop {
                            let obs = RefValue::observed(r.resolve(&c).unwrap());
                            if let ReplaceOutcome::Ok { .. } = r.replace(&c, &[(c.clone(), obs)], h(t), &yes).unwrap() {
                                break;
                            }
                        }
                    }
                });
            }
        });
        drop(r);
        let r = RefStore::open(d.path()).unwrap();
        assert_eq!(r.resolve(&c).unwrap().unwrap().version, threads * per, "no lost update");
        for t in 0..threads {
            let v = r.resolve(&n(&format!("t/{t}"))).unwrap().unwrap();
            assert_eq!((v.hash, v.version), (h(t * 1000 + per - 1), per));
        }
    });
}

/// Group commit with an fsync failure in the middle: commits that returned Ok
/// are durable; commits of the failed batch got an error; the store keeps working.
#[test]
fn group_commit_fsync_failure_fails_the_batch_only() {
    deadline(120, || {
        let d = tempfile::tempdir().unwrap();
        // Hit 25 is an append or an fsync, depending on batching; either way an error surfaces.
        let f = Faults::fail_once_at(25);
        let r = RefStore::open_with_faults(d.path(), f).unwrap();
        let acked: std::sync::Mutex<Vec<(Name, Hash)>> = Default::default();
        let failed = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for t in 0..6u64 {
                let (r, acked, failed) = (&r, &acked, &failed);
                sc.spawn(move || {
                    for i in 0..20u64 {
                        let name = n(&format!("x/{t}/{i}"));
                        match r.replace(&name, &[(name.clone(), Observed::Absent)], h(t * 100 + i), &yes) {
                            Ok(ReplaceOutcome::Ok { .. }) => acked.lock().unwrap().push((name, h(t * 100 + i))),
                            Ok(other) => panic!("{other:?}"),
                            Err(_) => {
                                failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });
        assert!(failed.load(std::sync::atomic::Ordering::Relaxed) >= 1, "the injected failure surfaced");
        drop(r);
        let r = RefStore::open(d.path()).unwrap();
        for (name, hash) in acked.into_inner().unwrap() {
            assert_eq!(r.resolve(&name).unwrap().map(|v| v.hash), Some(hash), "acknowledged commit lost: {name}");
        }
        let z = n("after");
        assert!(matches!(
            r.replace(&z, &[(z.clone(), Observed::Absent)], h(1), &yes).unwrap(),
            ReplaceOutcome::Ok { .. }
        ));
    });
}
