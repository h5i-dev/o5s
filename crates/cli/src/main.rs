//! Git remains the working-copy interface. o5s coordinates immutable proposals.
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(
    name = "o5s",
    version,
    about = "Patch collaboration for ordinary Git repositories and worktrees"
)]
struct Cli {
    #[arg(long, global = true, env = "O5S_URL")]
    url: Option<String>,
    #[arg(long, global = true, env = "O5S_PROFILE", default_value = "default")]
    profile: String,
    #[arg(long, global = true)]
    compact: bool,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Validate and save a bearer token. Read stdin by default; never prints the token.
    Login {
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    /// Remove this profile's saved token.
    Logout,
    /// Attach this existing Git repository to an o5s collaboration repository.
    Connect { repository: String },
    /// Show local changes, remote conflicts and the snapshot this worktree builds on.
    Status {
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Record this Git worktree's changes as an immutable proposal; leaves Git untouched.
    Propose {
        #[arg(short, long)]
        intent: String,
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Record an explicit half-open line range against an immutable patch.
    ProposeLines {
        #[arg(long)]
        base: u64,
        #[arg(long)]
        path: String,
        #[arg(long)]
        start: u64,
        #[arg(long)]
        end: u64,
        #[arg(long)]
        content_file: PathBuf,
        #[arg(short, long)]
        intent: String,
        #[arg(long, value_delimiter = ',')]
        depends: Vec<u64>,
    },
    /// Propose graph operations with stable node identities from a JSON edits array.
    ProposeGraph {
        #[arg(long)]
        operations_file: PathBuf,
        #[arg(short, long)]
        intent: String,
        #[arg(long, value_delimiter = ',')]
        dependencies: Vec<u64>,
    },
    /// Inspect the materialized graph's node identities, layers and conflicts.
    Document {
        path: String,
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Propose an explicit whole-file resolution of the current channel alternatives.
    Resolve {
        path: String,
        #[arg(long, required_unless_present = "delete", conflicts_with = "delete")]
        content_file: Option<PathBuf>,
        #[arg(long)]
        delete: bool,
        #[arg(short, long)]
        intent: String,
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Read a proposal and its review/check state.
    Show { patch: u64 },
    /// Page proposal summaries across all repository channels.
    Log {
        /// Resume after a proposal ID returned as next_after.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Number of summaries per page (no file contents are returned).
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
    },
    /// Independently approve an immutable proposal.
    Approve { patch: u64 },
    /// Run a check command on the snapshot each proposal defines (the
    /// proposal and its causal past, in a fresh directory) and report the
    /// result. Run it under a check reporter's profile, never the author's.
    Ci {
        /// Shell command run in the snapshot directory; exit status 0 passes.
        #[arg(long)]
        command: String,
        /// Only this proposal; otherwise every proposal without a passing
        /// report that this runner has not run yet.
        #[arg(long)]
        patch: Option<u64>,
        /// Seconds before the command is killed and the check fails.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Report an independently executed check result (does not run CI).
    Check {
        patch: u64,
        #[arg(long, conflicts_with = "fail", required_unless_present = "fail")]
        pass: bool,
        #[arg(long, required_unless_present = "pass")]
        fail: bool,
    },
    /// Atomically admit reviewed, passing proposals to a channel.
    Merge {
        #[arg(required = true)]
        patches: Vec<u64>,
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Fetch a conflict-free snapshot into refs/o5s/CHANNEL; never changes HEAD or files.
    Fetch {
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Read materialized files and explicit conflicts.
    View {
        #[arg(long, default_value = "main")]
        channel: String,
    },
    /// Fork patch membership (Git branches/worktrees still use ordinary Git commands).
    Channel {
        name: String,
        #[arg(long, default_value = "main")]
        from: String,
    },
}
/// Patches this runner has run, kept with the connection metadata.
#[derive(Default, Serialize, Deserialize)]
struct CiLog {
    runs: BTreeMap<u64, bool>,
}

/// Write a snapshot reply (`GET /api/snapshot`) into `dir`.
fn write_snapshot(dir: &Path, snapshot: &Value) -> Result<()> {
    let files = snapshot["files"]
        .as_object()
        .ok_or_else(|| error("snapshot has no files"))?;
    if snapshot["conflicts"]
        .as_object()
        .is_some_and(|c| !c.is_empty())
    {
        return Err(error("the snapshot has conflicts"));
    }
    let set = |k: &str| -> BTreeSet<String> {
        snapshot[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (binary, executable) = (set("binary"), set("executable"));
    let (symlinks, submodules) = (set("symlinks"), set("submodules"));
    for path in files.keys() {
        if path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c == ".git")
        {
            return Err(error(format!("unsafe snapshot path {path}")));
        }
        // Nothing is written through a symlink.
        for (i, c) in path.char_indices() {
            if c == '/' && symlinks.contains(&path[..i]) {
                return Err(error(format!("{path} is below a symlink")));
            }
        }
    }
    // Regular files first, then submodules (empty directories), then links.
    for (path, content) in files {
        let text = content
            .as_str()
            .ok_or_else(|| error("invalid snapshot file"))?;
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        if submodules.contains(path) {
            fs::create_dir_all(&target)?;
            continue;
        }
        if symlinks.contains(path) {
            continue;
        }
        let bytes = if binary.contains(path) {
            base64_decode(text).ok_or_else(|| error("invalid base64 in snapshot"))?
        } else {
            text.as_bytes().to_vec()
        };
        fs::write(&target, bytes)?;
        #[cfg(unix)]
        if executable.contains(path) {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
        }
    }
    #[cfg(unix)]
    for path in &symlinks {
        let to = files[path]
            .as_str()
            .ok_or_else(|| error("invalid symlink"))?;
        std::os::unix::fs::symlink(to, dir.join(path))?;
    }
    Ok(())
}

/// Run `command` in `dir` with a timeout; whether it exited successfully.
fn run_command(command: &str, dir: &Path, timeout: u64) -> Result<bool> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("O5S_SNAPSHOT", dir)
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("o5s ci: timed out after {timeout} s");
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `o5s ci`: execute checks on patch snapshots and report them.
fn run_checks(
    api: &Api,
    git: &Git,
    repo: &str,
    command: &str,
    only: Option<u64>,
    timeout: u64,
) -> Result<Value> {
    let path = git.common.join("o5s/ci.json");
    let mut log: CiLog = match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(_) => CiLog::default(),
    };
    let candidates = match only {
        Some(p) => vec![p],
        None => {
            let mut out = Vec::new();
            let mut after = 0u64;
            loop {
                let page = api.request(
                    "/api/patches",
                    &[
                        ("repo", repo),
                        ("after", &after.to_string()),
                        ("limit", "100"),
                    ],
                    None,
                )?;
                for p in page["patches"].as_array().into_iter().flatten() {
                    let id = p["id"].as_u64().unwrap_or(0);
                    if p["checks_passed"] != json!(true) && !log.runs.contains_key(&id) {
                        out.push(id);
                    }
                }
                match page["next_after"].as_u64() {
                    Some(n) => after = n,
                    None => break,
                }
            }
            out
        }
    };
    let mut results = Vec::new();
    for patch in candidates {
        let snapshot = api.request(
            "/api/snapshot",
            &[("repo", repo), ("patch", &patch.to_string())],
            None,
        )?;
        let dir = std::env::temp_dir().join(format!(
            "o5s-ci-{}-{patch}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(&dir)?;
        let passed = match write_snapshot(&dir, &snapshot) {
            Ok(()) => run_command(command, &dir, timeout)?,
            Err(e) => {
                eprintln!("o5s ci: patch {patch}: {e}");
                false
            }
        };
        let _ = fs::remove_dir_all(&dir);
        api.command(
            repo,
            json!({"cmd": "check", "patch": patch, "passed": passed}),
        )?;
        log.runs.insert(patch, passed);
        save(&path, &log)?;
        results.push(json!({"patch": patch, "hash": snapshot["hash"], "snapshot": snapshot["hashes"], "passed": passed}));
    }
    Ok(json!({"checked": results}))
}

#[derive(Default, Serialize, Deserialize)]
struct Profiles {
    profiles: BTreeMap<String, Profile>,
}
#[derive(Serialize, Deserialize)]
struct Profile {
    url: String,
    token: String,
}
#[derive(Serialize, Deserialize)]
struct Context {
    repository: String,
    url: String,
    tenant: u64,
    snapshots: BTreeMap<String, Snapshot>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    patches: Vec<u64>,
    #[serde(default)]
    bases: BTreeMap<String, u64>,
}
struct Git {
    root: PathBuf,
    common: PathBuf,
}
struct Api {
    client: Client,
    url: String,
    token: String,
}

fn error(s: impl Into<String>) -> Box<dyn std::error::Error> {
    s.into().into()
}
fn origin(s: &str) -> Result<String> {
    let u = reqwest::Url::parse(s)?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err(error(
            "URL must be an HTTP(S) origin without credentials, query or path",
        ));
    }
    Ok(s.trim_end_matches('/').into())
}
impl Api {
    fn new(url: String, token: String) -> Result<Self> {
        if token.trim().is_empty() {
            return Err(error("Missing token: set O5S_TOKEN or run o5s login"));
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            url: origin(&url)?,
            token,
        })
    }
    fn request(&self, route: &str, query: &[(&str, &str)], body: Option<Value>) -> Result<Value> {
        if let Some(value) = &body {
            if serde_json::to_vec(value)?.len() > 4 * 1024 * 1024 {
                return Err(error("Command exceeds server 4 MiB request limit"));
            }
        }
        let url = format!("{}{}", self.url, route);
        let req = if body.is_some() {
            self.client.post(url)
        } else {
            self.client.get(url)
        };
        let mut req = req.bearer_auth(&self.token).query(query);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send()?;
        let status = response.status();
        let bytes = response.bytes()?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| error(format!("Server returned non-JSON ({status})")))?;
        if !status.is_success() {
            return Err(error(format!(
                "{status}: {}",
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("request refused")
            )));
        }
        Ok(value)
    }
    fn state(&self, repo: &str, channel: &str) -> Result<Value> {
        self.request("/api/state", &[("repo", repo), ("channel", channel)], None)
    }
    fn command(&self, repo: &str, mut value: Value) -> Result<Value> {
        value["repo"] = json!(repo);
        self.request("/api/command", &[], Some(value))
    }
}
fn config_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("O5S_CONFIG") {
        return Ok(p.into());
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Set O5S_CONFIG or XDG_CONFIG_HOME")?;
    Ok(base.join("o5s/config.json"))
}
fn profiles() -> Result<Profiles> {
    let path = config_path()?;
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Profiles::default()),
        Err(e) => Err(e.into()),
    }
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".o5s-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut f = opts.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
impl Git {
    fn discover() -> Result<Self> {
        let root = raw_git(None, &["rev-parse", "--show-toplevel"], None)?;
        let root = PathBuf::from(text(root)?);
        let common = text(raw_git(
            Some(&root),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            None,
        )?)?;
        Ok(Self {
            root,
            common: common.into(),
        })
    }
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        raw_git(Some(&self.root), args, input)
    }
    fn context_path(&self) -> PathBuf {
        self.common.join("o5s/context.json")
    }
    fn context(&self) -> Result<Context> {
        let bytes = fs::read(self.context_path()).map_err(|e| {
            error(format!(
                "No o5s connection for this Git repository ({e}); run o5s connect NAME"
            ))
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn baseline(
        &self,
        context: &Context,
        state: &Value,
        channel: &str,
    ) -> Result<(Option<String>, Snapshot)> {
        // Local commits after a fetched snapshot retain its causal patch base.
        if let Ok(history) = self.run(&["rev-list", "HEAD"], None) {
            for commit in text(history)?.lines() {
                if let Some(snapshot) = context.snapshots.get(commit) {
                    return Ok((Some(commit.into()), snapshot.clone()));
                }
            }
        }
        let members = state["repository"]["channels"][channel]
            .as_array()
            .ok_or("Unknown channel")?;
        if !members.is_empty() {
            return Err(error(
                "HEAD has no known o5s snapshot. Run o5s fetch, then use git switch or git worktree add with refs/o5s/CHANNEL; local files will not be overwritten.",
            ));
        }
        Ok((
            None,
            Snapshot {
                patches: vec![],
                bases: BTreeMap::new(),
            },
        ))
    }
    fn committed(&self, commit: Option<&str>) -> Result<Files> {
        let Some(commit) = commit else {
            return Ok(BTreeMap::new());
        };
        let listing = self.run(&["ls-tree", "-r", "-z", commit], None)?;
        let mut files = BTreeMap::new();
        for entry in listing.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let (meta, path) = entry.split_at(
                entry
                    .iter()
                    .position(|b| *b == b'\t')
                    .ok_or("Malformed Git tree")?,
            );
            let fields: Vec<&str> = std::str::from_utf8(meta)?.split_whitespace().collect();
            let name: String = std::str::from_utf8(&path[1..])?.into();
            let data = match fields[..] {
                ["100644" | "100755", "blob", id] => FileData::from_bytes(
                    self.run(&["cat-file", "blob", id], None)?,
                    fields[0] == "100755",
                ),
                ["120000", "blob", id] => FileData::special(
                    String::from_utf8(self.run(&["cat-file", "blob", id], None)?)
                        .map_err(|_| error(format!("Symlink target is not UTF-8: {name}")))?,
                    Kind::Symlink,
                ),
                ["160000", "commit", id] => FileData::special(id.into(), Kind::Submodule),
                _ => return Err(error(format!("Unsupported Git tree entry: {name}"))),
            };
            files.insert(name, data);
        }
        Ok(files)
    }
    fn working(&self) -> Result<Files> {
        let paths = self.run(
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            None,
        )?;
        // Submodules are gitlinks in the index; their staged commit is the content.
        let mut gitlinks = BTreeMap::new();
        for entry in self
            .run(&["ls-files", "-s", "-z"], None)?
            .split(|b| *b == 0)
        {
            let entry = String::from_utf8_lossy(entry);
            if let Some((meta, name)) = entry.split_once('\t')
                && let [mode, commit, _] = meta.split(' ').collect::<Vec<_>>()[..]
                && mode == "160000"
            {
                gitlinks.insert(name.to_string(), commit.to_string());
            }
        }
        let mut files = BTreeMap::new();
        for path in paths.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let relative = std::str::from_utf8(path)?;
            if !valid_path(relative) {
                return Err(error(format!("Unsupported Git path: {relative}")));
            }
            if let Some(commit) = gitlinks.get(relative) {
                files.insert(
                    relative.into(),
                    FileData::special(commit.clone(), Kind::Submodule),
                );
                continue;
            }
            let mut full = self.root.clone();
            let mut missing = false;
            let mut link = None;
            let last = Path::new(relative).components().count() - 1;
            for (k, component) in Path::new(relative).components().enumerate() {
                let std::path::Component::Normal(part) = component else {
                    return Err(error("Invalid Git path"));
                };
                full.push(part);
                match fs::symlink_metadata(&full) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        if k != last {
                            return Err(error(format!("Path below a symlink: {relative}")));
                        }
                        link = Some(
                            fs::read_link(&full)?
                                .into_os_string()
                                .into_string()
                                .map_err(|_| {
                                    error(format!("Symlink target is not UTF-8: {relative}"))
                                })?,
                        );
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        missing = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            if missing {
                continue;
            }
            if let Some(target) = link {
                files.insert(relative.into(), FileData::special(target, Kind::Symlink));
                continue;
            }
            if fs::metadata(&full)?.len() > 4 * 1024 * 1024 {
                return Err(error(format!("File exceeds local 4 MiB limit: {relative}")));
            }
            if !fs::metadata(&full)?.is_file() {
                return Err(error(format!("Non-regular file unsupported: {relative}")));
            }
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&full)?.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            files.insert(
                relative.into(),
                FileData::from_bytes(fs::read(full)?, executable),
            );
        }
        Ok(files)
    }
    fn fetch(&self, context: &mut Context, state: &Value, channel: &str) -> Result<Value> {
        if !state["view"]["conflicts"]
            .as_object()
            .ok_or("Missing conflict map")?
            .is_empty()
        {
            return Err(error(
                "Snapshot has conflicts; inspect o5s view and propose a resolution before fetching",
            ));
        }
        let texts: BTreeMap<String, String> =
            serde_json::from_value(state["view"]["files"].clone())?;
        let set = |key: &str| -> Result<BTreeSet<String>> {
            Ok(serde_json::from_value(
                state["view"].get(key).cloned().unwrap_or(json!([])),
            )?)
        };
        let (executable, binary) = (set("executable")?, set("binary")?);
        let (symlinks, submodules) = (set("symlinks")?, set("submodules")?);
        let files: Files = texts
            .into_iter()
            .map(|(path, text)| {
                let kind = if symlinks.contains(&path) {
                    Kind::Symlink
                } else if submodules.contains(&path) {
                    Kind::Submodule
                } else {
                    Kind::File
                };
                let data = FileData {
                    executable: executable.contains(&path),
                    binary: binary.contains(&path),
                    text,
                    kind,
                };
                (path, data)
            })
            .collect();
        let patches: Vec<u64> = serde_json::from_value(state["view"]["patches"].clone())?;
        if channel.is_empty()
            || !channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("Invalid channel name"));
        }
        let reference = format!("refs/o5s/{channel}");
        let old = self
            .run(&["rev-parse", "--verify", &reference], None)
            .ok()
            .map(text)
            .transpose()?;
        if let Some(old) = &old {
            if context
                .snapshots
                .get(old)
                .is_some_and(|s| s.patches == patches)
            {
                return Ok(
                    json!({"ref":reference,"commit":old,"patches":patches,"unchanged":true}),
                );
            }
        }
        // Build objects and a tree without touching the real index or checkout.
        let mut entries = BTreeMap::new();
        for (path, content) in files {
            if !valid_path(&path) {
                return Err(error("Server supplied an unsafe file path"));
            }
            let entry = match content.kind {
                Kind::Submodule => {
                    if content.text.len() != 40
                        || !content.text.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return Err(error("Server supplied an invalid submodule commit"));
                    }
                    ("160000 commit", content.text.clone())
                }
                kind => {
                    let mode = match (kind, content.executable) {
                        (Kind::Symlink, _) => "120000 blob",
                        (_, true) => "100755 blob",
                        _ => "100644 blob",
                    };
                    let hash = text(
                        self.run(&["hash-object", "-w", "--stdin"], Some(&content.bytes()?))?,
                    )?;
                    (mode, hash)
                }
            };
            entries.insert(path, (entry.1, entry.0));
        }
        let tree = self.write_tree(&entries)?;
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(p) = &old {
            args.extend(["-p", p.as_str()]);
        }
        let msg = format!(
            "o5s {} / {channel}\n\nPatch handles: {}\n",
            context.repository,
            patches
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let commit = text(self.run(&args, Some(msg.as_bytes()))?)?;
        // Record metadata before exposing the ref. Orphan metadata is harmless
        // if Git's atomic old-value ref check fails under a concurrent fetch.
        context.snapshots.insert(
            commit.clone(),
            Snapshot {
                patches: patches.clone(),
                bases: snapshot_bases(state, &patches)?,
            },
        );
        save(&self.context_path(), context)?;
        self.run(
            &[
                "update-ref",
                &reference,
                &commit,
                old.as_deref().unwrap_or(""),
            ],
            None,
        )?;
        Ok(json!({"ref":reference,"commit":commit,"patches":patches,"unchanged":false}))
    }
    /// `files` maps a path to its object id and `"<mode> <type>"`.
    fn write_tree(&self, files: &BTreeMap<String, (String, &'static str)>) -> Result<String> {
        let mut dirs: BTreeMap<String, BTreeMap<String, (String, &'static str)>> = BTreeMap::new();
        let mut leaves: BTreeMap<String, String> = BTreeMap::new();
        for (path, (hash, mode)) in files {
            if let Some((dir, rest)) = path.split_once('/') {
                dirs.entry(dir.into())
                    .or_default()
                    .insert(rest.into(), (hash.clone(), *mode));
            } else {
                leaves.insert(path.clone(), format!("{mode} {hash}\t{path}\0"));
            }
        }
        for (dir, files) in dirs {
            let tree = self.write_tree(&files)?;
            leaves.insert(format!("{dir}/"), format!("040000 tree {tree}\t{dir}\0"));
        }
        text(self.run(
            &["mktree", "-z"],
            Some(leaves.values().cloned().collect::<String>().as_bytes()),
        )?)
    }
}
/// What a path holds: an ordinary file, a symlink (`text` is its target)
/// or a submodule (`text` is the commit id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Symlink,
    Submodule,
}
/// A file's content and Git mode. Non-UTF-8 content is carried as base64.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileData {
    text: String,
    executable: bool,
    binary: bool,
    kind: Kind,
}
impl FileData {
    fn from_bytes(bytes: Vec<u8>, executable: bool) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self {
                text,
                executable,
                binary: false,
                kind: Kind::File,
            },
            Err(e) => Self {
                text: base64_encode(e.as_bytes()),
                executable,
                binary: true,
                kind: Kind::File,
            },
        }
    }
    fn special(text: String, kind: Kind) -> Self {
        Self {
            text,
            executable: false,
            binary: false,
            kind,
        }
    }
    fn bytes(&self) -> Result<Vec<u8>> {
        if self.binary {
            base64_decode(&self.text).ok_or_else(|| error("Server sent invalid base64 content"))
        } else {
            Ok(self.text.clone().into_bytes())
        }
    }
}
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                BASE64[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = if i >= 4 - pad {
                0
            } else {
                BASE64.iter().position(|b| b == c)? as u32
            };
            n |= v << (18 - 6 * i);
        }
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad.min(2)]);
    }
    (base64_encode(&out) == text).then_some(out)
}
type Files = BTreeMap<String, FileData>;

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}
fn raw_git(root: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    if let Some(root) = root {
        cmd.arg("-C").arg(root);
    }
    let mut child = cmd
        .args(args)
        .env("GIT_AUTHOR_NAME", "o5s")
        .env("GIT_AUTHOR_EMAIL", "o5s@localhost")
        .env("GIT_COMMITTER_NAME", "o5s")
        .env("GIT_COMMITTER_EMAIL", "o5s@localhost")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or("Missing Git stdin")?
            .write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(error(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}
fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().into())
}
fn edit_json(path: &str, file: Option<&FileData>) -> Value {
    let mut edit = json!({"path": path, "content": file.map(|f| &f.text)});
    if let Some(f) = file {
        if f.executable {
            edit["executable"] = json!(true);
        }
        if f.binary {
            edit["base64"] = json!(true);
        }
        match f.kind {
            Kind::File => {}
            Kind::Symlink => edit["kind"] = json!("symlink"),
            Kind::Submodule => edit["kind"] = json!("submodule"),
        }
    }
    edit
}
fn edits(before: &Files, after: &Files) -> Vec<Value> {
    let mut paths: Vec<&String> = before.keys().chain(after.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|p| before.get(*p) != after.get(*p))
        .map(|p| edit_json(p, after.get(p)))
        .collect()
}
// The snapshot records a sole causal file head only. A merged file with
// multiple concurrent heads must first receive an explicit resolution before
// subsequent edits can use immutable-base ranges.
fn snapshot_bases(state: &Value, active: &[u64]) -> Result<BTreeMap<String, u64>> {
    let registry = state["repository"]["patches"]
        .as_object()
        .ok_or("Missing patch registry")?;
    let mut heads: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut ids = active.to_vec();
    ids.sort_unstable();
    for id in ids {
        let patch = registry
            .get(&id.to_string())
            .ok_or("Unknown snapshot patch")?;
        let dependencies: Vec<u64> = serde_json::from_value(patch["dependencies"].clone())?;
        for edit in patch["edits"].as_array().ok_or("Missing patch edits")? {
            let path = edit["path"].as_str().ok_or("Missing edit path")?;
            let current = heads.entry(path.into()).or_default();
            let mut targets = current.clone();
            let mut stack = dependencies.clone();
            let mut visited = BTreeSet::new();
            let min = targets.first().copied().unwrap_or(u64::MAX);
            while !targets.is_empty() {
                let Some(parent) = stack.pop() else { break };
                if parent < min || !visited.insert(parent) {
                    continue;
                }
                if targets.remove(&parent) {
                    current.remove(&parent);
                }
                if !targets.is_empty() {
                    let record = registry
                        .get(&parent.to_string())
                        .ok_or("Unknown dependency")?;
                    stack.extend(serde_json::from_value::<Vec<u64>>(
                        record["dependencies"].clone(),
                    )?);
                }
            }
            current.insert(id);
        }
    }
    Ok(heads
        .into_iter()
        .filter_map(|(path, heads)| (heads.len() == 1).then(|| (path, *heads.first().unwrap())))
        .collect())
}
fn line_changes(
    before: &Files,
    after: &Files,
    bases: &BTreeMap<String, u64>,
) -> Option<Vec<Value>> {
    let mut changes = Vec::new();
    for edit in edits(before, after) {
        let path = edit["path"].as_str()?;
        let (original, updated) = (before.get(path)?, after.get(path)?);
        // Line ranges change text only; binary content and mode changes are whole-file edits.
        if original.binary
            || updated.binary
            || original.executable != updated.executable
            || original.kind != Kind::File
            || updated.kind != Kind::File
        {
            return None;
        }
        let (original, updated) = (&original.text, &updated.text);
        let base = bases.get(path)?;
        let old: Vec<&str> = original.split_inclusive('\n').collect();
        let new: Vec<&str> = updated.split_inclusive('\n').collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        changes.push(json!({"path":path,"base":base,"start":prefix,"end":old.len()-suffix,"content":new[prefix..new.len()-suffix].concat()}));
    }
    Some(changes)
}
// Preserve complete unchanged atom blocks. A partial edit replaces its whole
// atom; inserted text is split at LF boundaries so later edits retain anchors.
fn graph_operations(document: &Value, original: &str, updated: &str) -> Result<Vec<Value>> {
    let layers = document["layers"]
        .as_array()
        .ok_or("Missing document layers")?;
    let conflicts = document["conflicts"]
        .as_array()
        .ok_or("Missing document conflicts")?;
    if !conflicts.is_empty()
        || layers
            .iter()
            .any(|layer| layer.as_array().is_none_or(|ids| ids.len() != 1))
    {
        return Err(error(
            "Graph document has unresolved conflicts; inspect o5s document and submit explicit reviewed operations",
        ));
    }
    let rendered: Vec<u8> = serde_json::from_value(document["bytes"].clone())
        .map_err(|_| error("Graph document has no unambiguous rendered bytes"))?;
    if rendered != original.as_bytes() {
        return Err(error(
            "Graph causal view differs from the Git snapshot; refusing to overwrite unobserved content",
        ));
    }
    let nodes = document["nodes"]
        .as_array()
        .ok_or("Missing document nodes")?;
    let mut atoms = Vec::new();
    for layer in layers {
        let id = &layer[0];
        let node = nodes
            .iter()
            .find(|node| node.get("id") == Some(id))
            .ok_or("Missing rendered document atom")?;
        let bytes: Vec<u8> = serde_json::from_value(node["bytes"].clone())?;
        if bytes.is_empty() {
            return Err(error("Document contains an empty atom"));
        }
        atoms.push((id.clone(), bytes));
    }
    if atoms
        .iter()
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect::<Vec<_>>()
        != rendered
    {
        return Err(error("Document nodes do not match its rendered bytes"));
    }
    let target = updated.as_bytes();
    let mut prefix = 0;
    let mut left_bytes = 0;
    while prefix < atoms.len() && target[left_bytes..].starts_with(&atoms[prefix].1) {
        left_bytes += atoms[prefix].1.len();
        prefix += 1;
    }
    let mut end = atoms.len();
    let mut right_bytes = target.len();
    while end > prefix
        && right_bytes >= left_bytes + atoms[end - 1].1.len()
        && target[..right_bytes].ends_with(&atoms[end - 1].1)
    {
        right_bytes -= atoms[end - 1].1.len();
        end -= 1;
    }
    let mut operations = Vec::new();
    if prefix < end {
        operations.push(json!({"kind":"delete","targets":atoms[prefix..end].iter().map(|(id,_)|id).collect::<Vec<_>>()}));
    }
    if left_bytes < right_bytes {
        let replacement = std::str::from_utf8(&target[left_bytes..right_bytes])?;
        let left = if prefix == 0 {
            json!("Start")
        } else {
            json!({"Node":atoms[prefix-1].0})
        };
        let right = if end == atoms.len() {
            json!("End")
        } else {
            json!({"Node":atoms[end].0})
        };
        operations.push(json!({"kind":"insert","left":left,"right":right,
            "atoms":replacement.split_inclusive('\n').map(|line|line.as_bytes()).collect::<Vec<_>>() }));
    }
    Ok(operations)
}

/// Graph edits for graph documents, and whole-file edits for binary or legacy
/// files, from a Git diff. `None` keeps a legacy repository's representation.
fn automatic_graph_edits(
    api: &Api,
    repo: &str,
    channel: &str,
    state: &Value,
    snapshot: &Snapshot,
    before: &Files,
    after: &Files,
    changes: &[Value],
) -> Result<Option<(Vec<Value>, Vec<Value>)>> {
    let registry = state["repository"]["patches"]
        .as_object()
        .ok_or("Missing patch registry")?;
    let graph_paths: BTreeSet<&str> = registry
        .values()
        .filter_map(|patch| patch["graph_edits"].as_array())
        .flatten()
        .filter_map(|edit| edit["path"].as_str())
        .collect();
    let legacy_paths: BTreeSet<&str> = registry
        .values()
        .filter_map(|patch| patch["edits"].as_array())
        .flatten()
        .filter_map(|edit| edit["path"].as_str())
        .collect();
    let changed_graph = changes
        .iter()
        .any(|edit| graph_paths.contains(edit["path"].as_str().unwrap_or("")));
    // Existing legacy repositories keep their historical representation for
    // new paths. A new repository, or one with graph documents, uses native
    // document operations for text and whole-file edits for binary content.
    if !changed_graph && !legacy_paths.is_empty() {
        return Ok(None);
    }
    let patches = snapshot
        .patches
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut graph_edits = Vec::new();
    let mut files = Vec::new();
    for edit in changes {
        let path = edit["path"].as_str().ok_or("Missing edited path")?;
        let updated = after.get(path);
        // Binary files, symlinks and submodules are whole-file edits.
        let legacy = legacy_paths.contains(path)
            || (!graph_paths.contains(path)
                && updated.is_some_and(|f| f.binary || f.kind != Kind::File));
        if legacy {
            files.push(edit.clone());
            continue;
        }
        let Some(updated) = updated else {
            graph_edits.push(json!({"path":path,"operations":[],"remove":true}));
            continue;
        };
        if updated.binary || updated.kind != Kind::File {
            return Err(error(format!(
                "{path} is a text graph document; binary content, a symlink or a submodule needs a new path"
            )));
        }
        let document = api.request(
            "/api/document",
            &[
                ("repo", repo),
                ("channel", channel),
                ("path", path),
                ("patches", &patches),
            ],
            None,
        )?;
        // A removed document keeps its graph; recreating it replaces those atoms.
        let original = match before.get(path) {
            Some(f) => f.text.clone(),
            None => serde_json::from_value::<Vec<u8>>(document["bytes"].clone())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default(),
        };
        let operations =
            if graph_paths.contains(path) || !original.is_empty() || !updated.text.is_empty() {
                graph_operations(&document, &original, &updated.text)?
            } else {
                Vec::new()
            };
        let mut graph = json!({"path":path,"operations":operations});
        if updated.executable {
            graph["executable"] = json!(true);
        }
        graph_edits.push(graph);
    }
    Ok(Some((graph_edits, files)))
}

fn read_graph_edits(path: &Path) -> Result<Value> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(error("Graph operations file exceeds 4 MiB limit"));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let edits = value
        .as_array()
        .ok_or("Graph operations file must contain a JSON edits array")?;
    if edits.is_empty() {
        return Err(error("Graph edits array must not be empty"));
    }
    for edit in edits {
        let path = edit["path"]
            .as_str()
            .ok_or("Each graph edit needs a path")?;
        if !valid_path(path) {
            return Err(error("Graph edit contains an unsupported path"));
        }
        if !edit["operations"].is_array() {
            return Err(error("Each graph edit needs an operations array"));
        }
    }
    // The server validates operation variants, node anchors and dependencies.
    Ok(value)
}
fn execute(cli: &Cli) -> Result<Value> {
    // Serialize profile read-modify-write without serializing unrelated Git work.
    let _profile_lock = if matches!(cli.command, Action::Login { .. } | Action::Logout) {
        let path = config_path()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut lock_name = path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock_name))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Some(file)
    } else {
        None
    };
    let mut profiles = profiles()?;
    if let Action::Logout = cli.command {
        profiles.profiles.remove(&cli.profile);
        save(&config_path()?, &profiles)?;
        return Ok(json!({"profile":cli.profile,"logged_out":true}));
    }
    if let Action::Login { token_file } = &cli.command {
        let token = if let Some(file) = token_file {
            fs::read_to_string(file)?
        } else {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        };
        let url = cli.url.clone().unwrap_or("http://127.0.0.1:3000".into());
        let api = Api::new(url, token.trim().into())?;
        let result = api.request("/api/repositories", &[], None)?;
        profiles.profiles.insert(
            cli.profile.clone(),
            Profile {
                url: api.url.clone(),
                token: api.token,
            },
        );
        save(&config_path()?, &profiles)?;
        return Ok(json!({"profile":cli.profile,"url":api.url,"actor":result["actor"]}));
    }
    let git = Git::discover()?;
    // Native Git worktrees share snapshot metadata and remote refs.
    fs::create_dir_all(git.common.join("o5s"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(git.common.join("o5s/context.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let existing = if matches!(cli.command, Action::Connect { .. }) {
        None
    } else {
        Some(git.context()?)
    };
    let profile = profiles.profiles.get(&cli.profile);
    let url = cli
        .url
        .clone()
        .or_else(|| existing.as_ref().map(|c| c.url.clone()))
        .or_else(|| profile.map(|p| p.url.clone()))
        .unwrap_or("http://127.0.0.1:3000".into());
    let token = match std::env::var("O5S_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            let profile = profile.ok_or("Set O5S_TOKEN or run o5s login")?;
            if origin(&profile.url)? != origin(&url)? {
                return Err(error(
                    "Saved token belongs to a different origin; select a matching profile",
                ));
            }
            profile.token.clone()
        }
    };
    let api = Api::new(url, token)?;
    let repos = api.request("/api/repositories", &[], None)?;
    let tenant = repos["actor"]["tenant"]
        .as_u64()
        .ok_or("Missing authenticated tenant")?;
    if let Some(context) = &existing {
        if context.tenant != tenant {
            return Err(error(
                "Authenticated tenant differs from this Git repository's connection",
            ));
        }
    }
    if let Action::Connect { repository } = &cli.command {
        if repository.is_empty()
            || !repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("Invalid repository name"));
        }
        let context = if git.context_path().try_exists()? {
            let old = git.context()?;
            if old.repository != *repository || old.url != api.url || old.tenant != tenant {
                return Err(error(
                    "Git repository is already connected elsewhere; use a separate clone",
                ));
            }
            old
        } else {
            Context {
                repository: repository.clone(),
                url: api.url.clone(),
                tenant,
                snapshots: BTreeMap::new(),
            }
        };
        if !repos["repositories"]
            .as_array()
            .ok_or("Malformed repository list")?
            .iter()
            .any(|r| r.as_str() == Some(repository))
        {
            api.command(repository, json!({"cmd":"create"}))?;
        }
        save(&git.context_path(), &context)?;
        return Ok(json!({"repository":repository,"url":api.url,"git_root":git.root}));
    }
    let mut context = existing.ok_or("Missing repository connection")?;
    if context.url != api.url {
        return Err(error(
            "URL differs from this Git repository's saved connection",
        ));
    }
    let repo = &context.repository.clone();
    match &cli.command {
        Action::Status { channel } | Action::Propose { channel, .. } => {
            let state = api.state(repo, channel)?;
            let (base, snapshot) = git.baseline(&context, &state, channel)?;
            let before = git.committed(base.as_deref())?;
            let after = git.working()?;
            let changes = edits(&before, &after);
            if let Action::Propose { intent, .. } = &cli.command {
                if changes.is_empty() {
                    return Err(error("No changes to propose"));
                }
                let graph = automatic_graph_edits(
                    &api, repo, channel, &state, &snapshot, &before, &after, &changes,
                )?;
                let command = if let Some((graph_edits, files)) =
                    graph.filter(|(g, _)| !g.is_empty())
                {
                    json!({"cmd":"propose_graph","intent":intent,"dependencies":snapshot.patches,"edits":graph_edits,"files":files})
                } else if let Some(ranges) = line_changes(&before, &after, &snapshot.bases) {
                    json!({
                        "cmd": "propose_lines", "intent": intent,
                        "dependencies": snapshot.patches, "edits": ranges
                    })
                } else {
                    json!({
                        "cmd": "propose", "intent": intent,
                        "dependencies": snapshot.patches, "edits": changes
                    })
                };
                api.command(repo, command)
            } else {
                let changed_paths: Vec<_> = changes
                    .iter()
                    .map(|edit| json!({"path": edit["path"], "deleted": edit["content"].is_null()}))
                    .collect();
                Ok(json!({
                    "repository": repo, "channel": channel, "base_commit": base,
                    "dependencies": snapshot.patches, "changes": changed_paths,
                    "conflicts": state["view"]["conflicts"]
                }))
            }
        }
        Action::Resolve {
            path,
            content_file,
            delete,
            intent,
            channel,
        } => {
            let state = api.state(repo, channel)?;
            if state["view"]["conflicts"].get(path).is_none() {
                return Err(error(
                    "Path has no current conflict; use o5s propose for ordinary edits",
                ));
            }
            let content = if *delete {
                None
            } else {
                Some(fs::read_to_string(
                    content_file.as_ref().ok_or("Missing resolution content")?,
                )?)
            };
            api.command(
                repo,
                json!({
                    "cmd": "propose", "intent": intent,
                    "dependencies": state["view"]["patches"],
                    "edits": [{"path": path, "content": content}]
                }),
            )
        }
        Action::Fetch { channel } => {
            let state = api.state(repo, channel)?;
            git.fetch(&mut context, &state, channel)
        }
        Action::View { channel } => {
            api.request("/api/view", &[("repo", repo), ("channel", channel)], None)
        }
        Action::Log { after, limit } => api.request(
            "/api/patches",
            &[
                ("repo", repo),
                ("after", &after.to_string()),
                ("limit", &limit.to_string()),
            ],
            None,
        ),
        Action::Show { patch } => {
            let detail = api.request(
                "/api/patch",
                &[("repo", repo), ("id", &patch.to_string())],
                None,
            )?;
            detail
                .get("patch")
                .filter(|p| p.is_object())
                .cloned()
                .ok_or_else(|| error("Server returned an invalid proposal response"))
        }
        Action::Approve { patch } => api.command(repo, json!({"cmd": "approve", "patch": patch})),
        Action::Ci {
            command,
            patch,
            timeout,
        } => run_checks(&api, &git, repo, command, *patch, *timeout),
        Action::Check { patch, pass, .. } => api.command(
            repo,
            json!({
                "cmd": "check", "patch": patch, "passed": pass
            }),
        ),
        Action::Merge { patches, channel } => api.command(
            repo,
            json!({
                "cmd": "merge_batch", "channel": channel, "patches": patches
            }),
        ),
        Action::Channel { name, from } => api.command(
            repo,
            json!({
                "cmd": "fork", "source": from, "channel": name
            }),
        ),
        Action::ProposeLines {
            base,
            path,
            start,
            end,
            content_file,
            intent,
            depends,
        } => {
            let content = fs::read_to_string(content_file)?;
            api.command(repo, json!({
                "cmd": "propose_lines", "intent": intent, "dependencies": depends,
                "edits": [{"path": path, "base": base, "start": start, "end": end, "content": content}]
            }))
        }
        Action::ProposeGraph {
            operations_file,
            intent,
            dependencies,
        } => {
            let edits = read_graph_edits(operations_file)?;
            api.command(
                repo,
                json!({
                    "cmd": "propose_graph", "intent": intent,
                    "dependencies": dependencies, "edits": edits
                }),
            )
        }
        Action::Document { path, channel } => api.request(
            "/api/document",
            &[("repo", repo), ("channel", channel), ("path", path)],
            None,
        ),
        Action::Login { .. } | Action::Logout | Action::Connect { .. } => unreachable!(),
    }
}

fn main() {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(value) => println!(
            "{}",
            if cli.compact {
                serde_json::to_string(&value).unwrap()
            } else {
                serde_json::to_string_pretty(&value).unwrap()
            }
        ),
        Err(e) => {
            eprintln!("o5s: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_graph_edit_refuses_ambiguity_and_unobserved_bytes() {
        let mut document = json!({"layers":[[1,2]],"conflicts":[],"nodes":[],"bytes":[97]});
        assert!(
            graph_operations(&document, "a", "b")
                .unwrap_err()
                .to_string()
                .contains("unresolved conflicts")
        );
        document["layers"] = json!([[1]]);
        document["conflicts"] = json!([{"kind":"delete_insert"}]);
        assert!(
            graph_operations(&document, "a", "b")
                .unwrap_err()
                .to_string()
                .contains("unresolved conflicts")
        );
        document["conflicts"] = json!([]);
        assert!(
            graph_operations(&document, "old", "new")
                .unwrap_err()
                .to_string()
                .contains("unobserved content")
        );
    }

    #[test]
    fn graph_commands_parse_explicit_dependencies_and_document_channel() {
        let cli = Cli::try_parse_from([
            "o5s",
            "propose-graph",
            "--operations-file",
            "operations.json",
            "--intent",
            "insert block",
            "--dependencies",
            "1,4",
        ])
        .unwrap();
        match cli.command {
            Action::ProposeGraph {
                dependencies,
                operations_file,
                intent,
            } => {
                assert_eq!(dependencies, [1, 4]);
                assert_eq!(operations_file, PathBuf::from("operations.json"));
                assert_eq!(intent, "insert block");
            }
            _ => panic!("Wrong command"),
        }
        let cli =
            Cli::try_parse_from(["o5s", "document", "src/main.rs", "--channel", "review"]).unwrap();
        assert!(
            matches!(cli.command, Action::Document { path, channel } if path == "src/main.rs" && channel == "review")
        );
        assert!(
            Cli::try_parse_from([
                "o5s",
                "propose-graph",
                "--operations-file",
                "operations.json"
            ])
            .is_err()
        );
    }
    #[test]
    fn graph_files_reject_envelopes_unsafe_paths_and_oversize_before_http() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("operations.json");
        for invalid in [
            r#"{"edits":[]}"#,
            "[]",
            r#"[{"path":"../secret","operations":[]}]"#,
            r#"[{"path":"file","operations":{}}]"#,
        ] {
            fs::write(&file, invalid).unwrap();
            assert!(read_graph_edits(&file).is_err());
        }
        fs::write(&file, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
        assert!(
            read_graph_edits(&file)
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
        fs::write(&file, r#"[{"path":"file","operations":[]}]"#).unwrap();
        assert!(read_graph_edits(&file).is_ok());
    }
}
