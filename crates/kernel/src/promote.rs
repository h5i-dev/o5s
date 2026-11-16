//! Policy-gated promotion (`o5s_attest::promote`): may `src`'s object
//! become `dst`'s, and with which OCC batch?
//!
//! The shell verifies signatures and reads provenance (trusted: crypto and
//! storage); the kernel gets the facts and decides. The plan it returns moves
//! only `dst`, to `src`'s current object, with a read set that pins `src` at
//! its current version and `dst` as observed (P7). Committed through
//! `refs::decide`, a promotion therefore never moves anything but `dst`, and
//! never from a stale `src` (P2).

use crate::names::cmp;
use crate::{Hash, Observed, RefVal};

/// What the shell found about one attestation of the object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attestation {
    /// The statement names the object as a subject.
    pub about_object: bool,
    /// The statement's predicate type.
    pub predicate_type: Vec<u8>,
    /// Key ids whose signatures on the envelope verified.
    pub verified_keys: Vec<Vec<u8>>,
}

/// The promotion policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Each must have signed an attestation about the object.
    pub required_keys: Vec<Vec<u8>>,
    /// If set, only attestations with this predicate type count.
    pub predicate_type: Option<Vec<u8>>,
    /// The object must have a provenance record that produced it.
    pub require_provenance: bool,
}

/// Why a promotion is refused: every unmet requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// `src` does not exist (nothing else is checked then).
    pub no_source: bool,
    /// No provenance record produced the object (and the policy requires one).
    pub no_provenance: bool,
    /// Required keys with no valid signature on a counting attestation, in policy order.
    pub keys: Vec<Vec<u8>>,
}

/// An accepted promotion: set `dst` to `object` in one OCC batch over this
/// read set (`src` then `dst`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub object: Hash,
    pub src_observed: Observed,
    pub dst_observed: Observed,
}

/// Whether attestation `a` counts under the policy.
pub fn counts(policy: &Policy, a: &Attestation) -> bool {
    if !a.about_object {
        return false;
    }
    match &policy.predicate_type {
        None => true,
        Some(t) => cmp(t, &a.predicate_type) == 0,
    }
}

/// Whether key `k` signed some counting attestation.
pub fn signed_by(policy: &Policy, atts: &Vec<Attestation>, k: &Vec<u8>) -> bool {
    let mut i: usize = 0;
    while i < atts.len() {
        if counts(policy, &atts[i]) && has_key(&atts[i].verified_keys, k) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether `k` is in `keys`.
pub fn has_key(keys: &Vec<Vec<u8>>, k: &Vec<u8>) -> bool {
    let mut i: usize = 0;
    while i < keys.len() {
        if cmp(&keys[i], k) == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// The required keys that signed no counting attestation, in policy order.
pub fn keys_missing(policy: &Policy, atts: &Vec<Attestation>) -> Vec<Vec<u8>> {
    let mut missing: Vec<Vec<u8>> = Vec::new();
    let mut i: usize = 0;
    while i < policy.required_keys.len() {
        match key_missing(policy, atts, i) {
            Some(m) => missing.push(m),
            None => {}
        }
        i += 1;
    }
    missing
}

/// Required key `i` if it signed no counting attestation.
pub fn key_missing(policy: &Policy, atts: &Vec<Attestation>, i: usize) -> Option<Vec<u8>> {
    if i >= policy.required_keys.len() {
        return None;
    }
    if signed_by(policy, atts, &policy.required_keys[i]) { None } else { Some(policy.required_keys[i].clone()) }
}

/// The policy requires provenance and the object has none.
pub fn prov_missing(required: bool, has_producer: bool) -> bool {
    if required { !has_producer } else { false }
}

/// The observed state of a current value.
pub fn observed(v: &Option<RefVal>) -> Observed {
    match v {
        Some(d) => Observed::Version(d.version),
        None => Observed::Absent,
    }
}

/// Decides a promotion. `src` and `dst` are the names' current values;
/// `has_producer`: a provenance record produced `src`'s object.
pub fn decide_promote(
    policy: &Policy,
    src: &Option<RefVal>,
    dst: &Option<RefVal>,
    has_producer: bool,
    atts: &Vec<Attestation>,
) -> Result<Plan, Refused> {
    match src {
        None => Err(Refused { no_source: true, no_provenance: false, keys: Vec::new() }),
        Some(s) => {
            let no_provenance = prov_missing(policy.require_provenance, has_producer);
            let keys = keys_missing(policy, atts);
            if no_provenance || keys.len() > 0 {
                return Err(Refused { no_source: false, no_provenance, keys });
            }
            Ok(Plan { object: s.hash, src_observed: Observed::Version(s.version), dst_observed: observed(dst) })
        }
    }
}
