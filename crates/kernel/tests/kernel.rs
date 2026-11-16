//! The kernel against a direct model of the original semantics (the
//! pre-kernel `RefState::decide_*` and `o5s_core::{governing, mobility}`).

use o5s_kernel::names::{governing, is_under, mobility_of, validate};
use o5s_kernel::refs::{Decision, Effect, MAX_BATCH, Read, Refusal, Target, View, decide};
use o5s_kernel::{Mobility, Observed, RefVal, Rule};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn grammar() {
    for ok in ["a", "a/b", "A.b_c-d+e/0", "x/y/z"] {
        assert_eq!(validate(&b(ok)), Ok(()), "{ok}");
    }
    for bad in ["", "/a", "a/", "a//b", ".", "a/../b", "a/./b", "a b", "ä", "a\\b"] {
        assert!(validate(&b(bad)).is_err(), "{bad:?}");
    }
    assert!(validate(&vec![b'a'; 1024]).is_ok());
    assert!(validate(&vec![b'a'; 1025]).is_err());
    assert!(validate(&b("..a")).is_ok(), "'..a' is an ordinary segment");
}

#[test]
fn prefixes_are_segment_aligned() {
    assert!(is_under(&b("a/b"), &b("a")));
    assert!(is_under(&b("a"), &b("a")));
    assert!(!is_under(&b("ab"), &b("a")));
    assert!(is_under(&b("anything"), &b("")));
    let rules = vec![(b("m"), Rule::HeadRelease), (b("m/heads/x"), Rule::Immutable), (b("p"), Rule::Immutable)];
    assert_eq!(governing(&rules, &b("m/heads/x/y")).0, Rule::Immutable);
    assert_eq!(governing(&rules, &b("m/heads/y")).0, Rule::HeadRelease);
    assert_eq!(governing(&rules, &b("q")), (Rule::Mutable, None));
    assert_eq!(mobility_of(&rules, &b("m/heads/main")).1, Mobility::Mutable);
    assert_eq!(mobility_of(&rules, &b("m/releases/v1")).1, Mobility::CreateOnce);
    assert_eq!(mobility_of(&rules, &b("m/other")).1, Mobility::Forbidden);
    assert_eq!(mobility_of(&rules, &b("m")).1, Mobility::Forbidden);
    assert_eq!(mobility_of(&rules, &b("m/heads")).1, Mobility::Forbidden);
    assert_eq!(mobility_of(&rules, &b("m/heads/")).1, Mobility::Forbidden);
}

/// The original decision, written directly over maps (unsorted inputs).
#[allow(clippy::type_complexity)]
fn model(
    refs: &BTreeMap<Vec<u8>, RefVal>,
    tombs: &BTreeMap<Vec<u8>, u64>,
    rules: &[(Vec<u8>, Rule)],
    updates: &[(Vec<u8>, Option<[u8; 32]>)],
    read_set: &[(Vec<u8>, Observed)],
) -> Result<Result<Vec<Effect>, Vec<(Vec<u8>, Observed)>>, String> {
    let obs = |n: &Vec<u8>| refs.get(n).map_or(Observed::Absent, |v| Observed::Version(v.version));
    if updates.is_empty() || updates.len() > MAX_BATCH {
        return Err("size".into());
    }
    let mut seen = std::collections::HashSet::new();
    // Original order: per update, duplicate then membership. The kernel checks in
    // name order, so compare the first error by name: sort the updates first.
    let mut sorted: Vec<&(Vec<u8>, Option<[u8; 32]>)> = updates.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (n, _) in &sorted {
        if !seen.insert(n.clone()) {
            return Err(format!("dup {n:?}"));
        }
        if !read_set.iter().any(|(r, _)| r == n) {
            return Err(format!("unread {n:?}"));
        }
    }
    let mut stale: Vec<(Vec<u8>, Observed)> =
        read_set.iter().filter(|(n, o)| obs(n) != *o).map(|(n, _)| (n.clone(), obs(n))).collect();
    if !stale.is_empty() {
        stale.sort_by(|a, b| a.0.cmp(&b.0).then(format!("{:?}", a.1).cmp(&format!("{:?}", b.1))));
        stale.dedup();
        return Ok(Err(stale));
    }
    let mut out = Vec::new();
    for (n, new) in &sorted {
        let covering: Vec<(Vec<u8>, Rule)> = rules.iter().filter(|(p, _)| is_under(n, p)).cloned().collect();
        let (_, m) = mobility_of(&covering, n);
        let cur = refs.get(n);
        let e = match (new, cur) {
            (Some(h), _) => {
                match m {
                    Mobility::Mutable => {}
                    Mobility::CreateOnce if cur.is_none() => {}
                    _ => return Err(format!("forbidden {n:?}")),
                }
                let version = cur.map_or(tombs.get(n).map_or(1, |t| t + 1), |v| v.version + 1);
                Effect::Set(RefVal { hash: *h, version })
            }
            (None, None) => Effect::Noop,
            (None, Some(c)) => match m {
                Mobility::Mutable => Effect::Delete(c.version),
                _ => return Err(format!("forbidden {n:?}")),
            },
        };
        out.push(e);
    }
    Ok(Ok(out))
}

fn names() -> impl Strategy<Value = Vec<u8>> {
    prop::sample::select(vec!["a", "a/x", "m/heads/main", "m/releases/v1", "m/other", "p/v1", "p", "q/r"]).prop_map(b)
}

fn rule() -> impl Strategy<Value = Rule> {
    prop::sample::select(vec![Rule::Mutable, Rule::Immutable, Rule::HeadRelease])
}

fn observed() -> impl Strategy<Value = Observed> {
    prop_oneof![Just(Observed::Absent), (1u64..4).prop_map(Observed::Version)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]
    #[test]
    fn kernel_matches_model(
        refs in prop::collection::btree_map(names(), (any::<u8>(), 1u64..4), 0..6),
        tombs in prop::collection::btree_map(names(), 1u64..4, 0..3),
        rules in prop::collection::btree_map(prop::sample::select(vec!["m", "p", "a", "m/heads"]).prop_map(b), rule(), 0..3),
        updates in prop::collection::vec((names(), prop::option::of(any::<u8>())), 1..5),
        read_set in prop::collection::vec((names(), observed()), 0..6),
        use_current in any::<bool>(),
    ) {
        let refs: BTreeMap<Vec<u8>, RefVal> = refs.into_iter().map(|(n, (h, v))| (n, RefVal { hash: [h; 32], version: v })).collect();
        let tombs: BTreeMap<Vec<u8>, u64> = tombs.into_iter().filter(|(n, _)| !refs.contains_key(n)).collect();
        let rules: Vec<(Vec<u8>, Rule)> = rules.into_iter().collect();
        let updates: Vec<(Vec<u8>, Option<[u8; 32]>)> = updates.into_iter().map(|(n, h)| (n, h.map(|h| [h; 32]))).collect();
        // Often observe the current state, so commits are exercised too.
        let obs = |n: &Vec<u8>| refs.get(n).map_or(Observed::Absent, |v| Observed::Version(v.version));
        let mut read_set = read_set;
        if use_current {
            read_set = updates.iter().map(|(n, _)| (n.clone(), obs(n))).collect();
        }
        let want = model(&refs, &tombs, &rules, &updates, &read_set);
        // The view, as the shell builds it.
        let mut reads: Vec<Read> = read_set.iter().map(|(n, o)| Read { name: n.clone(), observed: *o, current: obs(n) }).collect();
        reads.sort_by(|a, b| a.name.cmp(&b.name));
        let mut sorted = updates.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let targets: Vec<Target> = sorted.iter().map(|(n, h)| Target {
            name: n.clone(),
            new: *h,
            current: refs.get(n).copied(),
            tombstone: tombs.get(n).copied(),
            rules: rules.iter().filter(|(p, _)| is_under(n, p)).cloned().collect(),
        }).collect();
        let got = decide(&View { reads, targets });
        match (&want, &got) {
            (Ok(Ok(es)), Ok(Decision::Commit(ks))) => prop_assert_eq!(es, ks),
            (Ok(Err(st)), Ok(Decision::Conflict(ks))) => prop_assert_eq!(st, ks),
            (Err(e), Err(r)) => {
                let kind = match r {
                    Refusal::BatchSize => "size".to_string(),
                    Refusal::Duplicate(i) => format!("dup {:?}", sorted[*i].0),
                    Refusal::NotInReadSet(i) => format!("unread {:?}", sorted[*i].0),
                    Refusal::Forbidden(i, _, _) => format!("forbidden {:?}", sorted[*i].0),
                    Refusal::Unsorted => "unsorted".to_string(),
                    Refusal::VersionExhausted(i) => format!("exhausted {:?}", sorted[*i].0),
                };
                prop_assert_eq!(e, &kind);
            }
            _ => prop_assert!(false, "model {:?} vs kernel {:?}", want, got),
        }
    }
}
