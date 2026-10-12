//! Local Git objects and detached workspaces. No network commands or remote
//! push. Export refuses unresolved conflicts and records immutable patch hashes
//! in the commit message. Cloudflare Artifacts stores the same Git projection.
use o5s_platform::{Kind, Mode, Refusal, Repository, valid_name, workspace};
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
    /// Object id (a commit for a submodule) and mode.
    files: BTreeMap<String, (String, Mode)>,
    dirs: BTreeMap<String, Tree>,
}
impl Tree {
    fn add(&mut self, path: &str, blob: String, mode: Mode) {
        match path.split_once('/') {
            Some((head, tail)) => self
                .dirs
                .entry(head.into())
                .or_default()
                .add(tail, blob, mode),
            None => {
                self.files.insert(path.into(), (blob, mode));
            }
        }
    }
    fn write(&self, path: &Path) -> Result<String, Refusal> {
        let mut entries = BTreeMap::new();
        for (name, (id, mode)) in &self.files {
            let (mode, kind) = match mode {
                Mode::Regular => ("100644", "blob"),
                Mode::Executable => ("100755", "blob"),
                Mode::Symlink => ("120000", "blob"),
                Mode::Gitlink => ("160000", "commit"),
            };
            entries.insert(name.clone(), format!("{mode} {kind} {id}\t{name}\0"));
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
fn write_mark(mark: &Path, generation: u64) -> Result<(), Refusal> {
    let staged = mark.with_extension("tmp");
    std::fs::write(&staged, generation.to_string()).map_err(|e| Refusal(e.to_string()))?;
    std::fs::rename(&staged, mark).map_err(|e| Refusal(e.to_string()))
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
    // One exporter per bare repository at a time, across processes; a view
    // older than the last exported generation never moves the ref backwards.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join("o5s-export.lock"))
        .map_err(|e| Refusal(e.to_string()))?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|e| Refusal(e.to_string()))?;
    let marks = path.join("o5s-generations");
    std::fs::create_dir_all(&marks).map_err(|e| Refusal(e.to_string()))?;
    let mark = marks.join(channel);
    let exported: u64 = std::fs::read_to_string(&mark)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if repo.generation < exported {
        let reference = format!("refs/heads/{channel}");
        let commit = git(&path, &["rev-parse", "--verify", &reference], &[])?;
        return Ok(
            serde_json::json!({"path":path,"commit":commit,"ref":reference,
            "stale":true,"generation":exported}),
        );
    }
    let mut tree = Tree::default();
    for name in view.files.keys() {
        let (bytes, mode) = view
            .entry(name)
            .ok_or_else(|| Refusal("corrupt binary file or submodule id".into()))?;
        let id = if mode == Mode::Gitlink {
            view.files[name].clone()
        } else {
            git(&path, &["hash-object", "-w", "--stdin"], &bytes)?
        };
        tree.add(name, id, mode);
    }
    let tree = tree.write(&path)?;
    let reference = format!("refs/heads/{channel}");
    let parent = git(&path, &["rev-parse", "--verify", &reference], &[]).ok();
    // The causal heads stand for the whole admitted set.
    let hashes = workspace::snapshot(repo, &view.patches)?;
    if let Some(p) = &parent {
        // Unchanged when the files and the admitted patch set are the same
        // (equal bytes can still come from a different patch set).
        if git(&path, &["rev-parse", &format!("{p}^{{tree}}")], &[])? == tree {
            let old = git(&path, &["show", "-s", "--format=%B", p], &[])?;
            if workspace::parse(&old).is_some_and(|(_, h)| h == hashes) {
                write_mark(&mark, repo.generation)?;
                return Ok(
                    serde_json::json!({"path":path,"commit":p,"tree":tree,"unchanged":true,"generation":repo.generation}),
                );
            }
        }
    }
    let message = workspace::message(Some(repo.generation), &hashes);
    let args = if let Some(p) = &parent {
        vec!["commit-tree", &tree, "-p", p]
    } else {
        vec!["commit-tree", &tree]
    };
    let commit = git(&path, &args, message.as_bytes())?;
    // Compare-and-swap: fails if another writer moved the ref meanwhile.
    let old = parent.clone().unwrap_or_else(|| "0".repeat(40));
    git(&path, &["update-ref", &reference, &commit, &old], &[])?;
    write_mark(&mark, repo.generation)?;
    Ok(
        serde_json::json!({"path":path,"commit":commit,"tree":tree,"ref":reference,"generation":repo.generation}),
    )
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
    use o5s_platform::valid_path;
    use std::collections::BTreeSet;
    let refusal = |message: &str| Refusal(message.into());
    if !valid_name(name)
        || !valid_name(channel)
        || commit.len() != 40
        || !commit.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(refusal("invalid workspace identity"));
    }
    if !repo.channels.contains_key(channel) {
        return Err(refusal("channel not found"));
    }
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
    if workspace::parse(&message).is_none() {
        return Err(refusal("workspace is not an o5s projection"));
    }
    // Submodules are gitlinks in the index: mode 160000 and a commit id.
    let mut submodules = BTreeMap::new();
    for entry in run(&["ls-files", "-s", "-z"])?.split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some((meta, name)) = entry.split_once('\t')
            && let [mode, commit, _] = meta.split(' ').collect::<Vec<_>>()[..]
            && mode == "160000"
        {
            submodules.insert(name.to_string(), commit.to_string());
        }
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
    if paths.is_empty() || paths.len() > workspace::MAX_FILES {
        return Err(Refusal(format!(
            "1..{} changed files required",
            workspace::MAX_FILES
        )));
    }
    let mut edits = Vec::new();
    let mut total = 0usize;
    for file in paths {
        if !valid_path(&file) {
            return Err(refusal("invalid workspace file path"));
        }
        if let Some(commit) = submodules.get(&file) {
            edits.push(workspace::edit(
                file,
                Some(commit.clone().into_bytes()),
                false,
                Kind::Submodule,
            )?);
            continue;
        }
        let mut target = path.clone();
        let mut absent = false;
        let last = file.split('/').count() - 1;
        let mut link = None;
        for (k, component) in file.split('/').enumerate() {
            target.push(component);
            match std::fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    if k != last {
                        return Err(refusal("a path below a symlink is not a file"));
                    }
                    let to = std::fs::read_link(&target).map_err(|e| Refusal(e.to_string()))?;
                    link = Some(
                        to.into_os_string()
                            .into_string()
                            .map_err(|_| refusal("symlink target is not UTF-8"))?,
                    );
                }
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    absent = true;
                    break;
                }
                Err(e) => return Err(Refusal(e.to_string())),
            }
        }
        if let Some(to) = link {
            edits.push(workspace::edit(
                file,
                Some(to.into_bytes()),
                false,
                Kind::Symlink,
            )?);
            continue;
        }
        let mut executable = false;
        let content = if absent {
            None
        } else {
            let metadata = std::fs::metadata(&target).map_err(|e| Refusal(e.to_string()))?;
            if !metadata.is_file() {
                return Err(refusal("workspace patch content must be a regular file"));
            }
            if metadata.len() > workspace::MAX_BYTES as u64 {
                return Err(refusal("workspace file exceeds proposal limit"));
            }
            let bytes = std::fs::read(&target).map_err(|e| Refusal(e.to_string()))?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| refusal("proposal too large"))?;
            if total > workspace::MAX_BYTES {
                return Err(refusal("workspace proposal exceeds limit"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                executable = metadata.permissions().mode() & 0o111 != 0;
            }
            Some(bytes)
        };
        edits.push(workspace::edit(file, content, executable, Kind::File)?);
    }
    workspace::proposal(name, channel, intent, &message, repo, edits)
}
