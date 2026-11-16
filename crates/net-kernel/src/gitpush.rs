//! Git push acceptance (receive-pack): which ref updates a push may attempt, before the name
//! layer's compare-and-swap. The shell gathers the facts per command (the ref's current oid, the
//! new object's presence and kind); the decision is here.
//!
//! Spec (`verify/net/proofs/GitPush.lean`):
//! - a ref outside `refs/` or otherwise unmappable is refused;
//! - a delete (`new` = zero) is accepted when `old` is zero (delete whatever is there) or `old`
//!   is the current value;
//! - an update needs the new object present, a branch (`refs/heads/`) must point at a commit,
//!   and `old` must be the current value (zero = the ref must not exist);
//! - with `--atomic`, one refusal refuses every command.

/// Why a command is refused (git's `ng <ref> <reason>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ng {
    UnsupportedRef,
    StaleOld,
    MissingNew,
    BranchNotCommit,
    AtomicFailed,
}

/// The facts about one push command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushCmd {
    /// The ref name maps to a o5s name.
    pub ref_ok: bool,
    /// The ref is a branch (`refs/heads/...`).
    pub is_branch: bool,
    /// `new` is the zero id: delete.
    pub delete: bool,
    /// `old`, or `None` if it is the zero id.
    pub old: Option<Vec<u8>>,
    /// The ref's current oid, if it exists.
    pub cur: Option<Vec<u8>>,
    /// The new object is in this push or stored (and live).
    pub new_present: bool,
    /// The new object is a commit.
    pub new_is_commit: bool,
}

/// An accepted command: delete the ref, or point it at the new object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accept {
    Delete,
    Update,
}

fn same(a: &Option<Vec<u8>>, b: &Option<Vec<u8>>) -> bool {
    match a {
        Some(x) => match b {
            Some(y) => *x == *y,
            None => false,
        },
        None => match b {
            Some(_) => false,
            None => true,
        },
    }
}

/// One command's decision.
pub fn decide_cmd(c: &PushCmd) -> Result<Accept, Ng> {
    if !c.ref_ok {
        return Err(Ng::UnsupportedRef);
    }
    if c.delete {
        match c.old {
            None => return Ok(Accept::Delete),
            Some(_) => {
                if same(&c.cur, &c.old) {
                    return Ok(Accept::Delete);
                }
                return Err(Ng::StaleOld);
            }
        }
    }
    if !c.new_present {
        return Err(Ng::MissingNew);
    }
    if c.is_branch && !c.new_is_commit {
        return Err(Ng::BranchNotCommit);
    }
    if !same(&c.cur, &c.old) {
        return Err(Ng::StaleOld);
    }
    Ok(Accept::Update)
}

/// Each command's decision, and whether any is a refusal.
pub fn decide_all(cmds: &Vec<PushCmd>) -> (Vec<Result<Accept, Ng>>, bool) {
    let mut out: Vec<Result<Accept, Ng>> = Vec::new();
    let mut any_err = false;
    for c in cmds.iter() {
        let d = decide_cmd(c);
        any_err = any_err || is_err(&d);
        out.push(d);
    }
    (out, any_err)
}

fn is_err(d: &Result<Accept, Ng>) -> bool {
    match d {
        Ok(_) => false,
        Err(_) => true,
    }
}

/// An atomic push's refusal: every accepted command becomes `AtomicFailed`.
pub fn refuse(d: &Result<Accept, Ng>) -> Result<Accept, Ng> {
    match d {
        Ok(_) => Err(Ng::AtomicFailed),
        Err(e) => Err(*e),
    }
}

/// [`refuse`] on every decision.
pub fn refuse_all(ds: &Vec<Result<Accept, Ng>>) -> Vec<Result<Accept, Ng>> {
    let mut all: Vec<Result<Accept, Ng>> = Vec::new();
    for d in ds.iter() {
        all.push(refuse(d));
    }
    all
}

/// Every command's decision; with `atomic`, all refused if any is.
pub fn decide_push(cmds: &Vec<PushCmd>, atomic: bool) -> Vec<Result<Accept, Ng>> {
    let (out, any_err) = decide_all(cmds);
    if atomic && any_err { refuse_all(&out) } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(delete: bool, old: Option<&[u8]>, cur: Option<&[u8]>) -> PushCmd {
        PushCmd {
            ref_ok: true,
            is_branch: true,
            delete,
            old: old.map(<[u8]>::to_vec),
            cur: cur.map(<[u8]>::to_vec),
            new_present: true,
            new_is_commit: true,
        }
    }

    #[test]
    fn decisions() {
        assert_eq!(decide_cmd(&cmd(false, None, None)), Ok(Accept::Update));
        assert_eq!(decide_cmd(&cmd(false, None, Some(b"a"))), Err(Ng::StaleOld));
        assert_eq!(decide_cmd(&cmd(false, Some(b"a"), Some(b"a"))), Ok(Accept::Update));
        assert_eq!(decide_cmd(&cmd(true, None, Some(b"a"))), Ok(Accept::Delete));
        assert_eq!(decide_cmd(&cmd(true, Some(b"b"), Some(b"a"))), Err(Ng::StaleOld));
        let mut c = cmd(false, None, None);
        c.new_is_commit = false;
        assert_eq!(decide_cmd(&c), Err(Ng::BranchNotCommit));
        c.is_branch = false;
        assert_eq!(decide_cmd(&c), Ok(Accept::Update));
        c.new_present = false;
        assert_eq!(decide_cmd(&c), Err(Ng::MissingNew));
        c.ref_ok = false;
        assert_eq!(decide_cmd(&c), Err(Ng::UnsupportedRef));
        let cs = vec![cmd(false, None, None), cmd(false, None, Some(b"x"))];
        assert_eq!(decide_push(&cs, false), vec![Ok(Accept::Update), Err(Ng::StaleOld)]);
        assert_eq!(decide_push(&cs, true), vec![Err(Ng::AtomicFailed), Err(Ng::StaleOld)]);
    }
}
