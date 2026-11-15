//! HTTP API v1 wire format, shared by `pico2peta-server` and this client.
//!
//! | Method | Path | Body / response |
//! |---|---|---|
//! | `PUT` | `/v1/objects[?delta_base=<hash>][&for_name=<name>][&group_width=2\|4]` | raw bytes (streamed) → [`PutResponse`]. Optional `x-p2p-expect-hash`; `delta_base` stores an XOR delta (v1.7) |
//! | `GET` | `/v1/objects/{hash}` | raw bytes; `Range: bytes=a-b` → 206 |
//! | `HEAD` | `/v1/objects/{hash}` | `content-length` = object size |
//! | `POST` | `/v1/flush` | makes earlier puts durable and visible to other handles |
//! | `GET` | `/v1/refs/{name}` | [`RefJson`] or 404 |
//! | `GET` | `/v1/refs?prefix=p` | [`RefList`] (only names the caller may read) |
//! | `POST` | `/v1/replace` | [`ReplaceRequest`] → 200/409 [`ReplaceResponse`] |
//! | `DELETE` | `/v1/refs/{name}` | [`DeleteRefRequest`] → 200/409 [`ReplaceResponse`] (OCC, rules apply) |
//! | `POST` | `/v1/update_many` | [`UpdateManyRequest`] → 200/409 [`ReplaceManyResponse`] (atomic sets + deletes) |
//! | `POST` | `/v1/replace_many` | [`ReplaceManyRequest`] → 200/409 [`ReplaceManyResponse`] (atomic) |
//! | `GET` | `/v1/namespaces` | [`RuleList`] |
//! | `PUT` | `/v1/namespaces/{prefix}` | [`SetRuleRequest`] (admin) |
//! | `GET` | `/v1/whoami` | [`WhoAmI`] |
//! | `POST` | `/v1/admin/gc?dry_run=bool&repack_all=bool` | [`GcReportJson`] (admin on `""`); 409 if a GC is running |
//!
//! **Operation lease:** GC reclaims objects that no ref reaches once the GC grace
//! period (default 1 h) has passed. A client must `replace` a ref to point at a
//! newly put object within that period, or put it again.
//!
//! Errors are `4xx/5xx` with an [`ErrorBody`]; `error.code` is
//! `pico2peta_core::Error::code()` or one of the HTTP-only codes in [`codes`].

use pico2peta_core::{Error, Hash, Name, Observed, RefValue, ReplaceManyOutcome, ReplaceOutcome, Rule};
use serde::{Deserialize, Serialize};

/// API version prefix.
pub const API_PREFIX: &str = "/v1";
/// Optional request header on `PUT /v1/objects`: the expected BLAKE3 hash (hex).
pub const EXPECT_HASH_HEADER: &str = "x-p2p-expect-hash";
/// Response header with the server-assigned request id.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// HTTP-only error codes (in addition to `core::Error::code()`).
pub mod codes {
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    pub const REF_NOT_FOUND: &str = "ref_not_found";
    pub const PERMISSION_DENIED: &str = "permission_denied";
    pub const HASH_MISMATCH: &str = "hash_mismatch";
    pub const TOO_LARGE: &str = "payload_too_large";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const SHUTTING_DOWN: &str = "shutting_down";
    pub const OVERLOADED: &str = "overloaded";
    pub const INTERNAL: &str = "internal";
    pub const GC_RUNNING: &str = "gc_running";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutResponse {
    pub hash: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefJson {
    pub name: String,
    pub hash: String,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefList {
    pub refs: Vec<RefJson>,
}

/// One read-set entry. `observed: null` means the name was absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSetEntry {
    pub name: String,
    pub observed: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceRequest {
    pub name: String,
    pub read_set: Vec<ReadSetEntry>,
    pub new: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReplaceResponse {
    Ok { version: u64 },
    Conflict { stale: Vec<ReadSetEntry> },
}

/// OCC delete: the target must be in `read_set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRefRequest {
    pub read_set: Vec<ReadSetEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateJson {
    pub name: String,
    pub new: String,
}

/// Atomic multi-name update including deletes (`new: null`), API v1.6.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManyRequest {
    pub updates: Vec<MaybeUpdateJson>,
    pub read_set: Vec<ReadSetEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaybeUpdateJson {
    pub name: String,
    pub new: Option<String>,
}

/// `(updates, read_set)` of a parsed [`UpdateManyRequest`].
pub type ParsedUpdateMany = (Vec<(Name, Option<Hash>)>, Vec<(Name, Observed)>);

impl UpdateManyRequest {
    pub fn from_core(updates: &[(Name, Option<Hash>)], read_set: &[(Name, Observed)]) -> Self {
        UpdateManyRequest {
            updates: updates
                .iter()
                .map(|(n, h)| MaybeUpdateJson { name: n.to_string(), new: h.map(|h| h.to_hex()) })
                .collect(),
            read_set: read_set.iter().map(|(n, o)| ReadSetEntry::from_core(n, *o)).collect(),
        }
    }
    pub fn to_core(&self) -> Result<ParsedUpdateMany, Error> {
        let u = self
            .updates
            .iter()
            .map(|u| Ok((Name::new(&u.name)?, u.new.as_deref().map(str::parse).transpose()?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let rs = self.read_set.iter().map(ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>()?;
        Ok((u, rs))
    }
}

/// Atomic multi-name update: every update target must be in `read_set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceManyRequest {
    pub updates: Vec<UpdateJson>,
    pub read_set: Vec<ReadSetEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReplaceManyResponse {
    Ok { versions: Vec<u64> },
    Conflict { stale: Vec<ReadSetEntry> },
}

/// `(updates, read_set)` of a parsed [`ReplaceManyRequest`].
pub type ParsedReplaceMany = (Vec<(Name, Hash)>, Vec<(Name, Observed)>);

impl ReplaceManyRequest {
    pub fn from_core(updates: &[(Name, Hash)], read_set: &[(Name, Observed)]) -> Self {
        ReplaceManyRequest {
            updates: updates.iter().map(|(n, h)| UpdateJson { name: n.to_string(), new: h.to_hex() }).collect(),
            read_set: read_set.iter().map(|(n, o)| ReadSetEntry::from_core(n, *o)).collect(),
        }
    }
    pub fn to_core(&self) -> Result<ParsedReplaceMany, Error> {
        let u = self
            .updates
            .iter()
            .map(|u| Ok((Name::new(&u.name)?, u.new.parse()?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let rs = self.read_set.iter().map(ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>()?;
        Ok((u, rs))
    }
}

impl ReplaceManyResponse {
    pub fn from_core(o: &ReplaceManyOutcome) -> Self {
        match o {
            ReplaceManyOutcome::Ok { versions } => ReplaceManyResponse::Ok { versions: versions.clone() },
            ReplaceManyOutcome::Conflict { stale } => ReplaceManyResponse::Conflict {
                stale: stale.iter().map(|(n, o)| ReadSetEntry::from_core(n, *o)).collect(),
            },
        }
    }
    pub fn to_core(&self) -> Result<ReplaceManyOutcome, Error> {
        Ok(match self {
            ReplaceManyResponse::Ok { versions } => ReplaceManyOutcome::Ok { versions: versions.clone() },
            ReplaceManyResponse::Conflict { stale } => ReplaceManyOutcome::Conflict {
                stale: stale.iter().map(ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>()?,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleJson {
    pub prefix: String,
    pub rule: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleList {
    pub rules: Vec<RuleJson>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetRuleRequest {
    pub rule: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoAmI {
    pub principal: String,
    pub authenticated: bool,
}

/// Result of one GC invocation (see `pico2peta_repo::GcReport`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReportJson {
    pub dry_run: bool,
    pub planned: Option<String>,
    pub swept: Vec<String>,
    pub condemned_packs: usize,
    pub copied_records: usize,
    pub rescued_records: usize,
    pub deleted_packs: usize,
    pub deleted_bytes: u64,
    pub live_hashes: usize,
    pub dangling_refs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    /// For `forbidden`: the governing rule (`Rule::as_str`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The name, prefix or hash the error is about, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

pub fn observed_to_wire(o: Observed) -> Option<u64> {
    match o {
        Observed::Absent => None,
        Observed::Version(v) => Some(v),
    }
}

pub fn observed_from_wire(o: Option<u64>) -> Observed {
    match o {
        None => Observed::Absent,
        Some(v) => Observed::Version(v),
    }
}

impl RefJson {
    pub fn from_core(name: &Name, v: &RefValue) -> RefJson {
        RefJson { name: name.to_string(), hash: v.hash.to_hex(), version: v.version }
    }
    pub fn to_core(&self) -> Result<(Name, RefValue), Error> {
        Ok((Name::new(&self.name)?, RefValue { hash: self.hash.parse()?, version: self.version }))
    }
}

impl ReadSetEntry {
    pub fn from_core(n: &Name, o: Observed) -> Self {
        ReadSetEntry { name: n.to_string(), observed: observed_to_wire(o) }
    }
    pub fn to_core(&self) -> Result<(Name, Observed), Error> {
        Ok((Name::new(&self.name)?, observed_from_wire(self.observed)))
    }
}

/// `(target, read_set, new_hash)` of a parsed [`ReplaceRequest`].
pub type ParsedReplace = (Name, Vec<(Name, Observed)>, Hash);

impl ReplaceRequest {
    pub fn from_core(name: &Name, read_set: &[(Name, Observed)], new: Hash) -> Self {
        ReplaceRequest {
            name: name.to_string(),
            read_set: read_set.iter().map(|(n, o)| ReadSetEntry::from_core(n, *o)).collect(),
            new: new.to_hex(),
        }
    }
    /// Parses into core types. Every name and the hash are validated.
    pub fn to_core(&self) -> Result<ParsedReplace, Error> {
        let rs = self.read_set.iter().map(ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>()?;
        Ok((Name::new(&self.name)?, rs, self.new.parse()?))
    }
}

impl ReplaceResponse {
    pub fn from_core(o: &ReplaceOutcome) -> Self {
        match o {
            ReplaceOutcome::Ok { version } => ReplaceResponse::Ok { version: *version },
            ReplaceOutcome::Conflict { stale } => {
                ReplaceResponse::Conflict { stale: stale.iter().map(|(n, o)| ReadSetEntry::from_core(n, *o)).collect() }
            }
        }
    }
    pub fn to_core(&self) -> Result<ReplaceOutcome, Error> {
        Ok(match self {
            ReplaceResponse::Ok { version } => ReplaceOutcome::Ok { version: *version },
            ReplaceResponse::Conflict { stale } => ReplaceOutcome::Conflict {
                stale: stale.iter().map(ReadSetEntry::to_core).collect::<Result<Vec<_>, _>>()?,
            },
        })
    }
}

/// Parses a rule from its wire name (`Rule::as_str`).
pub fn rule_from_wire(s: &str) -> Result<Rule, Error> {
    s.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_roundtrip() {
        let n = Name::new("a/b").unwrap();
        let rs = vec![(n.clone(), Observed::Absent), (Name::new("c").unwrap(), Observed::Version(3))];
        let h = Hash::of(b"x");
        let w = ReplaceRequest::from_core(&n, &rs, h);
        let j = serde_json::to_string(&w).unwrap();
        assert!(j.contains(r#""observed":null"#));
        let back: ReplaceRequest = serde_json::from_str(&j).unwrap();
        assert_eq!(back.to_core().unwrap(), (n.clone(), rs.clone(), h));

        let c = ReplaceOutcome::Conflict { stale: vec![(n, Observed::Version(2))] };
        let j = serde_json::to_string(&ReplaceResponse::from_core(&c)).unwrap();
        assert!(j.contains(r#""outcome":"conflict""#));
        let back: ReplaceResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(back.to_core().unwrap(), c);
    }

    #[test]
    fn rejects_bad_input() {
        let r = ReplaceRequest { name: "../x".into(), read_set: vec![], new: "zz".into() };
        assert!(r.to_core().is_err());
        let r = ReplaceRequest { name: "x".into(), read_set: vec![], new: "zz".into() };
        assert!(r.to_core().is_err());
    }
}
