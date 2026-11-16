//! o5s's front decisions and wire parsers, written in the Rust subset that Charon and
//! Aeneas extract to Lean 4 (h5i-app). The theorems in `verify/net/proofs` are about this
//! code as extracted. The server calls these functions; it does not reimplement them.

pub mod access;
pub mod gitpush;
pub mod parse;
pub mod publish;

use access::{Access, Command, Error, Hash, Principal, Reply, Row, Snapshot, Write, cells_of, ref_of};

// The access kernel's `transition` and `apply` (at the crate root, where h5i's `h5i_authz`
// looks for them; `access::Access` delegates to them).
pub fn one_row(r: Row) -> Reply {
    let mut rows = Vec::new();
    rows.push(r);
    Reply { rows }
}

/// The kernel behind the guard. It only reads: whether a read is allowed is the policy's
/// question, not the handler's.
pub fn transition(_p: &Principal, s: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
    match cmd {
        Command::ReadObject(h) => match cells_of(s, h) {
            Some(_) => Ok((Vec::new(), one_row(Row::Object(h.clone())))),
            None => Err(Error::NotFound),
        },
        Command::ReadRef(n) => match ref_of(s, n) {
            Some(h) => Ok((Vec::new(), one_row(Row::Ref(n.clone(), h)))),
            None => Err(Error::NotFound),
        },
        Command::WriteRef(n, _new) => {
            let mut ws = Vec::new();
            ws.push(Write::Ref(n.clone()));
            Ok((ws, Reply { rows: Vec::new() }))
        }
    }
}

/// A write's meaning on the view: the moved ref leaves it (its new value is core's to decide).
/// Objects' reachability after a move is the index's business, not this view's.
pub fn apply(s: &Snapshot, ws: &Vec<Write>) -> Snapshot {
    let mut refs = s.refs.clone();
    for w in ws.iter() {
        match w {
            Write::Ref(n) => {
                refs = drop_ref(&refs, n);
            }
        }
    }
    Snapshot { refs, objects: s.objects.clone() }
}

pub fn drop_ref(refs: &Vec<(Vec<u8>, Hash)>, n: &Vec<u8>) -> Vec<(Vec<u8>, Hash)> {
    let mut out = Vec::new();
    for e in refs.iter() {
        if e.0 != *n {
            out.push(e.clone());
        }
    }
    out
}

// Every request's authorization decision: the access kernel behind h5i-app's guard
// (`guard_viewed`). The proofs are about `guarded_transition` (`verify/net`, `[authz]`).
h5i_app_std::authz_kernel! {
    Access {
        principal: access::Principal, snapshot: access::Snapshot, command: access::Command,
        write: access::Write, reply: access::Reply, error: access::Error, row: access::Row,
        tenant: |_a| 0,
    }
}
