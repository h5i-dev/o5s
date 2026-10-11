use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use o5s_server::{AppState, Store, auth::HmacAuth, router};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
fn state(root: &std::path::Path) -> AppState {
    AppState {
        store: Arc::new(Mutex::new(Store::open(root).unwrap())),
        auth: Arc::new(HmacAuth::new("test")),
        root: root.into(),
    }
}
async fn request(
    s: &AppState,
    user: u64,
    tenant: u64,
    url: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let token = s.auth.issue(tenant, user, u64::MAX);
    let r = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.map_or(String::new(), |v| v.to_string())))
        .unwrap();
    let response = router(s.clone()).oneshot(r).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn command(s: &AppState, user: u64, body: serde_json::Value) -> serde_json::Value {
    let (status, v) = request(s, user, 1, "/api/command", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}
#[tokio::test]
async fn authenticated_parallel_requests_restart_and_git_projection() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"demo"})).await;
    let mut tasks = Vec::new();
    for user in 1..=32 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
        command(&s,user,serde_json::json!({"cmd":"propose","repo":"demo","intent":format!("agent {user}"),"dependencies":[],"edits":[{"path":format!("src/agent-{user}.txt"),"content":format!("agent {user}\n")}]})).await["patch"].as_u64().unwrap()
    }));
    }
    for task in tasks {
        let id = task.await.unwrap();
        command(
            &s,
            100,
            serde_json::json!({"cmd":"approve","repo":"demo","patch":id}),
        )
        .await;
        command(
            &s,
            101,
            serde_json::json!({"cmd":"check","repo":"demo","patch":id,"passed":true}),
        )
        .await;
        command(
            &s,
            100,
            serde_json::json!({"cmd":"merge","repo":"demo","channel":"main","patch":id}),
        )
        .await;
    }
    let (status, v) = request(
        &s,
        1,
        1,
        "/api/export",
        Some(serde_json::json!({"repo":"demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let path = v["path"].as_str().unwrap();
    let git = Command::new("git")
        .arg("--git-dir")
        .arg(path)
        .args(["show", "main:src/agent-32.txt"])
        .output()
        .unwrap();
    assert!(git.status.success());
    assert_eq!(git.stdout, b"agent 32\n");
    let (_, same) = request(
        &s,
        1,
        1,
        "/api/export",
        Some(serde_json::json!({"repo":"demo"})),
    )
    .await;
    assert_eq!(v["commit"], same["commit"]);
    assert_eq!(
        request(&s, 1, 2, "/api/state?repo=demo", None).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let saved = request(&s, 1, 1, "/api/state?repo=demo", None).await.1;
    assert!(Store::open(dir.path()).is_err());
    drop(s);
    let s = state(dir.path());
    assert_eq!(
        saved,
        request(&s, 1, 1, "/api/state?repo=demo", None).await.1
    );
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let (status, _) = request(
        &s,
        1,
        1,
        "/api/command",
        Some(serde_json::json!({"cmd":"merge","repo":"demo","channel":"main","patch":999})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        before,
        std::fs::read(dir.path().join("state.json")).unwrap()
    );
}
#[tokio::test]
async fn authentication_and_body_identity_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    let response = router(s.clone())
        .oneshot(
            Request::builder()
                .uri("/api/repositories")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (status, _) = request(
        &s,
        1,
        1,
        "/api/command",
        Some(serde_json::json!({"cmd":"create","repo":"x","tenant":2})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compact_view_cache_and_batch_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"cache"})).await;
    let p = command(&s,1,serde_json::json!({"cmd":"propose","repo":"cache","intent":"base","dependencies":[],"edits":[{"path":"a","content":"base"}]})).await["patch"].as_u64().unwrap();
    command(
        &s,
        2,
        serde_json::json!({"cmd":"approve","repo":"cache","patch":p}),
    )
    .await;
    command(
        &s,
        3,
        serde_json::json!({"cmd":"check","repo":"cache","patch":p,"passed":true}),
    )
    .await;
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let (status,_) = request(&s,1,1,"/api/command",Some(serde_json::json!({"cmd":"merge_batch","repo":"cache","channel":"main","patches":[p,999]}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    command(
        &s,
        1,
        serde_json::json!({"cmd":"merge_batch","repo":"cache","channel":"main","patches":[p]}),
    )
    .await;
    let (status, first) = request(&s, 1, 1, "/api/view?repo=cache", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["work"]["processed_patches"], 1);
    let (_, next) = request(&s, 1, 1, "/api/view?repo=cache", None).await;
    assert_eq!(next["work"]["processed_patches"], 0);
    assert_eq!(next["files"]["a"], "base");
    assert_eq!(next["patch_count"], 1);
    assert!(next.get("repository").is_none());
    assert!(next.get("patches").is_none());
    assert_eq!(
        request(&s, 1, 2, "/api/view?repo=cache", None).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
