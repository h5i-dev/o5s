//! Front API: profiles, nested prefix claims, front handles, the union GC
//! tracer, and config validation.
use pico2peta_core::{Encoding, Error, Hash, Name, ObjectStore, Observed, ReplaceOutcome, Repository, Rule};
use pico2peta_repo::gc::GcOptions;
use pico2peta_repo::{
    ChunkingSection, DerivePolicy, FORMAT_VERSION, Front, FrontRegistry, LocalRepo, PackConfig, PlanCtx, Profile,
    RefsSection, RepoConfig,
};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A toy front: owns `prefix`, says each object's children are the hashes
/// listed (one per 32 bytes) after a `LINKS:` marker, and records plans.
struct Toy {
    id: &'static str,
    prefix: &'static str,
    plans: Mutex<Vec<(Option<String>, Option<Hash>)>>,
}

impl Toy {
    fn new(id: &'static str, prefix: &'static str) -> Arc<Toy> {
        Arc::new(Toy { id, prefix, plans: Mutex::new(Vec::new()) })
    }
}

impl Front for Toy {
    fn id(&self) -> &str {
        self.id
    }
    fn prefixes(&self) -> Vec<String> {
        vec![self.prefix.to_string()]
    }
    fn plan(&self, ctx: &PlanCtx<'_>, src: &mut dyn Read) -> pico2peta_core::Result<(Vec<u8>, Encoding)> {
        // Reads 4 bytes (to prove consumed bytes are put back) and asks for grouping.
        let mut head = vec![0u8; 4];
        let n = src.read(&mut head)?;
        head.truncate(n);
        self.plans.lock().unwrap().push((ctx.name.map(|n| n.to_string()), ctx.previous));
        Ok((head, Encoding { group_width: Some(2), ..ctx.default.clone() }))
    }
    fn children(&self, h: &Hash, store: &dyn ObjectStore) -> pico2peta_core::Result<Vec<Hash>> {
        let b = store.get(h, None)?;
        Ok(match b.strip_prefix(format!("{}LINKS:", self.id).as_bytes()) {
            Some(rest) => rest.as_chunks::<32>().0.iter().map(|c| Hash(*c)).collect(),
            None => Vec::new(),
        })
    }
}

fn cfg(profiles: &[(&str, Profile)], namespaces: &[(&str, &str)]) -> RepoConfig {
    RepoConfig {
        format: FORMAT_VERSION,
        pack: (&PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() }).into(),
        refs: RefsSection::default(),
        chunking: ChunkingSection::default(),
        profiles: profiles.iter().map(|(n, p)| (n.to_string(), p.clone())).collect::<BTreeMap<_, _>>(),
        namespaces: namespaces.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        read: Default::default(),
        gc: Default::default(),
    }
}

fn set(r: &dyn Repository, name: &str, h: Hash) -> pico2peta_core::Result<ReplaceOutcome> {
    let n = Name::new(name).unwrap();
    let obs = pico2peta_core::RefValue::observed(r.resolve(&n)?);
    r.replace(&n, &[(n.clone(), obs)], h)
}

fn bf16ish(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| if i % 2 == 0 { (i as u8).wrapping_mul(31).wrapping_add(seed) } else { 0x3c }).collect()
}

#[test]
fn profiles_drive_put_named_and_derive_from_previous() {
    let d = tempfile::tempdir().unwrap();
    let toy = Toy::new("toy", "toy/");
    let profile =
        Profile { front: Some("toy".into()), chunker: None, derive: DerivePolicy::Previous, ..Default::default() };
    let r = LocalRepo::init_config(d.path(), &cfg(&[("t", profile)], &[("data/", "t")]))
        .unwrap()
        .with_fronts(FrontRegistry::new().with(toy.clone()).unwrap())
        .unwrap();
    let n = Name::new("data/model").unwrap();
    let v1 = bf16ish(3_000_000, 1);
    let mut v2 = v1.clone();
    for i in (0..v2.len()).step_by(4096) {
        v2[i] ^= 1;
    }
    let h1 = r.put_named(&n, &mut &v1[..]).unwrap();
    assert_eq!(h1, Hash::of(&v1));
    assert!(matches!(set(&r, "data/model", h1).unwrap(), ReplaceOutcome::Ok { .. }));
    let deltas_before = r.pack_stats().unwrap().deltas;
    let h2 = r.put_named(&n, &mut &v2[..]).unwrap();
    assert_eq!(h2, Hash::of(&v2), "bytes the front consumed were put back");
    assert_eq!(r.get(&h2, None).unwrap(), v2);
    assert_eq!(r.pack_stats().unwrap().deltas, deltas_before + 1, "v2 stored as base + patch");
    let plans = toy.plans.lock().unwrap().clone();
    assert_eq!(plans, vec![(Some("data/model".into()), None), (Some("data/model".into()), Some(h1))]);
    // A name outside every namespace uses the plain profile: no front call, no delta.
    let other = r.put_named(&Name::new("misc/x").unwrap(), &mut &v2[..]).unwrap();
    assert_eq!(other, h2);
    assert_eq!(toy.plans.lock().unwrap().len(), 2);
}

#[test]
fn nested_prefix_claims_and_front_handles() {
    let d = tempfile::tempdir().unwrap();
    let reg = FrontRegistry::new().with(Toy::new("git", "git/")).unwrap().with(Toy::new("hf", "git/hf/")).unwrap();
    // Same prefix twice, or the same id twice, is refused.
    assert!(reg.clone().with(Toy::new("other", "git/")).is_err());
    assert!(reg.clone().with(Toy::new("git", "x/")).is_err());
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap().with_fronts(reg).unwrap();
    let h = r.put(b"x").unwrap();
    // Plain handle: unowned names only.
    assert!(matches!(set(&r, "free/a", h).unwrap(), ReplaceOutcome::Ok { .. }));
    assert!(matches!(set(&r, "git/refs/main", h), Err(Error::Unauthorized(_))));
    assert!(matches!(set(&r, "git/hf/models/x", h), Err(Error::Unauthorized(_))));
    // Longest claim wins.
    let git = r.front("git").unwrap();
    let hf = r.front("hf").unwrap();
    assert!(matches!(set(&git, "git/refs/main", h).unwrap(), ReplaceOutcome::Ok { .. }));
    assert!(matches!(set(&git, "git/hf/models/x", h), Err(Error::Unauthorized(_))));
    assert!(matches!(set(&hf, "git/hf/models/x", h).unwrap(), ReplaceOutcome::Ok { .. }));
    assert!(matches!(set(&hf, "git/refs/main", h), Err(Error::Unauthorized(_))));
    assert!(matches!(set(&hf, "free/b", h).unwrap(), ReplaceOutcome::Ok { .. }));
    // Every write path is guarded, not just replace.
    let n = Name::new("git/refs/main").unwrap();
    assert!(matches!(r.delete_ref(&n, &[]), Err(Error::Unauthorized(_))));
    assert!(matches!(r.replace_many(&[(n.clone(), h)], &[]), Err(Error::Unauthorized(_))));
    assert!(matches!(r.update_many(&[(n.clone(), None)], &[]), Err(Error::Unauthorized(_))));
    assert!(matches!(r.set_rule("git", Rule::ImmutableAfterPublish), Err(Error::Unauthorized(_))));
    assert!(
        matches!(r.set_rule("", Rule::ImmutableAfterPublish), Err(Error::Unauthorized(_))),
        "a rule covering claims"
    );
    let r2 = git.set_rule("git/tags", Rule::ImmutableAfterPublish);
    assert!(r2.is_ok(), "{r2:?}");
    // Reads are open to everyone.
    assert_eq!(r.resolve(&n).unwrap().unwrap().hash, h);
    assert!(r.front("nope").is_err());
    let _ = Observed::Absent;
}

#[test]
fn gc_keeps_what_any_front_references() {
    let d = tempfile::tempdir().unwrap();
    let reg = FrontRegistry::new().with(Toy::new("a", "a/")).unwrap().with(Toy::new("b", "b/")).unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap().with_fronts(reg).unwrap();
    let leaf_a = r.put(b"leaf known only to front a").unwrap();
    let leaf_b = r.put(b"leaf known only to front b").unwrap();
    let garbage = r.put(b"unreferenced").unwrap();
    // One root links both leaves, in each front's own syntax: front a's
    // links point at leaf_a, and an object of front b's links at leaf_b.
    let mut ob = b"bLINKS:".to_vec();
    ob.extend_from_slice(leaf_b.as_bytes());
    let hb = r.put(&ob).unwrap();
    let mut oa = b"aLINKS:".to_vec();
    oa.extend_from_slice(leaf_a.as_bytes());
    oa.extend_from_slice(hb.as_bytes());
    let root = r.put(&oa).unwrap();
    assert!(matches!(set(&r.front("a").unwrap(), "a/root", root).unwrap(), ReplaceOutcome::Ok { .. }));
    r.flush().unwrap();
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, repack_all: true, ..GcOptions::default() };
    for _ in 0..3 {
        r.gc_with_fronts(&opts, None, false).unwrap();
    }
    for h in [root, hb, leaf_a, leaf_b] {
        assert!(r.contains(&h).unwrap(), "{h} must survive");
    }
    assert!(!LocalRepo::open(d.path()).unwrap().contains(&garbage).unwrap(), "garbage collected");
}

#[test]
fn config_validation() {
    let d = tempfile::tempdir().unwrap();
    // A namespace naming a missing profile.
    assert!(matches!(LocalRepo::init_config(d.path().join("a"), &cfg(&[], &[("m/", "nope")])), Err(Error::Parse(_))));
    // A profile with an unknown chunker.
    let bad = Profile { chunker: Some("rabin".into()), ..Default::default() };
    assert!(matches!(LocalRepo::init_config(d.path().join("b"), &cfg(&[("p", bad)], &[])), Err(Error::Parse(_))));
    // A profile naming a front that isn't registered: refused when opened with fronts.
    let p = Profile { front: Some("ghost".into()), ..Default::default() };
    let r = LocalRepo::init_config(d.path().join("c"), &cfg(&[("p", p)], &[("m/", "p")])).unwrap();
    assert!(matches!(r.with_fronts(FrontRegistry::new()), Err(Error::Parse(_))));
    // Profiles round-trip through config.toml.
    let p = Profile { front: None, chunker: Some("xet".into()), derive: DerivePolicy::Previous, ..Default::default() };
    let c = cfg(&[("w", p)], &[("models/", "w")]);
    LocalRepo::init_config(d.path().join("e"), &c).unwrap();
    assert_eq!(LocalRepo::read_config(&d.path().join("e")).unwrap(), c);
}

/// `Front::gc_tracer` is built once per GC run and replaces `children`; and
/// a front handle is 'static (fits in `Arc<dyn Repository>`).
#[test]
fn gc_snapshot_tracer_and_owned_handles() {
    use pico2peta_repo::gc::Tracer;
    struct Snap {
        root: Hash,
        leaf: Hash,
        built: Mutex<u32>,
    }
    struct Fixed(Hash, Hash);
    impl Tracer for Fixed {
        fn children(&self, h: &Hash, _: &dyn ObjectStore) -> pico2peta_core::Result<Vec<Hash>> {
            Ok(if *h == self.0 { vec![self.1] } else { Vec::new() })
        }
    }
    impl Front for Snap {
        fn id(&self) -> &str {
            "snap"
        }
        fn prefixes(&self) -> Vec<String> {
            vec!["snap/".into()]
        }
        fn gc_tracer(&self, _: &dyn Repository) -> pico2peta_core::Result<Option<Box<dyn Tracer>>> {
            *self.built.lock().unwrap() += 1;
            Ok(Some(Box::new(Fixed(self.root, self.leaf))))
        }
        fn children(&self, _: &Hash, _: &dyn ObjectStore) -> pico2peta_core::Result<Vec<Hash>> {
            Ok(Vec::new()) // only the snapshot tracer knows the link
        }
    }
    let d = tempfile::tempdir().unwrap();
    let r0 = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let root = r0.put(b"root").unwrap();
    let leaf = r0.put(b"leaf reachable only via the snapshot").unwrap();
    let snap = Arc::new(Snap { root, leaf, built: Mutex::new(0) });
    let r = r0.with_fronts(FrontRegistry::new().with(snap.clone()).unwrap()).unwrap();
    let handle: Arc<dyn Repository> = Arc::new(r.front("snap").unwrap());
    assert!(matches!(set(handle.as_ref(), "snap/root", root).unwrap(), ReplaceOutcome::Ok { .. }));
    r.flush().unwrap();
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, repack_all: true, ..GcOptions::default() };
    for _ in 0..3 {
        r.gc_with_fronts(&opts, None, false).unwrap();
    }
    assert!(r.contains(&leaf).unwrap());
    assert!(*snap.built.lock().unwrap() >= 3, "built once per GC run");
}

/// `[read]` policy: eof reads are refused unless the repository allows them;
/// `verify = "eof"` makes eof the default (and allowed); the section
/// round-trips through config.toml; strict is always available.
#[test]
fn read_verify_policy() {
    use pico2peta_core::ReadVerify;
    use pico2peta_repo::ReadSection;
    let data = vec![5u8; 3 << 20];
    let mk = |dir: &std::path::Path, read: ReadSection| {
        let mut c = RepoConfig::new(&PackConfig::default());
        c.read = read.clone();
        let r = LocalRepo::init_config(dir, &c).unwrap();
        assert_eq!(LocalRepo::read_config(dir).unwrap().read, read);
        r
    };
    let d = tempfile::tempdir().unwrap();
    let strict = mk(&d.path().join("s"), ReadSection::default());
    let h = strict.put(&data).unwrap();
    assert!(matches!(strict.open_verified(&h, None, ReadVerify::Eof), Err(Error::Unsupported(_))));
    assert!(strict.open_verified(&h, None, ReadVerify::Strict).is_ok());
    let allowed = mk(&d.path().join("a"), ReadSection { verify: ReadVerify::Strict, allow_eof: true });
    let h = allowed.put(&data).unwrap();
    let mut v = Vec::new();
    allowed.open_verified(&h, None, ReadVerify::Eof).unwrap().read_to_end(&mut v).unwrap();
    assert_eq!(v, data);
    let eof_default = mk(&d.path().join("e"), ReadSection { verify: ReadVerify::Eof, allow_eof: false });
    assert!(eof_default.read_policy().eof_allowed());
    let h = eof_default.put(&data).unwrap();
    assert_eq!(eof_default.get(&h, None).unwrap(), data);
    // A bad value in config.toml is an error, not a silent default.
    let bad = d.path().join("b");
    std::fs::create_dir_all(&bad).unwrap();
    let text = toml_like(&d.path().join("s")).replace("format = 1", "format = 1\n[read]\nverify = \"fast\"");
    std::fs::write(bad.join("config.toml"), text).unwrap();
    assert!(LocalRepo::read_config(&bad).is_err());
}

fn toml_like(repo: &std::path::Path) -> String {
    std::fs::read_to_string(repo.join("config.toml")).unwrap()
}
