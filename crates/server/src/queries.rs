use crate::{AppState, actor, result};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::Response,
};
use o5s_platform::Refusal;
use serde::Deserialize;
use std::ops::Bound::{Excluded, Unbounded};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PatchPage {
    repo: String,
    #[serde(default)]
    after: u64,
    #[serde(default = "page_size")]
    limit: usize,
}
fn page_size() -> usize {
    50
}

/// Payload-free history pages: work and response size depend on page length,
/// rather than copying every historical file snapshot from the registry.
pub(super) async fn patches(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PatchPage>,
) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if !(1..=100).contains(&q.limit) {
        return result(Err(Refusal("limit must be between 1 and 100".into())));
    }
    let store = s.store.lock().unwrap();
    result((|| {
        let repo = store.db.repo(&a, &q.repo)?;
        let mut registry = repo.patches.range((Excluded(q.after), Unbounded));
        let page: Vec<_> = registry
            .by_ref()
            .take(q.limit)
            .map(|(id, p)| {
                serde_json::json!({
                    "id":id,"hash":p.hash,"author":p.author,"intent":p.intent,
                    "dependency_count":p.dependencies.len(),"edit_count":p.edits.len(),
                    "line_edit_count":p.line_edits.len(),"graph_edit_count":p.graph_edits.len(),
                    "approval_count":p.approvals.len(),"checks_passed":p.checks_passed,
                })
            })
            .collect();
        let next = if registry.next().is_some() {
            page.last().map(|p| p["id"].clone())
        } else {
            None
        };
        Ok(serde_json::json!({"patches":page,"next_after":next}))
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PatchQuery {
    repo: String,
    id: u64,
}
pub(super) async fn patch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PatchQuery>,
) -> Response {
    let a = match actor(&s, &headers) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let store = s.store.lock().unwrap();
    result((|| {
        let repo = store.db.repo(&a, &q.repo)?;
        let patch = repo
            .patches
            .get(&q.id)
            .ok_or_else(|| Refusal("patch not found".into()))?;
        Ok(serde_json::json!({"patch":patch}))
    })())
}
