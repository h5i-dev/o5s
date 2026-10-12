use super::*;

const REPO: u64 = REPOSITORY_SCOPE + 7;

fn who(org: u64, user: u64) -> Principal {
    Principal {
        org,
        user,
        scope: REPO,
    }
}

fn name(s: &str) -> Text {
    s.as_bytes().to_vec()
}

fn hash(n: u8) -> Text {
    let mut h = vec![b'0'; HASH_LEN];
    h[HASH_LEN - 1] = b'a' + n % 6;
    h[HASH_LEN - 2] = b'0' + n / 6;
    h
}

fn run(s: &Snapshot, a: Principal, c: Command) -> (Snapshot, Result<Reply, Error>) {
    match transition(&a, s, &c) {
        Ok((ws, r)) => (apply(s, &ws), Ok(r)),
        Err(e) => (s.clone(), Err(e)),
    }
}

fn ok(s: &Snapshot, a: Principal, c: Command) -> (Snapshot, Reply) {
    let (s, r) = run(s, a, c);
    (s, r.unwrap())
}

fn created() -> Snapshot {
    ok(
        &Snapshot::default(),
        who(1, 1),
        Command::Create {
            name: name("alpha"),
        },
    )
    .0
}

fn propose(s: &Snapshot, user: u64, n: u8, deps: Vec<u64>) -> (Snapshot, u64) {
    let (s, r) = ok(
        s,
        who(1, user),
        Command::Propose {
            repo: name("alpha"),
            hash: hash(n),
            dependencies: deps,
            paths: vec![PathUse {
                path: name("a.txt"),
                graph: false,
            }],
            body: vec![n],
        },
    );
    match r {
        Reply::Proposed {
            patch,
            duplicate: false,
        } => (s, patch),
        other => panic!("{other:?}"),
    }
}

fn ready(s: &Snapshot, id: u64) -> Snapshot {
    let s = ok(
        s,
        who(1, 2),
        Command::Approve {
            repo: name("alpha"),
            patch: id,
        },
    )
    .0;
    ok(
        &s,
        who(1, 3),
        Command::Check {
            repo: name("alpha"),
            patch: id,
            passed: true,
        },
    )
    .0
}

fn merge(s: &Snapshot, channel: &str, ids: Vec<u64>) -> (Snapshot, Result<Reply, Error>) {
    run(
        s,
        who(1, 4),
        Command::Merge {
            repo: name("alpha"),
            channel: name(channel),
            patches: ids,
        },
    )
}

#[test]
fn names_and_hashes() {
    assert!(valid_name(&name("a-b_C9")));
    assert!(!valid_name(&name("")));
    assert!(!valid_name(&name("a/b")));
    assert!(!valid_name(&vec![b'a'; 65]));
    assert!(valid_hash(&hash(3)));
    assert!(!valid_hash(&name("ABC")));
    assert!(!valid_hash(&vec![b'A'; HASH_LEN]));
}

#[test]
fn create_is_once_and_scoped() {
    let s = created();
    assert!(s.repository.created);
    assert_eq!(members_of(&s.members, 1), Vec::<u64>::new());
    assert_eq!(find_channel(&s.channels, &name("main")).unwrap().id, 1);
    assert_eq!(
        run(
            &s,
            who(1, 9),
            Command::Create {
                name: name("alpha")
            }
        )
        .1,
        Err(Error::RepositoryExists)
    );
    // Another organization, or another name, under the same key is a collision.
    assert_eq!(
        run(
            &s,
            who(2, 9),
            Command::Create {
                name: name("alpha")
            }
        )
        .1,
        Err(Error::KeyCollision)
    );
    assert_eq!(
        run(&s, who(1, 9), Command::Create { name: name("beta") }).1,
        Err(Error::KeyCollision)
    );
    let org = Principal {
        org: 1,
        user: 1,
        scope: 1,
    };
    assert_eq!(
        run(
            &Snapshot::default(),
            org,
            Command::Create { name: name("x") }
        )
        .1,
        Err(Error::WrongScope)
    );
    // Other organizations cannot see the repository.
    assert_eq!(
        run(
            &s,
            who(2, 1),
            Command::Approve {
                repo: name("alpha"),
                patch: 1
            }
        )
        .1,
        Err(Error::RepositoryNotFound)
    );
}

#[test]
fn directory_is_idempotent_and_detects_collisions() {
    let org = Principal {
        org: 5,
        user: 1,
        scope: 5,
    };
    let (s, r) = ok(
        &Snapshot::default(),
        org,
        Command::Register {
            name: name("alpha"),
            repo: REPO,
        },
    );
    assert_eq!(r, Reply::Registered { repo: REPO });
    let (s2, r) = ok(
        &s,
        org,
        Command::Register {
            name: name("alpha"),
            repo: REPO,
        },
    );
    assert_eq!(
        (r, s2.listings.len()),
        (Reply::Registered { repo: REPO }, 1)
    );
    assert_eq!(
        run(
            &s,
            org,
            Command::Register {
                name: name("alpha"),
                repo: REPO + 1
            }
        )
        .1,
        Err(Error::KeyCollision)
    );
    assert_eq!(
        run(
            &s,
            org,
            Command::Register {
                name: name("beta"),
                repo: REPO
            }
        )
        .1,
        Err(Error::KeyCollision)
    );
    let other = Principal {
        org: 5,
        user: 1,
        scope: 6,
    };
    assert_eq!(
        run(
            &s,
            other,
            Command::Register {
                name: name("c"),
                repo: REPO + 2
            }
        )
        .1,
        Err(Error::WrongScope)
    );
}

#[test]
fn patches_are_immutable_and_deduplicated() {
    let s = created();
    let (s, a) = propose(&s, 1, 1, vec![]);
    let (s, b) = propose(&s, 1, 2, vec![a]);
    assert_eq!((a, b), (1, 2));
    let (s2, r) = ok(
        &s,
        who(1, 1),
        Command::Propose {
            repo: name("alpha"),
            hash: hash(1),
            dependencies: vec![],
            paths: vec![],
            body: vec![],
        },
    );
    assert_eq!(
        r,
        Reply::Proposed {
            patch: 1,
            duplicate: true
        }
    );
    assert_eq!(s2, s);
    assert_eq!(dependencies_of(&s.dependencies, b), vec![a]);
    assert_eq!(
        run(
            &s,
            who(1, 1),
            Command::Propose {
                repo: name("alpha"),
                hash: hash(9),
                dependencies: vec![9],
                paths: vec![],
                body: vec![]
            }
        )
        .1,
        Err(Error::UnknownDependency)
    );
    assert_eq!(s.repository.events, 3);
    assert_eq!(s.bodies.len(), 2);
}

#[test]
fn path_models_do_not_mix() {
    let s = created();
    let (s, _) = propose(&s, 1, 1, vec![]);
    let graph = Command::Propose {
        repo: name("alpha"),
        hash: hash(5),
        dependencies: vec![],
        paths: vec![PathUse {
            path: name("a.txt"),
            graph: true,
        }],
        body: vec![],
    };
    assert_eq!(run(&s, who(1, 1), graph).1, Err(Error::LegacyDocument));
    let mixed = Command::Propose {
        repo: name("alpha"),
        hash: hash(6),
        dependencies: vec![],
        paths: vec![
            PathUse {
                path: name("g"),
                graph: true,
            },
            PathUse {
                path: name("g"),
                graph: false,
            },
        ],
        body: vec![],
    };
    assert_eq!(run(&s, who(1, 1), mixed).1, Err(Error::GraphDocument));
}

#[test]
fn review_requires_independence() {
    let s = created();
    let (s, a) = propose(&s, 1, 1, vec![]);
    assert_eq!(
        run(
            &s,
            who(1, 1),
            Command::Approve {
                repo: name("alpha"),
                patch: a
            }
        )
        .1,
        Err(Error::IndependentReviewerRequired)
    );
    assert_eq!(
        run(
            &s,
            who(1, 1),
            Command::Check {
                repo: name("alpha"),
                patch: a,
                passed: true
            }
        )
        .1,
        Err(Error::IndependentCheckReporterRequired)
    );
    assert_eq!(
        run(
            &s,
            who(1, 2),
            Command::Approve {
                repo: name("alpha"),
                patch: 9
            }
        )
        .1,
        Err(Error::PatchNotFound)
    );
}

#[test]
fn merge_is_gated_atomic_and_dependency_closed() {
    let s = created();
    let (s, a) = propose(&s, 1, 1, vec![]);
    let (s, b) = propose(&s, 1, 2, vec![a]);
    assert_eq!(merge(&s, "main", vec![a]).1, Err(Error::Unapproved));
    let s = ok(
        &s,
        who(1, 2),
        Command::Approve {
            repo: name("alpha"),
            patch: a,
        },
    )
    .0;
    assert_eq!(merge(&s, "main", vec![a]).1, Err(Error::ChecksFailed));
    let s = ok(
        &s,
        who(1, 3),
        Command::Check {
            repo: name("alpha"),
            patch: a,
            passed: true,
        },
    )
    .0;
    let s = ready(&s, b);
    // b alone lacks its dependency; a failing batch writes nothing.
    assert_eq!(merge(&s, "main", vec![b]).1, Err(Error::MissingDependency));
    assert_eq!(merge(&s, "main", vec![b, a]).1, Err(Error::InvalidBatch));
    assert_eq!(merge(&s, "nope", vec![a]).1, Err(Error::ChannelNotFound));
    let (s, r) = merge(&s, "main", vec![a, b]);
    assert_eq!(
        r,
        Ok(Reply::Merged {
            channel: 1,
            admitted: 2
        })
    );
    assert_eq!(members_of(&s.members, 1), vec![a, b]);
    // Duplicate delivery is idempotent.
    let (s, r) = merge(&s, "main", vec![a]);
    assert_eq!(
        r,
        Ok(Reply::Merged {
            channel: 1,
            admitted: 0
        })
    );
    assert_eq!(members_of(&s.members, 1), vec![a, b]);
    // A failed check report blocks later admission.
    let s = ok(
        &s,
        who(1, 3),
        Command::Check {
            repo: name("alpha"),
            patch: a,
            passed: false,
        },
    )
    .0;
    let (fork, r) = ok(
        &s,
        who(1, 4),
        Command::Fork {
            repo: name("alpha"),
            source: name("main"),
            channel: name("review"),
        },
    );
    assert_eq!(r, Reply::Forked { channel: 2 });
    assert_eq!(members_of(&fork.members, 2), vec![a, b]);
    assert_eq!(
        find_channel(&fork.channels, &name("review")).unwrap().size,
        2
    );
    assert_eq!(merge(&fork, "review", vec![a]).1, Err(Error::ChecksFailed));
    assert_eq!(
        run(
            &fork,
            who(1, 4),
            Command::Fork {
                repo: name("alpha"),
                source: name("main"),
                channel: name("review")
            }
        )
        .1,
        Err(Error::ChannelExists)
    );
}

#[test]
fn read_only_loads_never_commit() {
    let s = created();
    assert_eq!(run(&s, who(1, 1), Command::Inspect).1, Err(Error::ReadOnly));
}

fn grant(s: &Snapshot, by: u64, user: u64, level: u64) -> (Snapshot, Result<Reply, Error>) {
    run(
        s,
        who(1, by),
        Command::Grant {
            repo: name("alpha"),
            user,
            level,
        },
    )
}

#[test]
fn open_repositories_admit_every_member_until_restricted() {
    let s = created();
    // Open: anyone in the organization proposes, reviews and merges.
    let (s, a) = propose(&s, 1, 1, vec![]);
    let s = ready(&s, a);
    let (s, r) = merge(&s, "main", vec![a]);
    assert!(r.is_ok());
    // Restricting: the caller becomes an administrator and grants a reader.
    let (s, r) = grant(&s, 1, 9, READ);
    assert_eq!(
        r,
        Ok(Reply::Granted {
            user: 9,
            level: READ
        })
    );
    assert_eq!(level(&s.roles, 1), ADMIN);
    assert_eq!(level(&s.roles, 9), READ);
    // Users without a role no longer see the repository at all.
    assert_eq!(level(&s.roles, 2), 0);
    let (_, r) = run(
        &s,
        who(1, 2),
        Command::Approve {
            repo: name("alpha"),
            patch: a,
        },
    );
    assert_eq!(r, Err(Error::RepositoryNotFound));
    assert!(!readable(&who(1, 2), &s, &name("alpha")));
    assert!(readable(&who(1, 9), &s, &name("alpha")));
    // A reader cannot propose; a writer cannot review or grant.
    let propose_as = |s: &Snapshot, u: u64| {
        run(
            s,
            who(1, u),
            Command::Propose {
                repo: name("alpha"),
                hash: hash(2),
                dependencies: vec![],
                paths: vec![],
                body: vec![2],
            },
        )
        .1
    };
    assert_eq!(propose_as(&s, 9), Err(Error::Forbidden));
    let (s, _) = grant(&s, 1, 9, WRITE);
    assert!(propose_as(&s, 9).is_ok());
    assert_eq!(grant(&s, 9, 5, READ).1, Err(Error::Forbidden));
    let (_, r) = run(
        &s,
        who(1, 9),
        Command::Approve {
            repo: name("alpha"),
            patch: a,
        },
    );
    assert_eq!(r, Err(Error::Forbidden));
    // Revoking removes access; nobody changes their own level.
    let (s, _) = grant(&s, 1, 9, 0);
    assert_eq!(propose_as(&s, 9), Err(Error::RepositoryNotFound));
    assert_eq!(grant(&s, 1, 1, READ).1, Err(Error::OwnLevel));
    assert_eq!(grant(&s, 1, 7, ADMIN + 1).1, Err(Error::InvalidLevel));
    // Other organizations never see it, whatever their user id.
    assert_eq!(
        run(
            &s,
            who(2, 1),
            Command::Grant {
                repo: name("alpha"),
                user: 3,
                level: ADMIN
            }
        )
        .1,
        Err(Error::RepositoryNotFound)
    );
}

#[test]
fn restricting_cannot_demote_the_caller() {
    let s = created();
    assert_eq!(grant(&s, 1, 1, READ).1, Err(Error::OwnLevel));
    // Granting oneself admin on an open repository just restricts it.
    let (s, r) = grant(&s, 1, 1, ADMIN);
    assert!(r.is_ok());
    assert_eq!(s.roles.len(), 1);
    assert_eq!(level(&s.roles, 1), ADMIN);
}
