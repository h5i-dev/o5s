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
    agent: u64,
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
        .join(agent.to_string())
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
    Ok(serde_json::json!({"path":path,"commit":commit,"agent":agent,"reused":reused,"dirty":dirty}))
}

/// Read changes from this authenticated agent's detached snapshot. The caller
/// persists the resulting immutable proposal through the ordinary transaction.
/// Git files, index and working tree remain untouched.
pub fn workspace_proposal(
    root: &Path,
    tenant: u64,
    agent: u64,
    name: &str,
    channel: &str,
    commit: &str,
    intent: &str,
    repo: &Repository,
) -> Result<o5s_platform::Command, Refusal> {
    use o5s_platform::{Command as Proposal, Edit, valid_path};
    use std::collections::BTreeSet;
    let refusal = |message: &str| Refusal(message.into());
    if !valid_name(name)
        || !valid_name(channel)
        || commit.len() != 40
        || !commit.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(refusal("invalid workspace identity"));
    }
    let membership = repo
        .channels
        .get(channel)
        .ok_or_else(|| refusal("channel not found"))?;
    let root = std::fs::canonicalize(root).map_err(|e| Refusal(e.to_string()))?;
    let bare = std::fs::canonicalize(
        root.join("artifacts")
            .join(tenant.to_string())
            .join(format!("{name}.git")),
    )
    .map_err(|_| refusal("Git projection not found"))?;
    let path = root
        .join("workspaces")
        .join(tenant.to_string())
        .join(name)
        .join(channel)
        .join(agent.to_string())
        .join(commit);
    let run = |args: &[&str]| -> Result<Vec<u8>, Refusal> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(args)
            .output()
            .map_err(|e| Refusal(e.to_string()))?;
        if !output.status.success() {
            return Err(refusal("workspace inspection failed"));
        }
        Ok(output.stdout)
    };
    let text = |bytes: Vec<u8>| -> Result<String, Refusal> {
        String::from_utf8(bytes).map_err(|_| refusal("workspace requires UTF-8 text"))
    };
    if text(run(&["rev-parse", "HEAD"])?)?.trim() != commit {
        return Err(refusal(
            "workspace HEAD changed; select its original snapshot",
        ));
    }
    let common = text(run(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?)?;
    if std::fs::canonicalize(common.trim()).map_err(|e| Refusal(e.to_string()))? != bare {
        return Err(refusal("workspace belongs to a different repository"));
    }
    let message = text(run(&["show", "-s", "--format=%B", "HEAD"])?)?;
    let hashes = message
        .strip_prefix("o5s verified-admission projection\n\nPatch hashes:\n")
        .ok_or_else(|| refusal("workspace is not an o5s projection"))?;
    let mut dependencies = Vec::new();
    for hash in hashes.lines().filter(|line| !line.is_empty()) {
        let id = repo
            .patches
            .values()
            .find(|p| p.hash == hash)
            .map(|p| p.id)
            .ok_or_else(|| refusal("workspace patch identity not found"))?;
        if !membership.contains(&id) {
            return Err(refusal("workspace snapshot is outside this channel"));
        }
        dependencies.push(id);
    }
    let mut changed = run(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-only",
        "-z",
        "HEAD",
        "--",
    ])?;
    changed.extend(run(&["ls-files", "--others", "--exclude-standard", "-z"])?);
    let paths: BTreeSet<_> = changed
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| text(p.to_vec()))
        .collect::<Result<_, _>>()?;
    if paths.is_empty() || paths.len() > 1024 {
        return Err(refusal("1..1024 changed files required"));
    }
    let mut edits = Vec::new();
    let mut total = 0usize;
    for file in paths {
        if !valid_path(&file) {
            return Err(refusal("invalid workspace file path"));
        }
        let mut target = path.clone();
        let mut absent = false;
        for component in file.split('/') {
            target.push(component);
            match std::fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(refusal("symlinks are not text patch files"));
                }
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    absent = true;
                    break;
                }
                Err(e) => return Err(Refusal(e.to_string())),
            }
        }
        let content = if absent {
            None
        } else {
            let metadata = std::fs::metadata(&target).map_err(|e| Refusal(e.to_string()))?;
            if !metadata.is_file() {
                return Err(refusal("workspace patch content must be a regular file"));
            }
            let size = metadata.len();
            if size > 4 * 1024 * 1024 {
                return Err(refusal("workspace file exceeds proposal limit"));
            }
            let bytes = std::fs::read(&target).map_err(|e| Refusal(e.to_string()))?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| refusal("proposal too large"))?;
            if total > 4 * 1024 * 1024 {
                return Err(refusal("workspace proposal exceeds limit"));
            }
            Some(text(bytes)?)
        };
        edits.push(Edit {
            path: file,
            content,
        });
    }
    Ok(Proposal::Propose {
        repo: name.into(),
        intent: intent.into(),
        dependencies,
        edits,
    })
}
