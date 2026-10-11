use o5s_server::{AppState, Store, auth::HmacAuth, router};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

struct Fixture {
    root: tempfile::TempDir,
    url: String,
    auth: Arc<HmacAuth>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let auth = Arc::new(HmacAuth::new("cli-workflow-test"));
        let state = AppState {
            store: Arc::new(Mutex::new(Store::open(&root.path().join("store")).unwrap())),
            auth: auth.clone(),
            root: root.path().join("store"),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        Self {
            root,
            url,
            auth,
            server,
        }
    }
    fn repo(&self, name: &str) -> std::path::PathBuf {
        let path = self.root.path().join(name);
        fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q"]);
        path
    }
    fn command(&self, cwd: &Path, user: u64, tenant: u64, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_o5s"))
            .current_dir(cwd)
            .env("O5S_URL", &self.url)
            .env("O5S_TOKEN", self.auth.issue(tenant, user, u64::MAX))
            .env("O5S_CONFIG", self.root.path().join("config.json"))
            .env_remove("O5S_PROFILE")
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, cwd: &Path, user: u64, args: &[&str]) -> Value {
        let o = self.command(cwd, user, 1, args);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn fail(&self, cwd: &Path, user: u64, args: &[&str], message: &str) {
        let o = self.command(cwd, user, 1, args);
        assert!(!o.status.success());
        assert!(
            String::from_utf8_lossy(&o.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    fn admit(&self, cwd: &Path, patch: u64) {
        let id = patch.to_string();
        self.ok(cwd, 90, &["approve", &id]);
        self.ok(cwd, 91, &["check", &id, "--pass"]);
        self.ok(cwd, 90, &["merge", &id]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn git(path: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@localhost")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@localhost")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}
fn patch(value: Value) -> u64 {
    value["patch"].as_u64().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_git_worktrees_independent_review_and_parallel_line_merge() {
    let f = Fixture::new().await;
    let a = f.repo("author");
    fs::write(a.join("file.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    fs::create_dir_all(a.join("nested")).unwrap();
    fs::write(a.join("nested/staged.txt"), "staged\n").unwrap();
    git(&a, &["add", "nested/staged.txt"]);
    let before_index = git(&a, &["ls-files", "--stage"]);
    f.ok(&a, 1, &["connect", "demo"]);
    let initial = patch(f.ok(&a, 1, &["propose", "--intent", "initial import"]));
    f.fail(
        &a,
        1,
        &["approve", &initial.to_string()],
        "independent reviewer",
    );
    f.fail(&a, 90, &["merge", &initial.to_string()], "unapproved");
    f.admit(&a, initial);
    let fetched = f.ok(&a, 1, &["fetch"]);
    assert_eq!(git(&a, &["ls-files", "--stage"]), before_index);
    assert_eq!(
        fs::read_to_string(a.join("file.txt")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&a)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(f.ok(&a, 1, &["fetch"])["unchanged"], true);
    git(
        &a,
        &["reset", "--hard", fetched["commit"].as_str().unwrap()],
    );
    let b = f.root.path().join("native-worktree");
    git(
        &a,
        &[
            "worktree",
            "add",
            "--detach",
            b.to_str().unwrap(),
            "refs/o5s/main",
        ],
    );
    fs::write(a.join("file.txt"), "ONE\ntwo\nthree\nfour\n").unwrap();
    fs::write(b.join("file.txt"), "one\ntwo\nthree\nFOUR\n").unwrap();
    let pa = patch(f.ok(&a, 2, &["propose", "--intent", "first line"]));
    let pb = patch(f.ok(&b, 3, &["propose", "--intent", "last line"]));
    assert_eq!(
        f.ok(&a, 2, &["show", &pa.to_string()])["line_edits"][0]["start"],
        0
    );
    assert_eq!(
        f.ok(&b, 3, &["show", &pb.to_string()])["line_edits"][0]["start"],
        3
    );
    f.admit(&a, pa);
    f.admit(&b, pb);
    let view = f.ok(&a, 2, &["view"]);
    assert_eq!(view["files"]["file.txt"], "ONE\ntwo\nthree\nFOUR\n");
    let head = git(&a, &["rev-parse", "HEAD"]);
    let index = git(&a, &["ls-files", "--stage"]);
    f.ok(&a, 2, &["fetch"]);
    assert_eq!(git(&a, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&a, &["ls-files", "--stage"]), index);
    assert_eq!(
        fs::read_to_string(a.join("file.txt")).unwrap(),
        "ONE\ntwo\nthree\nfour\n"
    );
    assert_eq!(
        git(&a, &["show", "refs/o5s/main:file.txt"]),
        "ONE\ntwo\nthree\nFOUR"
    );
    let tenant = f.command(&a, 2, 2, &["status"]);
    assert!(!tenant.status.success());
    assert!(String::from_utf8_lossy(&tenant.stderr).contains("tenant differs"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicting_edits_never_overwrite_git_ref_or_checkout() {
    let f = Fixture::new().await;
    let a = f.repo("conflict");
    f.ok(&a, 1, &["connect", "conflict"]);
    fs::write(a.join("f"), "base\n").unwrap();
    let base = patch(f.ok(&a, 1, &["propose", "--intent", "base"]));
    f.admit(&a, base);
    let fetched = f.ok(&a, 1, &["fetch"]);
    git(
        &a,
        &["reset", "--hard", fetched["commit"].as_str().unwrap()],
    );
    fs::write(a.join("f"), "left\n").unwrap();
    let left = patch(f.ok(&a, 2, &["propose", "--intent", "left"]));
    fs::write(a.join("f"), "right\n").unwrap();
    let right = patch(f.ok(&a, 3, &["propose", "--intent", "right"]));
    f.admit(&a, left);
    f.admit(&a, right);
    assert!(!f.ok(&a, 1, &["view"])["conflicts"]["f"].is_null());
    let reference = git(&a, &["rev-parse", "refs/o5s/main"]);
    f.fail(&a, 1, &["fetch"], "conflicts");
    assert_eq!(git(&a, &["rev-parse", "refs/o5s/main"]), reference);
    assert_eq!(fs::read_to_string(a.join("f")).unwrap(), "right\n");
    let resolution = f.root.path().join("resolution.txt");
    fs::write(&resolution, "reviewed resolution\n").unwrap();
    let resolved = patch(f.ok(
        &a,
        4,
        &[
            "resolve",
            "f",
            "--content-file",
            resolution.to_str().unwrap(),
            "--intent",
            "resolve both alternatives",
        ],
    ));
    f.admit(&a, resolved);
    f.ok(&a, 1, &["fetch"]);
    assert_eq!(git(&a, &["show", "refs/o5s/main:f"]), "reviewed resolution");
    assert_eq!(fs::read_to_string(a.join("f")).unwrap(), "right\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_credentials_stay_at_the_authenticated_origin() {
    let f = Fixture::new().await;
    let a = f.repo("credentials");
    let token_file = f.root.path().join("token");
    let token = f.auth.issue(1, 1, u64::MAX);
    fs::write(&token_file, &token).unwrap();
    let reply = f.ok(
        &a,
        1,
        &["login", "--token-file", token_file.to_str().unwrap()],
    );
    assert!(!reply.to_string().contains(&token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(f.root.path().join("config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let o = Command::new(env!("CARGO_BIN_EXE_o5s"))
        .current_dir(&a)
        .env_remove("O5S_TOKEN")
        .env("O5S_URL", "http://127.0.0.1:1")
        .env("O5S_CONFIG", f.root.path().join("config.json"))
        .args(["connect", "wrong"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("different origin"));
    fs::write(&token_file, "invalid").unwrap();
    f.fail(
        &a,
        1,
        &["login", "--token-file", token_file.to_str().unwrap()],
        "401",
    );
    assert!(
        fs::read_to_string(f.root.path().join("config.json"))
            .unwrap()
            .contains(&token)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_unstaged_untracked_and_deleted_files_are_proposed_without_index_edits() {
    let f = Fixture::new().await;
    let a = f.repo("changes");
    f.ok(&a, 1, &["connect", "changes"]);
    fs::write(a.join("tracked"), "original\n").unwrap();
    fs::write(a.join("deleted"), "remove\n").unwrap();
    fs::write(a.join(".gitignore"), "ignored\n").unwrap();
    let initial = patch(f.ok(&a, 1, &["propose", "--intent", "import"]));
    f.admit(&a, initial);
    let commit = f.ok(&a, 1, &["fetch"])["commit"]
        .as_str()
        .unwrap()
        .to_owned();
    git(&a, &["reset", "--hard", &commit]);
    fs::write(a.join("tracked"), "staged\n").unwrap();
    git(&a, &["add", "tracked"]);
    fs::write(a.join("tracked"), "unstaged\n").unwrap();
    fs::write(a.join("new"), "untracked\n").unwrap();
    fs::write(a.join("ignored"), "must not publish\n").unwrap();
    fs::remove_file(a.join("deleted")).unwrap();
    let before = git(&a, &["ls-files", "--stage"]);
    let proposed = patch(f.ok(&a, 2, &["propose", "--intent", "working state"]));
    assert_eq!(git(&a, &["ls-files", "--stage"]), before);
    f.admit(&a, proposed);
    let view = f.ok(&a, 2, &["view"]);
    assert_eq!(view["files"]["tracked"], "unstaged\n");
    assert_eq!(view["files"]["new"], "untracked\n");
    assert!(view["files"].get("deleted").is_none());
    assert!(view["files"].get("ignored").is_none());
    f.fail(
        &a,
        2,
        &["--url", "http://127.0.0.1:1", "status"],
        "sending request",
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_git_content_is_rejected_before_a_proposal_exists() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new().await;
    let a = f.repo("unsupported");
    f.ok(&a, 1, &["connect", "unsupported"]);
    fs::write(a.join("binary"), [0xff, 0xfe]).unwrap();
    f.fail(&a, 1, &["propose", "--intent", "binary"], "UTF-8");
    fs::remove_file(a.join("binary")).unwrap();
    symlink("/etc/passwd", a.join("link")).unwrap();
    f.fail(
        &a,
        1,
        &["propose", "--intent", "symlink"],
        "Symlink unsupported",
    );
    fs::remove_file(a.join("link")).unwrap();
    fs::write(a.join("executable"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(a.join("executable"), fs::Permissions::from_mode(0o755)).unwrap();
    f.fail(
        &a,
        1,
        &["propose", "--intent", "mode"],
        "Executable mode unsupported",
    );
    assert!(f.ok(&a, 1, &["log"]).as_object().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_operations_use_portable_nodes_and_reviewed_order_resolution() {
    use serde_json::json;
    let f = Fixture::new().await;
    let a = f.repo("graph");
    f.ok(&a, 1, &["connect", "graph"]);
    let operations = f.root.path().join("operations.json");
    fs::write(
        &operations,
        json!([{"path":"graph.txt","operations":[{
            "kind":"insert","left":"Start","right":"End","atoms":[b"base\n".to_vec()]
        }]}])
        .to_string(),
    )
    .unwrap();
    let proposal = f.ok(
        &a,
        1,
        &[
            "propose-graph",
            "--operations-file",
            operations.to_str().unwrap(),
            "--intent",
            "base graph",
        ],
    );
    let base = patch(proposal.clone());
    f.admit(&a, base);
    let document = f.ok(&a, 1, &["document", "graph.txt"]);
    assert_eq!(document["bytes"], json!(b"base\n".to_vec()));
    let anchor = document["nodes"][0]["id"].clone();
    let hash_bytes: Vec<u8> = proposal["hash"]
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(anchor["insertion"]["patch"], json!(hash_bytes));
    let mut children = Vec::new();
    for (user, text) in [(2, "left\n"), (3, "right\n")] {
        fs::write(&operations, json!([{"path":"graph.txt","operations":[{
            "kind":"insert","left":"Start","right":{"Node":anchor},"atoms":[text.as_bytes().to_vec()]
        }]}]).to_string()).unwrap();
        let child = patch(f.ok(
            &a,
            user,
            &[
                "propose-graph",
                "--operations-file",
                operations.to_str().unwrap(),
                "--intent",
                text,
                "--dependencies",
                &base.to_string(),
            ],
        ));
        f.admit(&a, child);
        children.push(child);
    }
    let document = f.ok(&a, 1, &["document", "graph.txt"]);
    assert!(document["bytes"].is_null());
    assert!(!document["conflicts"].as_array().unwrap().is_empty());
    f.fail(&a, 1, &["fetch"], "conflicts");
    let nodes = document["nodes"].as_array().unwrap();
    let node_for = |text: &[u8]| {
        nodes
            .iter()
            .find(|node| node["bytes"] == json!(text))
            .unwrap()["id"]
            .clone()
    };
    fs::write(
        &operations,
        json!([{"path":"graph.txt","operations":[{
            "kind":"order","before":node_for(b"left\n"),"after":node_for(b"right\n")
        }]}])
        .to_string(),
    )
    .unwrap();
    let deps = format!("{base},{},{}", children[0], children[1]);
    let resolution = patch(f.ok(
        &a,
        4,
        &[
            "propose-graph",
            "--operations-file",
            operations.to_str().unwrap(),
            "--intent",
            "reviewed ordering",
            "--dependencies",
            &deps,
        ],
    ));
    f.admit(&a, resolution);
    let document = f.ok(&a, 1, &["document", "graph.txt"]);
    assert!(document["conflicts"].as_array().unwrap().is_empty());
    assert_eq!(document["bytes"], json!(b"left\nright\nbase\n".to_vec()));
    f.ok(&a, 1, &["fetch"]);
    assert_eq!(
        git(&a, &["show", "refs/o5s/main:graph.txt"]),
        "left\nright\nbase"
    );
    assert!(!a.join("graph.txt").exists());
    git(&a, &["reset", "--hard", "refs/o5s/main"]);
    fs::write(a.join("graph.txt"), "unsupported whole-file replacement\n").unwrap();
    f.fail(
        &a,
        1,
        &["propose", "--intent", "invalid mixed model"],
        "graph document requires graph operations",
    );
}
