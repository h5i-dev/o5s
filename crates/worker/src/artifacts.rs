//! Git projections in Cloudflare Artifacts: push a channel's admitted
//! snapshot as a commit on `refs/heads/<channel>` of the Artifacts repository
//! `<tenant>-<repo>`, the name the JavaScript adapter in `cloudflare/` uses.
//!
//! The Artifacts binding cannot write, so the Worker speaks Git's push
//! protocol itself (`o5s_gitpush`) with a short-lived write token. Each commit
//! records the registry generation it shows; an export never replaces a
//! commit of a newer generation, and the push names the remote's current
//! commit as the old value, so concurrent exports cannot lose one another.
use js_sys::{Function, Promise, Reflect, Uint8Array};
use o5s_gitpush as git;
use o5s_platform::{Repository, View};
use std::collections::BTreeMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

const TITLE: &str = "o5s verified-admission projection";

/// The commit message of a snapshot: its generation and patch identities.
pub fn message(generation: u64, repo: &Repository, view: &View) -> String {
    let mut m = format!("{TITLE} (generation {generation})\n\nPatch hashes:\n");
    for id in &view.patches {
        if let Some(p) = repo.patches.get(id) {
            m.push_str(&p.hash);
            m.push('\n');
        }
    }
    m
}

/// The generation an o5s commit message records, if any.
pub fn generation_of(message: &str) -> Option<u64> {
    let rest = message
        .strip_prefix(TITLE)?
        .trim_start()
        .strip_prefix("(generation ")?;
    rest[..rest.find(')')?].parse().ok()
}

/// The files of a conflict-free view, as bytes and modes.
pub fn files(view: &View) -> Result<BTreeMap<String, git::File>, String> {
    if !view.conflicts.is_empty() {
        return Err("resolve conflicts before Git export".into());
    }
    view.files
        .keys()
        .map(|p| {
            view.bytes(p)
                .map(|f| (p.clone(), f))
                .ok_or_else(|| format!("{p}: invalid binary content"))
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

/// Push `view` of `repo` to the Artifacts repository `name`.
pub async fn export(
    env: &worker::Env,
    name: &str,
    channel: &str,
    repo: &Repository,
    view: &View,
) -> Result<serde_json::Value, String> {
    let binding = Reflect::get(env.as_ref(), &"ARTIFACTS".into()).map_err(js_err)?;
    if binding.is_undefined() {
        return Err("Artifacts binding is not configured".into());
    }
    let files = files(view)?;
    let (tree, objects) = git::tree(&files)?;
    let message = message(repo.generation, repo, view);
    let refname = format!("refs/heads/{channel}");
    let (artifact, remote) = open(&binding, name).await?;
    let token = call(&artifact.0, "createToken", &["write".into(), 300.into()]).await?;
    let token = field(&token, "plaintext")
        .ok_or_else(|| format!("Artifacts returned no token: {}", describe(&token)))?;
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
        let head = refs.get(&refname).copied();
        if let Some(h) = head {
            let commit = call(&artifact.0, "readCommit", &[git::hex(&h).into()]).await?;
            let current = field(&commit, "message").unwrap_or_default();
            if current.is_empty() && !commit.is_null() && !commit.is_undefined() {
                worker::console_log!("readCommit returned no message: {}", describe(&commit));
            }
            if current.trim_end() == message.trim_end() {
                return Ok(
                    serde_json::json!({"remote":remote,"ref":refname,"commit":git::hex(&h),"unchanged":true}),
                );
            }
            if generation_of(&current).is_some_and(|g| g > repo.generation) {
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
        let body = git::push_request(&refname, &head.unwrap_or(git::ZERO), &id, &git::pack(&all));
        let report = http(
            &format!("{remote}/git-receive-pack"),
            &token,
            Some(body),
            "application/x-git-receive-pack-request",
        )
        .await?;
        match git::push_result(&report, &refname) {
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
        assert_eq!(generation_of("unrelated"), None);
    }
}
