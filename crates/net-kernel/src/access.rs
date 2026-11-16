//! Request authorization, behind h5i-app's guard (`h5i_app_std::authz::guard_viewed`).
//!
//! **Policy (the reviewed surface).** A principal holds grants `(prefix, access)`, with
//! access 1 = read, 2 = write, 3 = admin, each covering the names segment-aligned under its
//! prefix (`""` covers everything). A principal may:
//! - read or move a name only with read or write on it;
//! - read an **object** only if the object is reachable from some name it may read. The shell
//!   gives the kernel, for each requested hash, the *cells* that reach it: a cell is the
//!   deepest policy prefix covering a ref name, and every grant is a policy prefix, so a grant
//!   covers a ref iff it covers the ref's cell (core's reachability index; its maintenance
//!   invariant is core's proof obligation).
//!
//! Every decision goes through [`guarded`], an instance of `guard_viewed`. h5i-app proves for
//! every kernel and policy that a committed command's writes are permitted and its disclosed
//! rows stored and visible, and that read-only commands are noninterfering with what the view
//! hides (`verify/net/proofs`). Unauthorized and nonexistent are both refusals, which the shell
//! answers identically (404).

use h5i_app_std::authz::{Disclose, Policy, Transition, View};

pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const ADMIN: u8 = 3;

/// One policy grant: `access` on the names under `prefix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub prefix: Vec<u8>,
    pub access: u8,
}

/// The authenticated caller, as the policy resolves it: every grant that applies to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub grants: Vec<Grant>,
}

/// An object id (BLAKE3, 32 bytes). A byte vector, so its equality has a proven spec
/// (`H5iAppLib.vec_u8_eq_spec`).
pub type Hash = Vec<u8>;

/// What one request needs to see of the store: the refs it reads, and for each object it
/// reads, the cells (policy prefixes) from whose names the object is reachable. An object
/// absent from `objects` does not exist (or reaches no name).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub refs: Vec<(Vec<u8>, Hash)>,
    pub objects: Vec<(Hash, Vec<Vec<u8>>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Read an object's bytes (GET/HEAD `/v1/objects/<h>`, size, ranges).
    ReadObject(Hash),
    /// Resolve a name.
    ReadRef(Vec<u8>),
    /// Point a name at an object, or delete it (`None`). Whether it commits (OCC, rules) is
    /// core's decision; this is whether the caller may try.
    WriteRef(Vec<u8>, Option<Hash>),
}

/// A write a command makes: moving (or deleting) the ref `name`. Where it points is core's
/// name-layer decision; authorization judges only the name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Ref(Vec<u8>),
}

/// A row a reply discloses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Object(Hash),
    Ref(Vec<u8>, Hash),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub rows: Vec<Row>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
}

/// `s` is `prefix` or lies under `prefix/` (segment-aligned); `""` covers everything.
pub fn is_under(s: &[u8], prefix: &[u8]) -> bool {
    if prefix.len() == 0 {
        return true;
    }
    if s.len() < prefix.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if s[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    s.len() == prefix.len() || s[prefix.len()] == 47
}

/// Does `p` hold at least `need` on `name`? (Not named `access`: that is the module's name in
/// the extracted Lean.)
pub fn allows(p: &Principal, name: &[u8], need: u8) -> bool {
    for g in p.grants.iter() {
        if g.access >= need && is_under(name, &g.prefix) {
            return true;
        }
    }
    false
}

/// May `p` read some name in `cells` (an object reachable from those cells)?
pub fn reads_some(p: &Principal, cells: &Vec<Vec<u8>>) -> bool {
    for c in cells.iter() {
        if allows(p, c, READ) {
            return true;
        }
    }
    false
}

/// The cells reaching `h` in `s`, if `h` is there.
pub fn cells_of(s: &Snapshot, h: &Hash) -> Option<Vec<Vec<u8>>> {
    for e in s.objects.iter() {
        if e.0 == *h {
            return Some(e.1.clone());
        }
    }
    None
}

/// The ref `name` in `s`, if present.
pub fn ref_of(s: &Snapshot, name: &Vec<u8>) -> Option<Hash> {
    for e in s.refs.iter() {
        if e.0 == *name {
            return Some(e.1.clone());
        }
    }
    None
}

/// The guarded kernel's wiring and policy.
pub struct Access;

impl Transition<Principal, Snapshot, Command, Write, Reply, Error> for Access {
    fn kernel_transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Result<(Vec<Write>, Reply), Error> {
        crate::transition(actor, s, cmd)
    }
    fn kernel_apply(s: &Snapshot, ws: &Vec<Write>) -> Snapshot {
        crate::apply(s, ws)
    }
}

impl Policy<Principal, Snapshot, Write, Row> for Access {
    fn permits(actor: &Principal, _s: &Snapshot, w: &Write) -> bool {
        match w {
            Write::Ref(n) => allows(actor, n, WRITE),
        }
    }
    fn visible(actor: &Principal, s: &Snapshot, d: &Row) -> bool {
        match d {
            Row::Object(h) => match cells_of(s, h) {
                Some(cells) => reads_some(actor, &cells),
                None => false,
            },
            Row::Ref(n, _) => allows(actor, n, READ),
        }
    }
    fn stored(s: &Snapshot, d: &Row) -> bool {
        match d {
            Row::Object(h) => match cells_of(s, h) {
                Some(_) => true,
                None => false,
            },
            Row::Ref(n, h) => match ref_of(s, n) {
                Some(x) => x == *h,
                None => false,
            },
        }
    }
}

impl Disclose<Row> for Reply {
    fn disclosed(&self) -> Vec<Row> {
        self.rows.clone()
    }
}

impl View<Principal, Snapshot, Command> for Access {
    fn read_only(cmd: &Command) -> bool {
        match cmd {
            Command::ReadObject(_) => true,
            Command::ReadRef(_) => true,
            Command::WriteRef(_, _) => false,
        }
    }
    /// What the caller may see: readable refs, and objects reachable from a readable cell.
    fn view(actor: &Principal, s: &Snapshot) -> Snapshot {
        let mut refs = Vec::new();
        for e in s.refs.iter() {
            if allows(actor, &e.0, READ) {
                refs.push(e.clone());
            }
        }
        let mut objects = Vec::new();
        for e in s.objects.iter() {
            if reads_some(actor, &e.1) {
                objects.push(e.clone());
            }
        }
        Snapshot { refs, objects }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guarded_transition as guarded;
    use h5i_app_std::authz::Guarded;

    fn p(gs: &[(&str, u8)]) -> Principal {
        Principal { grants: gs.iter().map(|(n, a)| Grant { prefix: n.as_bytes().to_vec(), access: *a }).collect() }
    }

    #[test]
    fn under() {
        assert!(is_under(b"a/b", b"a"));
        assert!(is_under(b"a", b"a"));
        assert!(!is_under(b"ab", b"a"));
        assert!(is_under(b"x", b""));
    }

    #[test]
    fn objects_follow_reachability() {
        let h1 = vec![1u8; 32];
        let shared = vec![2u8; 32];
        let s = Snapshot {
            refs: vec![(b"team-a/x".to_vec(), h1.clone())],
            objects: vec![
                (h1.clone(), vec![b"team-a".to_vec()]),
                (shared.clone(), vec![b"team-a".to_vec(), b"team-b".to_vec()]),
            ],
        };
        let a = p(&[("team-a", READ)]);
        let b = p(&[("team-b", READ)]);
        // Cross-team read: denied, exactly like a missing object.
        assert_eq!(guarded(&b, &s, &Command::ReadObject(h1.clone())), Err(Guarded::App(Error::NotFound)));
        assert!(guarded(&a, &s, &Command::ReadObject(h1.clone())).is_ok());
        // A shared blob is readable by both.
        assert!(guarded(&a, &s, &Command::ReadObject(shared.clone())).is_ok());
        assert!(guarded(&b, &s, &Command::ReadObject(shared.clone())).is_ok());
        // Unknown objects.
        assert_eq!(guarded(&a, &s, &Command::ReadObject(vec![9u8; 32])), Err(Guarded::App(Error::NotFound)));
        // Refs: read needs read, write needs write.
        assert!(guarded(&a, &s, &Command::ReadRef(b"team-a/x".to_vec())).is_ok());
        assert_eq!(guarded(&a, &s, &Command::WriteRef(b"team-a/x".to_vec(), None)), Err(Guarded::Denied));
        let w = p(&[("team-a", WRITE)]);
        assert!(guarded(&w, &s, &Command::WriteRef(b"team-a/x".to_vec(), None)).is_ok());
        // Admin on "" reads everything.
        assert!(guarded(&p(&[("", ADMIN)]), &s, &Command::ReadObject(h1.clone())).is_ok());
    }
}
