//! Signed attestations and policy-gated promotion (ROADMAP P5).
//!
//! - **Format:** an attestation is an [in-toto Statement v1] (subjects with a
//!   `blake3` digest, plus `predicateType` and `predicate`) wrapped in a [DSSE]
//!   envelope and signed over DSSE's pre-authentication encoding (PAE). Both
//!   are the standard JSON forms, so existing tools can read them.
//! - **Keys:** pluggable ([`Signer`], [`Verifier`]). Ed25519 is built in;
//!   Sigstore can be added behind the same traits.
//! - **Storage:** the envelope is an ordinary object, linked to its subject by
//!   a provenance record (`op = "attest"`, input `subject`, params = the
//!   envelope). It inherits provenance's GC rule: params stay alive while the
//!   record does, and attestations of an object are found via
//!   `Provenance::consumers(subject)`.
//! - **Promotion:** [`promote`] moves `dst` to the object `src` names, if a
//!   [`Policy`] holds (required signatures, provenance present). The move is
//!   one OCC transaction that also pins `src`, and it is recorded as a
//!   provenance record (`op = "promote"`), which is the audit trail.
//!
//! [in-toto Statement v1]: https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md
//! [DSSE]: https://github.com/secure-systems-lab/dsse/blob/master/envelope.md
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use base64::Engine;
use pico2peta_core::{Error, Hash, Name, Observed, RefValue, Repository, Result};
use pico2peta_prov::{Provenance, Record};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// DSSE payload type of an in-toto statement.
pub const INTOTO_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// in-toto Statement v1 type.
pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
/// Provenance op of an attestation record.
pub const OP_ATTEST: &str = "attest";
/// Provenance op of a promotion record.
pub const OP_PROMOTE: &str = "promote";
/// Upper bound on an envelope's size (decoder limit).
pub const MAX_ENVELOPE: usize = 1 << 20;
/// Upper bound on signatures per envelope.
pub const MAX_SIGNATURES: usize = 64;

/// Signs messages.
pub trait Signer {
    /// The key id recorded in the envelope.
    fn keyid(&self) -> String;
    /// A signature over `msg`.
    fn sign(&self, msg: &[u8]) -> Vec<u8>;
}

/// Verifies signatures by one key.
pub trait Verifier {
    /// The key id this verifier checks.
    fn keyid(&self) -> String;
    /// Whether `sig` is a valid signature over `msg`.
    fn verify(&self, msg: &[u8], sig: &[u8]) -> bool;
}

/// An Ed25519 key pair (built-in [`Signer`]).
pub struct Ed25519Signer {
    key: ed25519_dalek::SigningKey,
}

/// An Ed25519 public key (built-in [`Verifier`]).
#[derive(Clone)]
pub struct Ed25519Verifier {
    key: ed25519_dalek::VerifyingKey,
}

/// Key id of an Ed25519 public key: `"ed25519:"` + hex SHA-256 of the key.
fn ed25519_keyid(pk: &ed25519_dalek::VerifyingKey) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(pk.as_bytes());
    format!("ed25519:{}", hex(&d))
}

impl Ed25519Signer {
    /// A key from a 32-byte secret seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Ed25519Signer { key: ed25519_dalek::SigningKey::from_bytes(seed) }
    }

    /// The matching verifier.
    pub fn verifier(&self) -> Ed25519Verifier {
        Ed25519Verifier { key: self.key.verifying_key() }
    }
}

impl Signer for Ed25519Signer {
    fn keyid(&self) -> String {
        ed25519_keyid(&self.key.verifying_key())
    }
    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer as _;
        self.key.sign(msg).to_bytes().to_vec()
    }
}

impl Ed25519Verifier {
    /// A verifier from a 32-byte public key.
    pub fn from_public(bytes: &[u8; 32]) -> Result<Self> {
        ed25519_dalek::VerifyingKey::from_bytes(bytes)
            .map(|key| Ed25519Verifier { key })
            .map_err(|e| Error::Parse(format!("ed25519 public key: {e}")))
    }

    /// The raw public key.
    pub fn public(&self) -> [u8; 32] {
        self.key.to_bytes()
    }
}

impl Verifier for Ed25519Verifier {
    fn keyid(&self) -> String {
        ed25519_keyid(&self.key)
    }
    fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else { return false };
        self.key.verify_strict(msg, &sig).is_ok()
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// DSSE pre-authentication encoding: `"DSSEv1" SP len(type) SP type SP len(body) SP body`.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut v = format!("DSSEv1 {} {} {} ", payload_type.len(), payload_type, payload.len()).into_bytes();
    v.extend_from_slice(payload);
    v
}

/// One subject of a statement: a name and the object's BLAKE3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    /// A human-meaningful name (e.g. `pypi/proj/1.0/proj-1.0-py3-none-any.whl`).
    pub name: String,
    /// The object.
    pub hash: Hash,
}

/// An in-toto Statement v1.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    /// What the statement is about.
    pub subjects: Vec<Subject>,
    /// The predicate's type URI (e.g. `https://slsa.dev/provenance/v1`).
    pub predicate_type: String,
    /// The predicate (any JSON).
    pub predicate: Value,
}

impl Statement {
    /// The canonical JSON payload (keys sorted, as serde_json's `Value` maps are).
    pub fn to_json(&self) -> Vec<u8> {
        let subjects: Vec<Value> =
            self.subjects.iter().map(|s| json!({"name": s.name, "digest": {"blake3": s.hash.to_string()}})).collect();
        let v = json!({
            "_type": STATEMENT_TYPE,
            "subject": subjects,
            "predicateType": self.predicate_type,
            "predicate": self.predicate,
        });
        serde_json::to_vec(&v).unwrap_or_default()
    }

    /// Parses a statement payload.
    pub fn from_json(b: &[u8]) -> Result<Statement> {
        let bad = |m: &str| Error::Parse(format!("in-toto statement: {m}"));
        let v: Value = serde_json::from_slice(b).map_err(|e| bad(&e.to_string()))?;
        if v.get("_type").and_then(Value::as_str) != Some(STATEMENT_TYPE) {
            return Err(bad("_type"));
        }
        let mut subjects = Vec::new();
        for s in v.get("subject").and_then(Value::as_array).ok_or_else(|| bad("subject"))? {
            let name = s.get("name").and_then(Value::as_str).ok_or_else(|| bad("subject.name"))?;
            let d = s.get("digest").and_then(|d| d.get("blake3")).and_then(Value::as_str);
            let hash: Hash =
                d.ok_or_else(|| bad("subject.digest.blake3"))?.parse().map_err(|_| bad("blake3 digest"))?;
            subjects.push(Subject { name: name.to_string(), hash });
        }
        let predicate_type =
            v.get("predicateType").and_then(Value::as_str).ok_or_else(|| bad("predicateType"))?.to_string();
        let predicate = v.get("predicate").cloned().unwrap_or(Value::Null);
        Ok(Statement { subjects, predicate_type, predicate })
    }
}

/// A DSSE envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// The payload type (e.g. [`INTOTO_PAYLOAD_TYPE`]).
    pub payload_type: String,
    /// The payload bytes.
    pub payload: Vec<u8>,
    /// `(keyid, signature)` pairs.
    pub signatures: Vec<(String, Vec<u8>)>,
}

impl Envelope {
    /// Signs `statement` with every signer.
    pub fn sign(statement: &Statement, signers: &[&dyn Signer]) -> Envelope {
        let payload = statement.to_json();
        let msg = pae(INTOTO_PAYLOAD_TYPE, &payload);
        let signatures = signers.iter().map(|s| (s.keyid(), s.sign(&msg))).collect();
        Envelope { payload_type: INTOTO_PAYLOAD_TYPE.into(), payload, signatures }
    }

    /// The standard DSSE JSON form (base64 payload and signatures).
    pub fn to_json(&self) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let sigs: Vec<Value> = self.signatures.iter().map(|(k, s)| json!({"keyid": k, "sig": b64.encode(s)})).collect();
        let v = json!({"payloadType": self.payload_type, "payload": b64.encode(&self.payload), "signatures": sigs});
        serde_json::to_vec(&v).unwrap_or_default()
    }

    /// Parses the DSSE JSON form, with size and count limits.
    pub fn from_json(b: &[u8]) -> Result<Envelope> {
        let bad = |m: &str| Error::Parse(format!("DSSE envelope: {m}"));
        if b.len() > MAX_ENVELOPE {
            return Err(bad("too large"));
        }
        let v: Value = serde_json::from_slice(b).map_err(|e| bad(&e.to_string()))?;
        let b64 = base64::engine::general_purpose::STANDARD;
        let payload_type = v.get("payloadType").and_then(Value::as_str).ok_or_else(|| bad("payloadType"))?.to_string();
        let payload = b64
            .decode(v.get("payload").and_then(Value::as_str).ok_or_else(|| bad("payload"))?)
            .map_err(|_| bad("payload base64"))?;
        let sigs = v.get("signatures").and_then(Value::as_array).ok_or_else(|| bad("signatures"))?;
        if sigs.len() > MAX_SIGNATURES {
            return Err(bad("too many signatures"));
        }
        let mut signatures = Vec::with_capacity(sigs.len());
        for s in sigs {
            let keyid = s.get("keyid").and_then(Value::as_str).unwrap_or("").to_string();
            let sig = b64
                .decode(s.get("sig").and_then(Value::as_str).ok_or_else(|| bad("sig"))?)
                .map_err(|_| bad("sig base64"))?;
            signatures.push((keyid, sig));
        }
        Ok(Envelope { payload_type, payload, signatures })
    }

    /// Key ids whose signature verifies with the given verifiers.
    pub fn verified_by(&self, verifiers: &[&dyn Verifier]) -> Vec<String> {
        let msg = pae(&self.payload_type, &self.payload);
        let mut out = Vec::new();
        for v in verifiers {
            let id = v.keyid();
            if self.signatures.iter().any(|(k, s)| *k == id && v.verify(&msg, s)) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// The statement inside (if the payload type is in-toto).
    pub fn statement(&self) -> Result<Statement> {
        if self.payload_type != INTOTO_PAYLOAD_TYPE {
            return Err(Error::Parse(format!("not an in-toto envelope: {}", self.payload_type)));
        }
        Statement::from_json(&self.payload)
    }
}

/// Stores `envelope` and links it to each of its statement's subjects
/// (provenance records, `op = "attest"`). Returns the envelope's hash.
pub fn attest(repo: &dyn Repository, log_ref: &Name, envelope: &Envelope) -> Result<Hash> {
    let st = envelope.statement()?;
    if st.subjects.is_empty() {
        return Err(Error::Parse("attestation without a subject".into()));
    }
    let eh = repo.put(&envelope.to_json())?;
    let recs: Vec<Record> = st
        .subjects
        .iter()
        .map(|s| Record {
            op: OP_ATTEST.into(),
            params: Some(eh),
            inputs: [("subject".to_string(), s.hash)].into_iter().collect(),
            outputs: BTreeMap::new(),
            meta: [
                ("predicate_type".to_string(), st.predicate_type.clone()),
                ("subject_name".to_string(), s.name.clone()),
                (
                    "keyids".to_string(),
                    envelope.signatures.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(","),
                ),
            ]
            .into_iter()
            .collect(),
        })
        .collect();
    pico2peta_prov::record(repo, log_ref, &recs)?;
    Ok(eh)
}

/// Every stored attestation envelope about `subject`.
pub fn attestations(repo: &dyn Repository, log_ref: &Name, subject: &Hash) -> Result<Vec<Envelope>> {
    let p = Provenance::load(repo, log_ref)?;
    let mut out = Vec::new();
    for (_, r) in p.consumers(subject)? {
        if r.op != OP_ATTEST {
            continue;
        }
        if let Some(eh) = r.params {
            out.push(Envelope::from_json(&repo.get(&eh, None)?)?);
        }
    }
    Ok(out)
}

/// What a promotion requires.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Key ids that must each have signed an attestation about the object
    /// (verified with the verifiers passed to [`promote`]).
    pub required_keys: Vec<String>,
    /// If set, the required attestations must have this predicate type.
    pub predicate_type: Option<String>,
    /// The object must have a provenance record that produced it.
    pub require_provenance: bool,
}

/// The result of a [`promote`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromoteOutcome {
    /// `dst` now names the object; the promotion record's hash.
    Promoted {
        /// The promoted object.
        object: Hash,
        /// The provenance record of the promotion (the audit entry).
        record: Hash,
    },
    /// The policy refused it: the reasons.
    Refused(Vec<String>),
    /// `src` or `dst` moved concurrently (OCC conflict); nothing changed.
    Conflict,
}

/// Promotes the object `src` names to `dst` (e.g. `dev/app` → `prod/app`) if
/// `policy` holds. The name move is one OCC transaction with a read set on
/// both names, and the promotion is recorded as a provenance record
/// (`op = "promote"`, meta: source, destination, actor, the verifying keys).
/// `dst`'s name rules still apply (e.g. an immutable `prod/` refuses a second promotion).
pub fn promote(
    repo: &dyn Repository,
    log_ref: &Name,
    src: &Name,
    dst: &Name,
    policy: &Policy,
    verifiers: &[&dyn Verifier],
    actor: &str,
) -> Result<PromoteOutcome> {
    let Some(sv) = repo.resolve(src)? else {
        return Ok(PromoteOutcome::Refused(vec![format!("{src} does not exist")]));
    };
    let obj = sv.hash;
    let mut reasons = Vec::new();
    let p = Provenance::load(repo, log_ref)?;
    if policy.require_provenance && p.producers(&obj)?.is_empty() {
        reasons.push(format!("{obj} has no provenance record"));
    }
    let mut signed: Vec<String> = Vec::new();
    for (_, r) in p.consumers(&obj)? {
        if r.op != OP_ATTEST {
            continue;
        }
        let Some(eh) = r.params else { continue };
        let env = Envelope::from_json(&repo.get(&eh, None)?)?;
        let Ok(st) = env.statement() else { continue };
        if !st.subjects.iter().any(|s| s.hash == obj) {
            continue;
        }
        if policy.predicate_type.as_ref().is_some_and(|t| *t != st.predicate_type) {
            continue;
        }
        for k in env.verified_by(verifiers) {
            if !signed.contains(&k) {
                signed.push(k);
            }
        }
    }
    for k in &policy.required_keys {
        if !signed.contains(k) {
            reasons.push(format!("missing a valid signature by {k}"));
        }
    }
    if !reasons.is_empty() {
        return Ok(PromoteOutcome::Refused(reasons));
    }
    let dv = repo.resolve(dst)?;
    let read_set = [(src.clone(), Observed::Version(sv.version)), (dst.clone(), RefValue::observed(dv))];
    match repo.update_many(&[(dst.clone(), Some(obj))], &read_set)? {
        pico2peta_core::ReplaceManyOutcome::Ok { .. } => {}
        pico2peta_core::ReplaceManyOutcome::Conflict { .. } => return Ok(PromoteOutcome::Conflict),
    }
    let rec = Record {
        op: OP_PROMOTE.into(),
        params: None,
        inputs: [("object".to_string(), obj)].into_iter().collect(),
        outputs: BTreeMap::new(),
        meta: [
            ("src".to_string(), src.to_string()),
            ("dst".to_string(), dst.to_string()),
            ("actor".to_string(), actor.to_string()),
            ("verified_keys".to_string(), signed.join(",")),
        ]
        .into_iter()
        .collect(),
    };
    let hs = pico2peta_prov::record(repo, log_ref, &[rec])?;
    Ok(PromoteOutcome::Promoted { object: obj, record: hs.first().copied().unwrap_or(obj) })
}
