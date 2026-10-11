//! Local Git objects and detached workspaces. No network commands or remote
//! push. Export refuses unresolved conflicts and records immutable patch hashes
//! in the commit message. Cloudflare Artifacts stores the same Git projection.
use o5s_platform::{Refusal, Repository, valid_name};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
fn git(path: &Path, args: &[&str], input: &[u8]) -> Result<String, Refusal> {
    let mut child = Command::new("git")
        .arg("--git-dir")
        .arg(path)
        .args(args)
        .env("GIT_AUTHOR_NAME", "o5s")
        .env("GIT_AUTHOR_EMAIL", "o5s@localhost")
        .env("GIT_COMMITTER_NAME", "o5s")
        .env("GIT_COMMITTER_EMAIL", "o5s@localhost")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Refusal(e.to_string()))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .map_err(|e| Refusal(e.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|e| Refusal(e.to_string()))?;
    if !output.status.success() {
        return Err(Refusal(String::from_utf8_lossy(&output.stderr).into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
#[derive(Default)]
struct Tree {
    files: BTreeMap<String, String>,
    dirs: BTreeMap<String, Tree>,
}
impl Tree {
    fn add(&mut self, path: &str, blob: String) {
        match path.split_once('/') {
            Some((head, tail)) => self.dirs.entry(head.into()).or_default().add(tail, blob),
            None => {
                self.files.insert(path.into(), blob);
            }
        }
    }
    fn write(&self, path: &Path) -> Result<String, Refusal> {
        let mut entries = BTreeMap::new();
        for (name, blob) in &self.files {
            entries.insert(name.clone(), format!("100644 blob {blob}\t{name}\0"));
        }
        for (name, dir) in &self.dirs {
            entries.insert(
                format!("{name}/"),
                format!("040000 tree {}\t{name}\0", dir.write(path)?),
            );
        }
        git(
            path,
            &["mktree", "-z"],
            entries.values().cloned().collect::<String>().as_bytes(),
        )
    }
}
pub fn export(
    root: &Path,
    tenant: u64,
    name: &str,
    channel: &str,
    repo: &Repository,
) -> Result<serde_json::Value, Refusal> {
    if !valid_name(name) || !valid_name(channel) {
        return Err(Refusal("invalid name".into()));
    }
    let view = repo.materialize(channel)?;
    if !view.conflicts.is_empty() {
        return Err(Refusal("resolve conflicts before Git export".into()));
    }
    let path = root
        .join("artifacts")
        .join(tenant.to_string())
        .join(format!("{name}.git"));
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| Refusal(e.to_string()))?;
    if !path.exists() {
        let status = Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&path)
            .output()
            .map_err(|e| Refusal(e.to_string()))?;
        if !status.status.success() {
            return Err(Refusal("git init failed".into()));
        }
    }
    let mut tree = Tree::default();
    for (name, content) in &view.files {
        tree.add(
            name,
            git(&path, &["hash-object", "-w", "--stdin"], content.as_bytes())?,
        );
    }
    let tree = tree.write(&path)?;
    let reference = format!("refs/heads/{channel}");
    let parent = git(&path, &["rev-parse", "--verify", &reference], &[]).ok();
    if let Some(p) = &parent {
        if git(&path, &["rev-parse", &format!("{p}^{{tree}}")], &[])? == tree {
            // Still write a commit when the patch set changed (same file bytes
            // can have a different intent, review or dependency identity).
            let old = git(&path, &["show", "-s", "--format=%B", p], &[])?;
            let hashes: Vec<&str> = view
                .patches
                .iter()
                .map(|id| repo.patches[id].hash.as_str())
                .collect();
            let expected = format!(
                "o5s verified-admission projection\n\nPatch hashes:\n{}",
                hashes.join("\n")
            );
            if old == expected.trim_end() {
                return Ok(
                    serde_json::json!({"path":path,"commit":p,"tree":tree,"unchanged":true}),
                );
            }
        }
    }
    let mut message = String::from("o5s verified-admission projection\n\nPatch hashes:\n");
    for id in &view.patches {
        message.push_str(&repo.patches[id].hash);
        message.push('\n');
    }
    let args = if let Some(p) = &parent {
        vec!["commit-tree", &tree, "-p", p]
    } else {
        vec!["commit-tree", &tree]
    };
    let commit = git(&path, &args, message.as_bytes())?;
    git(&path, &["update-ref", &reference, &commit], &[])?;
    Ok(serde_json::json!({"path":path,"commit":commit,"tree":tree,"ref":reference}))
}

/// Detached local workspaces are snapshots of admitted patch sets. Repeated
/// checkout of the same snapshot preserves any agent's existing local edits.
pub fn worktree(
    root: &Path,
    tenant: u64,
    name: &str,
    channel: &str,
    repo: &Repository,
) -> Result<serde_json::Value, Refusal> {
    let exported = export(root, tenant, name, channel, repo)?;
    let commit = exported["commit"]
        .as_str()
        .ok_or_else(|| Refusal("missing Git projection".into()))?;
    let bare = std::fs::canonicalize(exported["path"].as_str().unwrap())
        .map_err(|e| Refusal(e.to_string()))?;
    let root = std::fs::canonicalize(root).map_err(|e| Refusal(e.to_string()))?;
    let path = root
        .join("workspaces")
        .join(tenant.to_string())
        .join(name)
        .join(channel)
        .join(commit);
    let reused = path.exists();
    if !reused {
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| Refusal(e.to_string()))?;
        git(
            &bare,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str()
                    .ok_or_else(|| Refusal("invalid workspace path".into()))?,
                commit,
            ],
            &[],
        )?;
    }
    let inspect = |args: &[&str]| -> Result<String, Refusal> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(args)
            .output()
            .map_err(|e| Refusal(e.to_string()))?;
        if !output.status.success() {
            return Err(Refusal("workspace inspection failed".into()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    if inspect(&["rev-parse", "HEAD"])? != commit {
        return Err(Refusal(
            "existing workspace HEAD changed; refusing to overwrite".into(),
        ));
    }
    let common = inspect(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if std::fs::canonicalize(&common).map_err(|e| Refusal(e.to_string()))? != bare {
        return Err(Refusal(
            "workspace belongs to a different repository".into(),
        ));
    }
    let dirty = !inspect(&["status", "--porcelain"])?.is_empty();
    Ok(serde_json::json!({"path":path,"commit":commit,"reused":reused,"dirty":dirty}))
}
