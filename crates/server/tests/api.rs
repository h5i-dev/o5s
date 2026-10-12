use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use o5s_server::{AppState, Backend, Store, auth::HmacAuth, router};
use std::process::Command;
use tower::ServiceExt;
fn state(root: &std::path::Path) -> AppState {
    AppState::new(
        Backend::local(Store::open(root).unwrap()),
        HmacAuth::new("test"),
        root.into(),
    )
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
    let before = std::fs::read(dir.path().join("registry.log")).unwrap();
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
        std::fs::read(dir.path().join("registry.log")).unwrap()
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
    let before = std::fs::read(dir.path().join("registry.log")).unwrap();
    let (status,_) = request(&s,1,1,"/api/command",Some(serde_json::json!({"cmd":"merge_batch","repo":"cache","channel":"main","patches":[p,999]}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        std::fs::read(dir.path().join("registry.log")).unwrap(),
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

#[tokio::test]
async fn concurrent_line_proposals_survive_restart_and_export_merged_git_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"lines"})).await;
    command(&s,1,serde_json::json!({"cmd":"propose","repo":"lines","intent":"base","dependencies":[],"edits":[{"path":"src/a.txt","content":"一\n二\n三\n"}]})).await;
    let left = command(
        &s,
        1,
        serde_json::json!({"cmd":"propose_lines","repo":"lines","intent":"left","dependencies":[],"edits":[{"path":"src/a.txt","base":1,"start":0,"end":1,"content":"ONE\n"}]}),
    );
    let right = command(
        &s,
        2,
        serde_json::json!({"cmd":"propose_lines","repo":"lines","intent":"right","dependencies":[],"edits":[{"path":"src/a.txt","base":1,"start":2,"end":3,"content":"THREE\n"}]}),
    );
    let (a, b) = tokio::join!(left, right);
    let ids = vec![
        1,
        a["patch"].as_u64().unwrap(),
        b["patch"].as_u64().unwrap(),
    ];
    for id in &ids {
        command(
            &s,
            10,
            serde_json::json!({"cmd":"approve","repo":"lines","patch":id}),
        )
        .await;
        command(
            &s,
            11,
            serde_json::json!({"cmd":"check","repo":"lines","patch":id,"passed":true}),
        )
        .await;
    }
    command(
        &s,
        10,
        serde_json::json!({"cmd":"merge_batch","repo":"lines","channel":"main","patches":ids}),
    )
    .await;
    let (_, v) = request(&s, 1, 1, "/api/view?repo=lines", None).await;
    assert_eq!(v["files"]["src/a.txt"], "ONE\n二\nTHREE\n");
    drop(s);
    // JoinHandle completion can precede the blocking worker dropping its captured
    // Arc. Wait for that final owner before simulating a process restart.
    let reopened = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match Store::open(dir.path()) {
                Ok(store) => {
                    break AppState::new(
                        Backend::local(store),
                        HmacAuth::new("test"),
                        dir.path().into(),
                    );
                }
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                Err(e) => panic!("restart failed: {e}"),
            }
        }
    };
    let (_, v) = request(&reopened, 1, 1, "/api/state?repo=lines", None).await;
    assert_eq!(v["view"]["files"]["src/a.txt"], "ONE\n二\nTHREE\n");
    let (status, export) = request(
        &reopened,
        1,
        1,
        "/api/export",
        Some(serde_json::json!({"repo":"lines"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    let git = Command::new("git")
        .args([
            "--git-dir",
            export["path"].as_str().unwrap(),
            "show",
            "main:src/a.txt",
        ])
        .output()
        .unwrap();
    assert!(git.status.success());
    assert_eq!(git.stdout, "ONE\n二\nTHREE\n".as_bytes());
}

#[tokio::test]
async fn detached_workspace_reuse_preserves_edits_and_is_tenant_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(
        &s,
        1,
        serde_json::json!({"cmd":"create","repo":"workspace"}),
    )
    .await;
    command(&s,1,serde_json::json!({"cmd":"propose","repo":"workspace","intent":"base","dependencies":[],"edits":[{"path":"a.txt","content":"base\n"}]})).await;
    command(
        &s,
        2,
        serde_json::json!({"cmd":"approve","repo":"workspace","patch":1}),
    )
    .await;
    command(
        &s,
        3,
        serde_json::json!({"cmd":"check","repo":"workspace","patch":1,"passed":true}),
    )
    .await;
    command(
        &s,
        1,
        serde_json::json!({"cmd":"merge","repo":"workspace","channel":"main","patch":1}),
    )
    .await;
    let (status, v) = request(
        &s,
        1,
        1,
        "/api/worktree",
        Some(serde_json::json!({"repo":"workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reused"], false);
    assert_eq!(v["dirty"], false);
    let path = std::path::Path::new(v["path"].as_str().unwrap());
    let branch = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    assert!(!branch.status.success());
    assert_eq!(
        std::fs::read_to_string(path.join("a.txt")).unwrap(),
        "base\n"
    );
    std::fs::write(path.join("a.txt"), "agent changes\n").unwrap();
    let (status, next) = request(
        &s,
        1,
        1,
        "/api/worktree",
        Some(serde_json::json!({"repo":"workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["reused"], true);
    assert_eq!(next["dirty"], true);
    assert_eq!(
        std::fs::read_to_string(path.join("a.txt")).unwrap(),
        "agent changes\n"
    );
    let (status, other) = request(
        &s,
        2,
        1,
        "/api/worktree",
        Some(serde_json::json!({"repo":"workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other}");
    assert_ne!(v["path"], other["path"]);
    assert_eq!(v["commit"], other["commit"]);
    assert_eq!(other["dirty"], false);
    assert_eq!(
        std::fs::read_to_string(
            std::path::Path::new(other["path"].as_str().unwrap()).join("a.txt")
        )
        .unwrap(),
        "base\n"
    );
    let (status, _) = request(
        &s,
        1,
        2,
        "/api/worktree",
        Some(serde_json::json!({"repo":"workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn workspace_import_records_snapshot_dependencies_without_altering_files() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"import"})).await;
    command(&s,1,serde_json::json!({"cmd":"propose","repo":"import","intent":"base","dependencies":[],"edits":[{"path":"a.txt","content":"base\n"},{"path":"b.txt","content":"delete me\n"}]})).await;
    for (user, body) in [
        (
            2,
            serde_json::json!({"cmd":"approve","repo":"import","patch":1}),
        ),
        (
            3,
            serde_json::json!({"cmd":"check","repo":"import","patch":1,"passed":true}),
        ),
        (
            1,
            serde_json::json!({"cmd":"merge","repo":"import","channel":"main","patch":1}),
        ),
    ] {
        command(&s, user, body).await;
    }
    let (status, workspace) = request(
        &s,
        1,
        1,
        "/api/worktree",
        Some(serde_json::json!({"repo":"import"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{workspace}");
    let path = std::path::Path::new(workspace["path"].as_str().unwrap());
    std::fs::write(path.join("a.txt"), "agent edit\n").unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(path.join("a.txt"), "agent edit 二\n").unwrap();
    std::fs::remove_file(path.join("b.txt")).unwrap();
    std::fs::create_dir(path.join("src")).unwrap();
    std::fs::write(path.join("src/新.txt"), "new file\n").unwrap();
    let body = serde_json::json!({"repo":"import","commit":workspace["commit"],"intent":"Import agent workspace"});
    let (status, proposed) = request(&s, 1, 1, "/api/worktree/propose", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{proposed}");
    let id = proposed["patch"].as_u64().unwrap().to_string();
    let (_, snapshot) = request(&s, 1, 1, "/api/state?repo=import", None).await;
    let patch = &snapshot["repository"]["patches"][&id];
    assert_eq!(patch["dependencies"], serde_json::json!([1]));
    assert_eq!(patch["author"], 1);
    assert_eq!(
        patch["edits"],
        serde_json::json!([
            {"path":"a.txt","content":"agent edit 二\n"},
            {"path":"b.txt","content":null},
            {"path":"src/新.txt","content":"new file\n"},
        ])
    );
    assert_eq!(
        std::fs::read_to_string(path.join("a.txt")).unwrap(),
        "agent edit 二\n"
    );
    let staged = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["show", ":a.txt"])
        .output()
        .unwrap();
    assert_eq!(staged.stdout, b"agent edit\n");
    let (status, repeated) = request(&s, 1, 1, "/api/worktree/propose", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{repeated}");
    assert_eq!(proposed["patch"], repeated["patch"]);
    assert_eq!(repeated["duplicate"], true);
    assert_eq!(
        request(&s, 2, 1, "/api/worktree/propose", Some(body.clone()))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(&s, 1, 2, "/api/worktree/propose", Some(body.clone()))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // A symlink is proposed as a link: its target, never the bytes behind it.
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "must not import\n").unwrap();
    std::os::unix::fs::symlink(&outside, path.join("link.txt")).unwrap();
    let (status, linked) = request(&s, 1, 1, "/api/worktree/propose", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    let (_, detail) = request(
        &s,
        1,
        1,
        &format!("/api/patch?repo=import&id={}", linked["patch"]),
        None,
    )
    .await;
    let edits = detail["patch"]["edits"].as_array().unwrap();
    let link = edits.iter().find(|e| e["path"] == "link.txt").unwrap();
    assert_eq!(link["kind"], "symlink");
    assert_eq!(link["content"], outside.to_str().unwrap());
    assert!(!detail.to_string().contains("must not import"));
    std::fs::remove_file(path.join("link.txt")).unwrap();
    let before = std::fs::read(dir.path().join("registry.log")).unwrap();
    std::fs::remove_file(path.join("a.txt")).unwrap();
    std::fs::create_dir(path.join("a.txt")).unwrap();
    let (status, refusal) = request(&s, 1, 1, "/api/worktree/propose", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    assert!(refusal["error"].as_str().unwrap().contains("regular file"));
    assert_eq!(
        before,
        std::fs::read(dir.path().join("registry.log")).unwrap()
    );
}

#[tokio::test]
async fn history_pages_bound_payloads_and_enforce_tenant() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"pages"})).await;
    for id in 1..=3 {
        command(&s, id, serde_json::json!({"cmd":"propose","repo":"pages","intent":format!("change {id}"),"dependencies":[],"edits":[{"path":format!("{id}.txt"),"content":"private historical payload"}]})).await;
    }
    let (status, first) = request(&s, 1, 1, "/api/patches?repo=pages&limit=2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["patches"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_after"], 2);
    assert!(!first.to_string().contains("private historical payload"));
    let (_, last) = request(&s, 1, 1, "/api/patches?repo=pages&limit=2&after=2", None).await;
    assert_eq!(last["patches"].as_array().unwrap().len(), 1);
    assert_eq!(last["patches"][0]["id"], 3);
    assert!(last["next_after"].is_null());
    let (status, detail) = request(&s, 1, 1, "/api/patch?repo=pages&id=2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        detail["patch"]["edits"][0]["content"],
        "private historical payload"
    );
    for url in ["/api/patches?repo=pages", "/api/patch?repo=pages&id=2"] {
        let (status, _) = request(&s, 1, 2, url, None).await;
        assert_ne!(status, StatusCode::OK);
    }
    for url in [
        "/api/patches?repo=pages&limit=0",
        "/api/patches?repo=pages&limit=101",
        "/api/patch?repo=pages&id=99",
    ] {
        assert_ne!(request(&s, 1, 1, url, None).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn document_snapshot_preserves_observed_graph_when_channel_advances() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"snap"})).await;
    for (user, bytes) in [(1, vec![65u8]), (2, vec![66u8])] {
        command(&s, user, serde_json::json!({"cmd":"propose_graph","repo":"snap","intent":"insert","dependencies":[],"edits":[{"path":"a.txt","operations":[{"kind":"insert","left":"Start","right":"End","atoms":[bytes]}]}]})).await;
        command(
            &s,
            3,
            serde_json::json!({"cmd":"approve","repo":"snap","patch":user}),
        )
        .await;
        command(
            &s,
            4,
            serde_json::json!({"cmd":"check","repo":"snap","patch":user,"passed":true}),
        )
        .await;
        command(
            &s,
            3,
            serde_json::json!({"cmd":"merge","repo":"snap","channel":"main","patch":user}),
        )
        .await;
    }
    let (status, old) = request(
        &s,
        1,
        1,
        "/api/document?repo=snap&path=a.txt&patches=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert_eq!(old["bytes"], serde_json::json!([65]));
    assert_eq!(old["graph_backed"], true);
    let (_, current) = request(&s, 1, 1, "/api/document?repo=snap&path=a.txt", None).await;
    assert!(current["bytes"].is_null());
    assert!(!current["conflicts"].as_array().unwrap().is_empty());
    let (_, empty) = request(
        &s,
        1,
        1,
        "/api/document?repo=snap&path=a.txt&patches=",
        None,
    )
    .await;
    assert_eq!(empty["graph_backed"], false);
    assert_eq!(empty["bytes"], serde_json::json!([]));
    for selected in ["999", "invalid", "-1"] {
        let url = format!("/api/document?repo=snap&path=a.txt&patches={selected}");
        assert_ne!(request(&s, 1, 1, &url, None).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn request_ids_and_metrics_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    let app = router(s.clone());
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("x-request-id", "trace-7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.headers()["x-request-id"], "trace-7");
    request(&s, 1, 1, "/api/repositories", None).await;
    let r = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let text =
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(
        text.contains(r#"o5s_http_requests_total{method="GET",route="/healthz",status="200"} 1"#),
        "{text}"
    );
    assert!(
        text.contains(r#"route="/api/repositories",status="200""#),
        "{text}"
    );
    assert!(text.contains("o5s_local_log_bytes"));
}

async fn with_token(
    s: &AppState,
    token: &str,
    url: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
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

#[tokio::test]
async fn roles_restrict_a_repository_to_the_users_granted() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    command(&s, 1, serde_json::json!({"cmd":"create","repo":"private"})).await;
    let grant = |user: u64, role: &str| serde_json::json!({"cmd":"grant","repo":"private","user":user,"role":role});
    // User 1 restricts the repository and lets user 2 write.
    let r = command(&s, 1, grant(2, "write")).await;
    assert_eq!(r, serde_json::json!({"user":2,"role":"write"}));
    let propose = serde_json::json!({"cmd":"propose","repo":"private","intent":"x","dependencies":[],"edits":[{"path":"a","content":"a\n"}]});
    let p = command(&s, 2, propose.clone()).await["patch"]
        .as_u64()
        .unwrap();
    // User 3 does not see it: not in the list, not readable, cannot propose.
    let (_, list) = request(&s, 3, 1, "/api/repositories", None).await;
    assert!(
        list["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "private")
    );
    let (status, _) = request(&s, 3, 1, "/api/state?repo=private", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, v) = request(&s, 3, 1, "/api/command", Some(propose)).await;
    assert_eq!(v["error"], "repository not found", "{status}");
    // A writer cannot approve or grant.
    let (_, v) = request(
        &s,
        2,
        1,
        "/api/command",
        Some(serde_json::json!({"cmd":"approve","repo":"private","patch":p})),
    )
    .await;
    assert_eq!(v["error"], "your role does not allow this");
    let (_, v) = request(&s, 2, 1, "/api/command", Some(grant(3, "read"))).await;
    assert_eq!(v["error"], "your role does not allow this");
    let (_, v) = request(&s, 1, 1, "/api/command", Some(grant(1, "read"))).await;
    assert_eq!(v["error"], "users cannot change their own role");
    let (_, saved) = request(&s, 1, 1, "/api/state?repo=private", None).await;
    assert_eq!(
        saved["repository"]["roles"],
        serde_json::json!({"1":4,"2":2})
    );
    // The roles survive a restart.
    drop(s);
    let s = state(dir.path());
    let (status, _) = request(&s, 3, 1, "/api/state?repo=private", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        request(&s, 2, 1, "/api/state?repo=private", None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn revoked_tokens_are_refused_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let s = state(dir.path());
    let one = s.auth.issue(1, 5, 1_000_000_000_000);
    let two = s.auth.issue(1, 5, 2_000_000_000_000);
    assert_eq!(
        with_token(&s, &one, "/api/repositories", None).await.0,
        StatusCode::OK
    );
    // Revoking one token leaves the other.
    let (status, v) = with_token(&s, &one, "/api/tokens/revoke", Some(serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = with_token(&s, &one, "/api/repositories", None).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("token revoked"))
    );
    assert_eq!(
        with_token(&s, &two, "/api/repositories", None).await.0,
        StatusCode::OK
    );
    // `all` revokes every token expiring no later than the one presented.
    let three = s.auth.issue(1, 5, 3_000_000_000_000);
    with_token(
        &s,
        &three,
        "/api/tokens/revoke",
        Some(serde_json::json!({"all":true})),
    )
    .await;
    for t in [&two, &three] {
        assert_eq!(
            with_token(&s, t, "/api/repositories", None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // Another user's tokens are untouched; the file survives restarts.
    let other = s.auth.issue(1, 6, 3_000_000_000_000);
    assert_eq!(
        with_token(&s, &other, "/api/repositories", None).await.0,
        StatusCode::OK
    );
    drop(s);
    let s = state(dir.path());
    assert_eq!(
        with_token(&s, &two, "/api/repositories", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}
