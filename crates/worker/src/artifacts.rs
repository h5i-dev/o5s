//! Git projections in Cloudflare Artifacts: push a channel's admitted
//! snapshot as a commit on `refs/heads/<channel>` of the Artifacts repository
//! `<tenant>-<repo>`. Agents get read tokens for it, and each agent gets its
//! own workspace repository `<tenant>-<repo>.<user>` (names cannot contain
//! `.`, so the two never collide) holding the snapshot on `o5s/<channel>`.
//!
//! The Artifacts binding cannot write, so the Worker speaks Git's push
//! protocol itself (`o5s_gitpush`) with a short-lived write token. Each commit
//! records the registry generation it shows; an export never replaces a
//! commit of a newer generation, and the push names the remote's current
//! commit as the old value, so concurrent exports cannot lose one another.
use crate::workspace::{Entry, Kind, Objects};
use js_sys::{Function, Promise, Reflect, Uint8Array};
use o5s_gitpush as git;
use o5s_platform::{Repository, View, workspace};
use std::collections::BTreeMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The commit message of a snapshot: its generation and patch identities.
pub fn message(generation: u64, repo: &Repository, view: &View) -> String {
    workspace::message(Some(generation), &hashes(repo, view))
}

fn hashes<'a>(repo: &'a Repository, view: &View) -> Vec<&'a str> {
    view.patches
        .iter()
        .filter_map(|id| repo.patches.get(id).map(|p| p.hash.as_str()))
        .collect()
}

/// The generation an o5s commit message records, if any.
pub fn generation_of(message: &str) -> Option<u64> {
    workspace::parse(message)?.0
}

/// The files of a conflict-free view, as bytes and modes.
pub fn files(view: &View) -> Result<BTreeMap<String, git::File>, String> {
    if !view.conflicts.is_empty() {
        return Err("resolve conflicts before Git export".into());
    }
    view.files
        .keys()
        .map(|p| {
            let (bytes, mode) = view
                .entry(p)
                .ok_or_else(|| format!("{p}: invalid binary content or submodule id"))?;
            let mode = match mode {
                o5s_platform::Mode::Regular => git::Mode::Regular,
                o5s_platform::Mode::Executable => git::Mode::Executable,
                o5s_platform::Mode::Symlink => git::Mode::Symlink,
                o5s_platform::Mode::Gitlink => git::Mode::Gitlink,
            };
            Ok((p.clone(), (bytes, mode)))
        })
        .collect()
}

fn js_err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// `target.method(...args)`, awaited if it returns a promise.
async fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let f: Function = Reflect::get(target, &method.into())
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| format!("Artifacts has no method {method}"))?;
    let array = js_sys::Array::new();
    for a in args {
        array.push(a);
    }
    // `Reflect.apply`, not `f.apply`: on an RPC stub, the property `apply`
    // would itself be a remote method.
    let value = Reflect::apply(&f, target, &array).map_err(js_err)?;
    // Binding methods return RPC promises, which are thenables but not
    // `Promise` instances; `Promise.resolve` awaits any thenable.
    JsFuture::from(Promise::resolve(&value))
        .await
        .map_err(js_err)
}

/// A short description of a value the binding returned, for error messages.
fn describe(v: &JsValue) -> String {
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_else(|| format!("{v:?}"))
}

fn field(v: &JsValue, name: &str) -> Option<String> {
    Reflect::get(v, &name.into())
        .ok()
        .and_then(|x| x.as_string())
}

/// A repository capability, disposed of when dropped.
struct Repo(JsValue);

impl Drop for Repo {
    fn drop(&mut self) {
        // `Symbol.dispose`, where the runtime defines it.
        let dispose = Reflect::get(&js_sys::global(), &"Symbol".into())
            .and_then(|s| Reflect::get(&s, &"dispose".into()));
        if let Ok(key) = dispose
            && !key.is_undefined()
            && let Ok(f) = Reflect::get(&self.0, &key)
            && let Ok(f) = f.dyn_into::<Function>()
        {
            let _ = Reflect::apply(&f, &self.0, &js_sys::Array::new());
        }
    }
}

/// The repository named `name`, created with branch `main` if it is missing.
async fn open(binding: &JsValue, name: &str) -> Result<(Repo, String), String> {
    let repo = match call(binding, "get", &[name.into()]).await {
        Ok(r) => Repo(r),
        Err(_) => {
            let opts = js_sys::Object::new();
            Reflect::set(&opts, &"setDefaultBranch".into(), &"main".into()).map_err(js_err)?;
            call(binding, "create", &[name.into(), opts.into()]).await?;
            Repo(call(binding, "get", &[name.into()]).await?)
        }
    };
    let info = call(&repo.0, "info", &[]).await?;
    let remote = field(&info, "remote").ok_or_else(|| {
        format!(
            "Artifacts repository info has no remote: {}",
            describe(&info)
        )
    })?;
    Ok((repo, remote))
}

async fn http(
    url: &str,
    token: &str,
    body: Option<Vec<u8>>,
    content_type: &str,
) -> Result<Vec<u8>, String> {
    let headers = worker::Headers::new();
    headers
        .set("authorization", &format!("Bearer {token}"))
        .map_err(|e| e.to_string())?;
    headers
        .set("git-protocol", "version=0")
        .map_err(|e| e.to_string())?;
    let mut init = worker::RequestInit::new();
    init.with_headers(headers);
    if let Some(b) = body {
        init.headers
            .set("content-type", content_type)
            .map_err(|e| e.to_string())?;
        init.with_method(worker::Method::Post)
            .with_body(Some(Uint8Array::from(b.as_slice()).into()));
    }
    let request = worker::Request::new_with_init(url, &init).map_err(|e| e.to_string())?;
    let mut response = worker::Fetch::Request(request)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status_code();
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "{url}: HTTP {status}: {}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
        ));
    }
    Ok(bytes)
}

fn binding(env: &worker::Env) -> Result<JsValue, String> {
    let binding = Reflect::get(env.as_ref(), &"ARTIFACTS".into()).map_err(js_err)?;
    if binding.is_undefined() {
        return Err("Artifacts binding is not configured".into());
    }
    Ok(binding)
}

/// A token for `name` with `scope` ("read" or "write"), valid `ttl` seconds.
async fn token(artifact: &Repo, scope: &str, ttl: u32) -> Result<(String, String), String> {
    let t = call(&artifact.0, "createToken", &[scope.into(), ttl.into()]).await?;
    let plaintext = field(&t, "plaintext")
        .ok_or_else(|| format!("Artifacts returned no token: {}", describe(&t)))?;
    Ok((plaintext, field(&t, "expiresAt").unwrap_or_default()))
}

/// Push `view` of `repo` to the Artifacts repository `name`.
pub async fn export(
    env: &worker::Env,
    name: &str,
    channel: &str,
    repo: &Repository,
    view: &View,
) -> Result<serde_json::Value, String> {
    let (artifact, remote) = open(&binding(env)?, name).await?;
    push(
        &artifact,
        &remote,
        &format!("refs/heads/{channel}"),
        repo,
        view,
    )
    .await
}

/// Clone access to the projection `name`: its remote and a read token.
pub async fn access(env: &worker::Env, name: &str) -> Result<serde_json::Value, String> {
    let (artifact, remote) = open(&binding(env)?, name).await?;
    let (token, expires) = token(&artifact, "read", 3600).await?;
    Ok(serde_json::json!({"remote":remote,"token":token,"expires_at":expires}))
}

/// Put the snapshot on `o5s/<channel>` of the agent's workspace repository
/// `name` and hand out a write token for it. The agent clones it, commits on
/// another branch and pushes; `propose` reads the changes from there.
pub async fn workspace(
    env: &worker::Env,
    name: &str,
    channel: &str,
    repo: &Repository,
    view: &View,
) -> Result<serde_json::Value, String> {
    let (artifact, remote) = open(&binding(env)?, name).await?;
    let branch = format!("o5s/{channel}");
    let mut pushed = push(
        &artifact,
        &remote,
        &format!("refs/heads/{branch}"),
        repo,
        view,
    )
    .await?;
    let (token, expires) = token(&artifact, "write", 3600).await?;
    pushed["branch"] = branch.into();
    pushed["token"] = token.into();
    pushed["expires_at"] = expires.into();
    Ok(pushed)
}

/// Objects of an Artifacts repository.
struct Remote<'a>(&'a Repo);

impl Objects for Remote<'_> {
    async fn tree(&self, hash: &str) -> Result<Vec<Entry>, String> {
        let v = call(&self.0.0, "readTree", &[hash.into()]).await?;
        let array: js_sys::Array = v.dyn_into().map_err(|_| format!("tree {hash} not found"))?;
        array
            .iter()
            .map(|e| {
                let kind = field(&e, "type").and_then(|t| Kind::parse(&t));
                match (field(&e, "name"), kind, field(&e, "hash")) {
                    (Some(name), Some(kind), Some(hash)) => Ok(Entry { name, kind, hash }),
                    _ => Err(format!("unexpected tree entry: {}", describe(&e))),
                }
            })
            .collect()
    }
    async fn blob(&self, hash: &str) -> Result<Vec<u8>, String> {
        let blob = call(&self.0.0, "readBlob", &[hash.into()]).await?;
        if blob.is_null() || blob.is_undefined() {
            return Err(format!("blob {hash} not found"));
        }
        let buffer = call(&blob, "arrayBuffer", &[]).await?;
        Ok(Uint8Array::new(&buffer).to_vec())
    }
}

/// A branch name an agent may push: Git ref characters, no `..`, and not
/// under `o5s/`, which holds the snapshots.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.starts_with("o5s/")
        && !b.contains("..")
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c))
}

/// The workspace's changes on `branch` since the newest projection commit on
/// its first-parent chain, and that commit's message.
pub async fn changes(
    env: &worker::Env,
    name: &str,
    branch: &str,
) -> Result<(String, String, Vec<o5s_platform::Edit>), o5s_platform::Refusal> {
    use o5s_platform::Refusal;
    let fail = |e: String| Refusal(format!("workspace read failed: {e}"));
    let artifact = Repo(
        call(&binding(env).map_err(fail)?, "get", &[name.into()])
            .await
            .map_err(|_| Refusal("workspace not found; create it first".into()))?,
    );
    let opts = js_sys::Object::new();
    Reflect::set(&opts, &"ref".into(), &branch.into()).map_err(|e| fail(js_err(e)))?;
    Reflect::set(&opts, &"limit".into(), &1000.into()).map_err(|e| fail(js_err(e)))?;
    let log: js_sys::Array = call(&artifact.0, "log", &[opts.into()])
        .await
        .map_err(fail)?
        .dyn_into()
        .map_err(|_| fail("log is not an array".into()))?;
    let head = log.get(0);
    if head.is_undefined() {
        return Err(Refusal(format!(
            "branch {branch} not found in the workspace"
        )));
    }
    let base = log
        .iter()
        .find(|c| field(c, "message").is_some_and(|m| workspace::parse(&m).is_some()))
        .ok_or_else(|| Refusal("no o5s snapshot in the branch's first-parent history".into()))?;
    let tree = |c: &JsValue| field(c, "treeHash").ok_or_else(|| fail(describe(c)));
    let edits = crate::workspace::changes(&Remote(&artifact), &tree(&base)?, &tree(&head)?).await?;
    Ok((
        field(&base, "hash").unwrap_or_default(),
        field(&base, "message").unwrap_or_default(),
        edits,
    ))
}

/// Push a commit of `view` to `refname` unless it is already there or a
/// newer generation is.
async fn push(
    artifact: &Repo,
    remote: &str,
    refname: &str,
    repo: &Repository,
    view: &View,
) -> Result<serde_json::Value, String> {
    let files = files(view)?;
    let (tree, objects) = git::tree(&files)?;
    let hashes = hashes(repo, view);
    let message = message(repo.generation, repo, view);
    let (token, _) = token(artifact, "write", 300).await?;
    let remote = remote.trim_end_matches('/');
    for _ in 0..3 {
        let refs = git::advertised_refs(
            &http(
                &format!("{remote}/info/refs?service=git-receive-pack"),
                &token,
                None,
                "",
            )
            .await?,
        )?;
        let head = refs.get(refname).copied();
        if let Some(h) = head {
            let commit = call(&artifact.0, "readCommit", &[git::hex(&h).into()]).await?;
            let current = field(&commit, "message").unwrap_or_default();
            let parsed = workspace::parse(&current);
            // The same files from the same patch set: nothing to do.
            if field(&commit, "treeHash").as_deref() == Some(git::hex(&tree).as_str())
                && parsed.as_ref().is_some_and(|(_, h)| *h == hashes)
            {
                return Ok(
                    serde_json::json!({"remote":remote,"ref":refname,"commit":git::hex(&h),"unchanged":true}),
                );
            }
            if parsed
                .and_then(|(g, _)| g)
                .is_some_and(|g| g > repo.generation)
            {
                return Ok(
                    serde_json::json!({"remote":remote,"ref":refname,"commit":git::hex(&h),"stale":true}),
                );
            }
        }
        let seconds = (worker::Date::now().as_millis() / 1000) as u64;
        let commit = git::commit(
            &tree,
            head.as_ref(),
            "o5s <o5s@localhost>",
            seconds,
            &message,
        );
        let id = commit.id();
        let mut all = objects.clone();
        all.push(commit);
        let body = git::push_request(refname, &head.unwrap_or(git::ZERO), &id, &git::pack(&all));
        let report = http(
            &format!("{remote}/git-receive-pack"),
            &token,
            Some(body),
            "application/x-git-receive-pack-request",
        )
        .await?;
        match git::push_result(&report, refname) {
            Ok(()) => {
                return Ok(
                    serde_json::json!({"remote":remote,"ref":refname,"commit":git::hex(&id),
                    "generation":repo.generation}),
                );
            }
            // Another export moved the ref; look again.
            Err(e) if e.contains("refused") => continue,
            Err(e) => return Err(e),
        }
    }
    Err("the Artifacts branch kept moving; retry the export".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_round_trip_through_messages() {
        let repo = Repository::default();
        let view = View::default();
        assert_eq!(generation_of(&message(42, &repo, &view)), Some(42));
        assert_eq!(
            generation_of("o5s verified-admission projection\n\nPatch hashes:\n"),
            None
        );
        assert!(valid_branch("work") && valid_branch("agent/fix-1"));
        for b in ["", "o5s/main", "a..b", "/x", "x/", "x.lock", "a b", "a:b"] {
            assert!(!valid_branch(b), "{b}");
        }
        assert_eq!(generation_of("unrelated"), None);
    }
}
