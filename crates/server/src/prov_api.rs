//! Cross-front provenance, search, attestations, promotion and the audit query (ROADMAP P5,
//! front side). Everything goes through the core's provenance log (`_prov/log`).
//!
//! **External ids.** Clients name objects by what they know. [`resolve_ref`] maps these to
//! core objects, checking read access on each front's namespace:
//! - `blake3:<hex>` (or bare hex): a core object (readable by reachability, as every id is);
//! - `name:<core name>`: what a name points at;
//! - `git:<repo>@<oid>`: a git object (SHA-1 or SHA-256 id);
//! - `pypi:<index>/<project>/<file>`, `npm:<registry>/<package>@<version>`,
//!   `maven:<repo>/<path>`: a package file's content;
//! - `oci:<repo>@sha256:<hex>`: a manifest or blob of that repository.
//!
//! **Automatic records.** Every front records its publishes ([`record_publish`]):
//! `op = "<front>.publish"`, the stored object as output `artifact`, and the front's ids in
//! `meta` (which makes them searchable).

use crate::api::{AppState, Caller};
use crate::audit::Event;
use crate::auth::Principal;
use crate::config::Access;
use crate::error::{ApiError, ApiResult};
use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pico2peta_core::{Error, Hash, Name};
use pico2peta_prov::{DEFAULT_LOG_REF, Provenance, Record};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn log_ref() -> Name {
    Name::new(DEFAULT_LOG_REF).unwrap_or_else(|_| unreachable!("static name is valid"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> pico2peta_core::Result<T> + Send + 'static) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(e) => Err(ApiError::internal(format!("worker panicked: {e}"))),
    }
}

/// Records a front's publish (best effort: a failed record is logged, not fatal to the
/// publish, which has already happened).
pub(crate) fn record_publish(
    st: &AppState,
    front: &str,
    output: Hash,
    principal: &str,
    meta: BTreeMap<String, String>,
) {
    record_publishes(st, front, principal, vec![(output, meta)]);
}

/// Records several publishes of one request (e.g. every ref of a git push) in one log update:
/// one record per object, but a single OCC append to the provenance log instead of one per ref.
pub(crate) fn record_publishes(
    st: &AppState,
    front: &str,
    principal: &str,
    items: Vec<(Hash, BTreeMap<String, String>)>,
) {
    if items.is_empty() {
        return;
    }
    let recs: Vec<Record> = items
        .into_iter()
        .map(|(output, mut meta)| {
            meta.insert("front".into(), front.into());
            meta.insert("principal".into(), principal.into());
            Record {
                op: format!("{front}.publish"),
                outputs: [("artifact".to_string(), output)].into_iter().collect(),
                meta,
                ..Default::default()
            }
        })
        .collect();
    let repo = st.repo.clone();
    let front = front.to_string();
    tokio::task::spawn_blocking(move || {
        // At most 2^16 records per call (the provenance crate's limit).
        for batch in recs.chunks(1 << 16) {
            if let Err(e) = pico2peta_prov::record(&*repo, &log_ref(), batch) {
                tracing::warn!(front, error = %e, "provenance record of a publish failed");
            }
        }
    });
}

/// The namespace that authorizes reading an external id (`None` for raw hashes).
fn namespace_of(r: &str) -> Option<String> {
    let (scheme, rest) = r.split_once(':')?;
    Some(match scheme {
        "name" => rest.to_string(),
        "git" => format!("git/{}", rest.split('@').next()?),
        "pypi" => format!("pypi/{}", rest.split('/').next()?),
        "npm" => format!("npm/{}", rest.split('/').next()?),
        "maven" => format!("maven/{}", rest.split('/').next()?),
        "oci" => format!("oci/{}", rest.split('@').next()?),
        _ => return None,
    })
}

/// Resolves an external id to a core object, checking read access.
pub(crate) async fn resolve_ref(st: &Arc<AppState>, p: &Principal, c: &Caller, r: &str) -> ApiResult<Hash> {
    match namespace_of(r) {
        Some(ns) => st.require(st.authz.allows(p, &ns, Access::Read), p, c, "resolve_ref", &ns)?,
        None => st.require(st.authz.allows_somewhere(p, Access::Read), p, c, "resolve_ref", "objects")?,
    }
    let bad = |m: &str| ApiError::bad_request(format!("{r}: {m}"));
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", format!("{r} not found"));
    let (scheme, rest) = r.split_once(':').unwrap_or(("blake3", r));
    let repo = st.repo.clone();
    let resolve_name = |n: String| -> ApiResult<Name> { Name::new(n).map_err(|e| bad(&e.to_string())) };
    let h = match scheme {
        "blake3" => {
            let h: Hash = rest.parse().map_err(|_| bad("not a BLAKE3 hash"))?;
            Some(h)
        }
        "name" => {
            let n = resolve_name(rest.into())?;
            blocking(move || Ok(repo.resolve(&n)?.map(|v| v.hash))).await?
        }
        "git" => {
            let (repo_name, oid) = rest.split_once('@').ok_or_else(|| bad("expected git:<repo>@<oid>"))?;
            let oid = crate::git::parse_oid(oid.as_bytes()).ok_or_else(|| bad("bad git object id"))?;
            let git = st.git.clone().ok_or_else(|| bad("the git front is disabled"))?;
            let _ = repo_name;
            blocking(move || Ok(crate::git::View::new(&git)?.lookup(&oid)?.map(|x| x.1))).await?
        }
        "pypi" | "npm" | "maven" => {
            let name = match scheme {
                "npm" => {
                    let (pkgpart, ver) =
                        rest.rsplit_once('@').ok_or_else(|| bad("expected npm:<registry>/<package>@<version>"))?;
                    let (reg, pkg) =
                        pkgpart.split_once('/').ok_or_else(|| bad("expected npm:<registry>/<package>@<version>"))?;
                    let seg = match pkg.strip_prefix('@').and_then(|s| s.split_once('/')) {
                        Some((scope, n)) => format!("{scope}+{n}"),
                        None => pkg.to_string(),
                    };
                    format!("npm/{reg}/{seg}/{ver}")
                }
                "pypi" => {
                    let mut it = rest.splitn(3, '/');
                    let (i, proj, file) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
                    format!("pypi/{i}/{}/{file}", crate::pypi::normalize(proj))
                }
                _ => format!("maven/{rest}"),
            };
            let n = resolve_name(name)?;
            // The name points at the front's record; its content hash follows the 8-byte magic.
            blocking(move || {
                Ok(match repo.resolve(&n)? {
                    Some(v) => {
                        let b = repo.get(&v.hash, Some(0..40))?;
                        (b.len() == 40 && b.starts_with(b"P2P")).then(|| Hash(b[8..40].try_into().unwrap_or([0; 32])))
                    }
                    None => None,
                })
            })
            .await?
        }
        "oci" => {
            let (orepo, digest) = rest.split_once('@').ok_or_else(|| bad("expected oci:<repo>@sha256:<hex>"))?;
            let hex = digest.strip_prefix("sha256:").ok_or_else(|| bad("expected a sha256 digest"))?.to_string();
            let oci = st.oci.clone().ok_or_else(|| bad("the OCI front is disabled"))?;
            oci.resolve_digest(orepo, &hex).await.map_err(|e| bad(&e))?
        }
        _ => return Err(bad("unknown id scheme (blake3, name, git, pypi, npm, maven, oci)")),
    };
    // Whatever the scheme, the caller must be able to read the object by reachability: a
    // `blake3:` id needs no name, and git oids are global across repositories, so a namespace
    // check alone would resolve other teams' objects. Refused exactly like a missing id.
    let h = h.ok_or_else(not_found)?;
    if st.objects_readable(p, vec![h]).await?.first() == Some(&true) { Ok(h) } else { Err(not_found()) }
}

fn record_json(h: &Hash, r: &Record) -> Value {
    json!({
        "record": h.to_hex(), "op": r.op, "inputs": r.inputs.iter().map(|(k, v)| (k.clone(), v.to_hex())).collect::<BTreeMap<_, _>>(),
        "outputs": r.outputs.iter().map(|(k, v)| (k.clone(), v.to_hex())).collect::<BTreeMap<_, _>>(),
        "meta": r.meta, "params": r.params.map(|p| p.to_hex()),
    })
}

#[derive(serde::Deserialize)]
pub struct RefQuery {
    #[serde(rename = "ref")]
    r: String,
}

/// `GET /v1/provenance?ref=<id>`: producers, consumers, transitive lineage and impact.
pub async fn get_provenance(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    let h = resolve_ref(&st, &p, &c, &q.r).await?;
    let repo = st.repo.clone();
    let v = blocking(move || {
        let prov = Provenance::load(&*repo, &log_ref())?;
        let producers: Vec<Value> = prov.producers(&h)?.iter().map(|(rh, r)| record_json(rh, r)).collect();
        let consumers: Vec<Value> = prov.consumers(&h)?.iter().map(|(rh, r)| record_json(rh, r)).collect();
        let lineage: Vec<String> = prov.lineage(&h)?.iter().map(Hash::to_hex).collect();
        let impact: Vec<String> = prov.impact(&h)?.iter().map(Hash::to_hex).collect();
        Ok(json!({ "object": h.to_hex(), "producers": producers, "consumers": consumers, "lineage": lineage, "impact": impact }))
    })
    .await?;
    Ok(Json(v).into_response())
}

#[derive(serde::Deserialize)]
pub struct NewRecord {
    op: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    outputs: BTreeMap<String, String>,
    #[serde(default)]
    meta: BTreeMap<String, String>,
}

/// `POST /v1/provenance`: a client-declared record ("this wheel was built from that commit"),
/// with external ids. Recording a claim about outputs requires write access to each of them.
pub async fn post_provenance(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Json(req): Json<NewRecord>,
) -> ApiResult<Response> {
    if req.outputs.is_empty() {
        return Err(ApiError::bad_request("a record needs at least one output"));
    }
    let mut inputs = BTreeMap::new();
    for (role, r) in &req.inputs {
        inputs.insert(role.clone(), resolve_ref(&st, &p, &c, r).await?);
    }
    let mut outputs = BTreeMap::new();
    for (role, r) in &req.outputs {
        match namespace_of(r) {
            Some(ns) => st.require(st.authz.allows(&p, &ns, Access::Write), &p, &c, "provenance", &ns)?,
            None => st.require(st.authz.allows(&p, "", Access::Write), &p, &c, "provenance", "objects")?,
        }
        outputs.insert(role.clone(), resolve_ref(&st, &p, &c, r).await?);
    }
    let mut meta = req.meta;
    meta.insert("principal".into(), p.name.clone());
    for (role, r) in req.inputs.iter().chain(&req.outputs) {
        meta.entry(format!("ref.{role}")).or_insert_with(|| r.clone());
    }
    st.require_audit_healthy()?;
    let rec = Record { op: req.op.clone(), inputs, outputs, meta, ..Default::default() };
    let repo = st.repo.clone();
    let hashes = blocking(move || pico2peta_prov::record(&*repo, &log_ref(), &[rec])).await?;
    let rh = hashes.first().copied().ok_or_else(|| ApiError::internal("no record hash"))?;
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "provenance",
        target: rh.to_hex(),
        outcome: "ok".into(),
        detail: Some(req.op),
        remote: c.remote.clone(),
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "record": rh.to_hex() }))).into_response())
}

/// A search hit: the object, its names, and its metadata `(key, value, op)`.
type SearchRow = (Hash, Vec<String>, Vec<(String, String, String)>);

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    prefix: Option<String>,
    exact: Option<String>,
}

/// `GET /v1/search?q=<text>|prefix=<name prefix>|exact=<name>`: names, front metadata and
/// provenance. Results are limited to names the caller may read.
pub async fn search(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Response> {
    st.require(st.authz.allows_somewhere(&p, Access::Read), &p, &c, "search", "")?;
    let idx = st.search.clone();
    let repo = st.repo.clone();
    let (exact, prefix, text) = (q.exact.clone(), q.prefix.clone(), q.q.clone());
    let res = blocking(move || {
        let mut g = idx.lock().map_err(|_| Error::Corrupt("search index lock poisoned".into()))?;
        match g.as_mut() {
            Some(i) => i.refresh(&*repo, &log_ref())?,
            None => *g = Some(pico2peta_prov::search::SearchIndex::build(&*repo, &log_ref())?),
        }
        let i = g.as_ref().ok_or_else(|| Error::Corrupt("no search index".into()))?;
        let mut hits: Vec<(Hash, Vec<String>)> = Vec::new();
        if let Some(e) = exact {
            if let Some(h) = i.exact(&e) {
                hits.push((h, vec![e]));
            }
        } else if let Some(pf) = prefix {
            for (n, h) in i.prefix(&pf) {
                hits.push((h, vec![n]));
            }
        } else if let Some(t) = text {
            for h in i.text(&t) {
                hits.push((h.hash, h.names));
            }
        }
        let out: Vec<SearchRow> =
            hits.into_iter().take(200).map(|(h, names)| (h, names, i.metadata(&h).to_vec())).collect();
        Ok(out)
    })
    .await?;
    let mut out = Vec::new();
    for (h, names, meta) in res {
        let names: Vec<String> = names.into_iter().filter(|n| st.authz.allows(&p, n, Access::Read)).collect();
        // Objects reached only through metadata need some name the caller can read.
        if names.is_empty() {
            continue;
        }
        let meta: Vec<Value> = meta.into_iter().map(|(k, v, op)| json!({ "key": k, "value": v, "op": op })).collect();
        out.push(json!({ "object": h.to_hex(), "names": names, "meta": meta }));
    }
    Ok(Json(json!({ "results": out })).into_response())
}

#[derive(serde::Deserialize)]
pub struct AuditQuery {
    principal: Option<String>,
    action: Option<String>,
    /// Targets starting with this (e.g. `git/team`, `pypi/`, an object hash).
    target: Option<String>,
    outcome: Option<String>,
    /// RFC 3339 lower bound (string comparison on the fixed-width UTC timestamps).
    since: Option<String>,
    limit: Option<usize>,
}

/// `GET /v1/admin/audit?...`: one query over the audit log of every front (admin).
pub async fn audit_query(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Response> {
    st.require(st.authz.allows(&p, "", Access::Admin), &p, &c, "audit_query", "")?;
    let path = st.audit.path().to_path_buf();
    let limit = q.limit.unwrap_or(1000).min(100_000);
    let rows = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<crate::audit::AuditRecord>> {
        let mut files: Vec<std::path::PathBuf> = crate::audit::rotated_files(&path)?.into_iter().map(|x| x.1).collect();
        files.push(path);
        let mut out = std::collections::VecDeque::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            for line in text.lines() {
                let Ok(r) = serde_json::from_str::<crate::audit::AuditRecord>(line) else { continue };
                let keep = q.principal.as_ref().is_none_or(|x| *x == r.principal)
                    && q.action.as_ref().is_none_or(|x| *x == r.action)
                    && q.outcome.as_ref().is_none_or(|x| *x == r.outcome)
                    && q.target.as_ref().is_none_or(|x| r.target.starts_with(x.as_str()))
                    && q.since.as_ref().is_none_or(|x| r.ts.as_str() >= x.as_str());
                if keep {
                    out.push_back(r);
                    if out.len() > limit {
                        out.pop_front(); // keep the newest `limit`
                    }
                }
            }
        }
        Ok(out.into_iter().collect())
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(format!("reading the audit log: {e}")))?;
    Ok(Json(json!({ "records": rows })).into_response())
}

// ---------------------------------------------------------------------------
// Attestations and promotion
// ---------------------------------------------------------------------------

/// Verifiers for every public key named by a promotion rule.
fn verifiers(st: &AppState) -> Vec<pico2peta_attest::Ed25519Verifier> {
    let mut out = Vec::new();
    for r in &st.promotion {
        for k in &r.required_keys {
            if let Some(v) = parse_key(k)
                && !out.iter().any(|x: &pico2peta_attest::Ed25519Verifier| x.public() == v.public())
            {
                out.push(v);
            }
        }
    }
    out
}

fn parse_key(hex: &str) -> Option<pico2peta_attest::Ed25519Verifier> {
    pico2peta_attest::Ed25519Verifier::from_public(&crate::hexs::decode32(hex)?).ok()
}

/// `POST /v1/attestations`: stores a DSSE envelope (in-toto Statement) and links it to its
/// subjects. Anyone with write access somewhere may attest; promotion only counts signatures
/// by keys its rules trust.
pub async fn post_attestation(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    st.require(st.authz.allows_somewhere(&p, Access::Write), &p, &c, "attest", "")?;
    st.require_audit_healthy()?;
    if body.len() > pico2peta_attest::MAX_ENVELOPE {
        return Err(ApiError::bad_request("envelope too large"));
    }
    let env = pico2peta_attest::Envelope::from_json(&body)
        .map_err(|e| ApiError::bad_request(format!("bad envelope: {e}")))?;
    let stmt = env.statement().map_err(|e| ApiError::bad_request(format!("bad statement: {e}")))?;
    let repo = st.repo.clone();
    let e2 = env.clone();
    let eh = blocking(move || pico2peta_attest::attest(&*repo, &log_ref(), &e2)).await?;
    // (Computed in a scope of its own: `&dyn Verifier` isn't `Send` across the await below.)
    let trusted = {
        let vs = verifiers(&st);
        let dyn_vs: Vec<&dyn pico2peta_attest::Verifier> =
            vs.iter().map(|v| v as &dyn pico2peta_attest::Verifier).collect();
        env.verified_by(&dyn_vs)
    };
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "attest",
        target: eh.to_hex(),
        outcome: "ok".into(),
        detail: Some(format!(
            "predicate={} subjects={} trusted_keys={}",
            stmt.predicate_type,
            stmt.subjects.len(),
            trusted.join(",")
        )),
        remote: c.remote.clone(),
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "envelope": eh.to_hex(), "trusted_keys": trusted }))).into_response())
}

/// `GET /v1/attestations?ref=<id>`: the envelopes about an object, with the trusted keys that
/// verify each one.
pub async fn get_attestations(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    let h = resolve_ref(&st, &p, &c, &q.r).await?;
    let repo = st.repo.clone();
    let envs = blocking(move || pico2peta_attest::attestations(&*repo, &log_ref(), &h)).await?;
    let vs = verifiers(&st);
    let dyn_vs: Vec<&dyn pico2peta_attest::Verifier> =
        vs.iter().map(|v| v as &dyn pico2peta_attest::Verifier).collect();
    let out: Vec<Value> = envs
        .iter()
        .map(|e| {
            let raw = e.to_json();
            let env: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            // `hash` is the stored envelope object (what POST returned as `envelope`).
            json!({ "hash": Hash::of(&raw).to_hex(), "envelope": env, "trusted_keys": e.verified_by(&dyn_vs) })
        })
        .collect();
    Ok(Json(json!({ "object": h.to_hex(), "attestations": out })).into_response())
}

#[derive(serde::Deserialize)]
pub struct PromoteRequest {
    src: String,
    dst: String,
}

/// `POST /v1/promote {src, dst}`: points name `dst` at what `src` names, if the
/// `[[promotion]]` rule for `dst` holds (required signatures, provenance). One OCC
/// transaction; recorded in the provenance log and the audit log.
pub async fn promote(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Extension(c): Extension<Caller>,
    Json(req): Json<PromoteRequest>,
) -> ApiResult<Response> {
    let (src, dst) = (Name::new(&req.src).map_err(ApiError::from)?, Name::new(&req.dst).map_err(ApiError::from)?);
    st.require(st.authz.allows(&p, src.as_str(), Access::Read), &p, &c, "promote", src.as_str())?;
    st.require(st.authz.allows(&p, dst.as_str(), Access::Write), &p, &c, "promote", dst.as_str())?;
    st.require_audit_healthy()?;
    let Some(rule) = st
        .promotion
        .iter()
        .filter(|r| pico2peta_core::is_under(dst.as_str(), &r.dst))
        .max_by_key(|r| r.dst.len())
        .cloned()
    else {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", format!("no [[promotion]] rule covers {dst}")));
    };
    let mut required = Vec::new();
    for k in &rule.required_keys {
        let v = parse_key(k).ok_or_else(|| ApiError::internal(format!("bad promotion key {k}")))?;
        required.push(pico2peta_attest::Verifier::keyid(&v));
    }
    let policy = pico2peta_attest::Policy {
        required_keys: required,
        predicate_type: rule.predicate_type.clone(),
        require_provenance: rule.require_provenance,
    };
    let vs = verifiers(&st);
    // Names owned by a front (e.g. oci/...) move through that front's handle.
    let repo = st.repo_for([src.as_str(), dst.as_str()]);
    let (s2, d2, actor) = (src.clone(), dst.clone(), p.name.clone());
    let out = blocking(move || {
        let dyn_vs: Vec<&dyn pico2peta_attest::Verifier> =
            vs.iter().map(|v| v as &dyn pico2peta_attest::Verifier).collect();
        pico2peta_attest::promote(&*repo, &log_ref(), &s2, &d2, &policy, &dyn_vs, &actor)
    })
    .await?;
    let (status, outcome, body) = match &out {
        pico2peta_attest::PromoteOutcome::Promoted { object, record } => {
            (StatusCode::OK, "ok", json!({ "promoted": object.to_hex(), "record": record.to_hex() }))
        }
        pico2peta_attest::PromoteOutcome::Refused(reasons) => {
            (StatusCode::FORBIDDEN, "refused", json!({ "refused": reasons }))
        }
        pico2peta_attest::PromoteOutcome::Conflict => {
            (StatusCode::CONFLICT, "conflict", json!({ "error": "src or dst moved concurrently" }))
        }
    };
    st.audit_mutation(Event {
        request_id: c.request_id.clone(),
        principal: p.name.clone(),
        action: "promote",
        target: format!("{src} -> {dst}"),
        outcome: outcome.into(),
        detail: Some(body.to_string()),
        remote: c.remote.clone(),
    })
    .await?;
    Ok((status, Json(body)).into_response())
}
