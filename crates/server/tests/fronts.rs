//! The server's fronts on the core Front API: claims are enforced, admins act through the
//! owning front, and server GC follows every front's objects.

mod common;

use common::*;
use o5s_core::{Name, ObjectStore, Observed, ReplaceOutcome, Repository};
use o5s_server::config::Access;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

fn absent(s: &str) -> Vec<(Name, Observed)> {
    vec![(n(s), Observed::Absent)]
}

#[test]
fn front_claims_are_enforced() {
    let srv = TestServer::start(|cfg| cfg.policy.push(grant("alice", "git/alice", Access::Write)));
    // In-process: the plain repository refuses names owned by a front.
    let st = &srv.state;
    let h = st.repo.put(b"x").unwrap();
    let e = st.repo.replace(&n("git/alice/r/refs/heads/main"), &absent("git/alice/r/refs/heads/main"), h).unwrap_err();
    assert!(matches!(e, o5s_core::Error::Unauthorized(_)), "{e}");
    assert!(st.repo.replace(&n("oci/alice/app/_tags/v1"), &absent("oci/alice/app/_tags/v1"), h).is_err());
    assert!(matches!(st.repo.replace(&n("alice/free"), &absent("alice/free"), h).unwrap(), ReplaceOutcome::Ok { .. }));
    // Over HTTP: a namespace writer still can't move adapter-managed names via /v1 ...
    let alice = srv.client(ALICE);
    let h = alice.put(b"y").unwrap();
    assert!(alice.replace(&n("git/alice/r/refs/heads/main"), &absent("git/alice/r/refs/heads/main"), h).is_err());
    // ... while a global admin can (through the git front's handle), and so can set rules.
    let admin = srv.client(ADMIN);
    let r = admin.replace(&n("git/alice/r/refs/heads/main"), &absent("git/alice/r/refs/heads/main"), h);
    assert!(matches!(r.unwrap(), ReplaceOutcome::Ok { .. }));
    admin.set_rule("git/alice/r/refs/tags", o5s_core::Rule::ImmutableAfterPublish).unwrap();
    // A multi-update spanning two fronts can't pick one owner: refused, nothing moved.
    let r = admin.replace_many(
        &[(n("git/alice/r/refs/heads/x"), h), (n("oci/alice/app/_tags/v1"), h)],
        &[(n("git/alice/r/refs/heads/x"), Observed::Absent), (n("oci/alice/app/_tags/v1"), Observed::Absent)],
    );
    assert!(r.is_err());
    assert!(admin.resolve(&n("git/alice/r/refs/heads/x")).unwrap().is_none());
}
