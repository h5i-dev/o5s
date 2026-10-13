//! Agent workspaces: Git snapshots of an admitted channel that an agent edits
//! with ordinary Git, and the proposal its changes become. The native server
//! keeps workspaces as local worktrees; the Worker keeps them as Artifacts
//! repositories. Both read the snapshot's patch set from its commit message
//! and turn changed files into one proposal through this module: graph
//! operations for text documents, as `o5s propose` sends, and whole-file
//! edits for binary files and for repositories that predate graph documents.
use crate::documents::GraphEdit;
use crate::{Command, Edit, Kind, Refusal, Repository, base64_encode, valid_path};
use o5s_kernel::model::{Anchor, DocumentGraph, NodeId, Operation};
use std::collections::BTreeSet;

/// The first line of every projection commit message.
pub const TITLE: &str = "o5s verified-admission projection";

/// At most this many changed files in one workspace proposal.
pub const MAX_FILES: usize = 1024;
/// At most this many bytes per file and in total.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The message of a projection commit: optionally its registry generation,
/// then the hashes of the snapshot's causal heads (`heads`), whose causal
/// past is the admitted patch set. Older projections list every admitted
/// patch; both read back as the same snapshot.
pub fn message(generation: Option<u64>, hashes: &[&str]) -> String {
    let mut m = String::from(TITLE);
    if let Some(g) = generation {
        m.push_str(&format!(" (generation {g})"));
    }
    m.push_str("\n\nPatch hashes:\n");
    for h in hashes {
        m.push_str(h);
        m.push('\n');
    }
    m
}

/// The generation and patch hashes a projection commit message records, or
/// `None` when `message` is not one.
pub fn parse(message: &str) -> Option<(Option<u64>, Vec<&str>)> {
    let rest = message.strip_prefix(TITLE)?;
    let (generation, rest) = match rest.strip_prefix(" (generation ") {
        Some(r) => {
            let end = r.find(')')?;
            (Some(r[..end].parse().ok()?), &r[end + 1..])
        }
        None => (None, rest),
    };
    let hashes = rest.strip_prefix("\n\nPatch hashes:")?;
    let hashes = hashes.strip_prefix('\n').unwrap_or(hashes);
    let hashes: Vec<&str> = hashes.lines().filter(|l| !l.is_empty()).collect();
    if hashes
        .iter()
        .any(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return None;
    }
    Some((generation, hashes))
}

/// The causal heads of a set of patches: the members no member depends on,
/// ascending. Their causal past is the set's, so for a dependency-closed set
/// such as a channel it is the set itself (`causality::heads`).
pub fn heads(repo: &Repository, members: &[u64]) -> Result<Vec<u64>, Refusal> {
    let corrupt = || Refusal("corrupt patch registry".into());
    let mut members = members.to_vec();
    members.sort_unstable();
    members.dedup();
    let len = members.last().map_or(0, |m| *m as usize + 1);
    let mut rows = vec![Vec::new(); len];
    for m in &members {
        let p = repo.patches.get(m).ok_or_else(corrupt)?;
        if p.dependencies.iter().any(|d| d >= m) {
            return Err(corrupt());
        }
        rows[*m as usize] = p.dependencies.iter().map(|d| *d as usize).collect();
    }
    let dense: Vec<usize> = members.iter().map(|m| *m as usize).collect();
    let heads = o5s_kernel::causality::heads(&rows, &dense, vec![false; len]);
    Ok(heads.into_iter().map(|h| h as u64).collect())
}

/// The hashes a projection commit of the admitted set `patches` records.
pub fn snapshot<'a>(repo: &'a Repository, patches: &[u64]) -> Result<Vec<&'a str>, Refusal> {
    Ok(heads(repo, patches)?
        .iter()
        .map(|h| repo.patches[h].hash.as_str())
        .collect())
}

/// The dependencies of a proposal from a snapshot: the causal heads of its
/// recorded patches, each of which must be a member of `channel`. Their
/// causal past is that of the recorded patches, so the proposal depends on
/// exactly what it saw.
pub fn dependencies(
    repo: &Repository,
    channel: &str,
    hashes: &[&str],
) -> Result<Vec<u64>, Refusal> {
    let members: std::collections::HashSet<u64> = repo
        .channels
        .get(channel)
        .ok_or_else(|| Refusal("channel not found".into()))?
        .iter()
        .copied()
        .collect();
    let mut seen = Vec::with_capacity(hashes.len());
    for h in hashes {
        let id = repo
            .handle(h)
            .ok_or_else(|| Refusal("workspace patch identity not found".into()))?;
        if !members.contains(&id) {
            return Err(Refusal("workspace snapshot is outside this channel".into()));
        }
        seen.push(id);
    }
    heads(repo, &seen)
}

/// A changed file as a whole-file edit: UTF-8 as text, anything else as
/// base64. `None` content removes the file. For a symlink the bytes are its
/// target, for a submodule the commit id in hexadecimal.
pub fn edit(
    path: String,
    content: Option<Vec<u8>>,
    executable: bool,
    kind: crate::Kind,
) -> Result<Edit, Refusal> {
    if !valid_path(&path) {
        return Err(Refusal("invalid workspace file path".into()));
    }
    let (content, base64) = match content {
        None => (None, false),
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) => (Some(text), false),
            Err(e) => (Some(base64_encode(e.as_bytes())), true),
        },
    };
    let kind = if content.is_some() {
        kind
    } else {
        crate::Kind::File
    };
    if let Some(text) = &content
        && !crate::valid_special(kind, text, executable, base64)
    {
        return Err(Refusal(format!(
            "{path}: invalid symlink target or submodule commit"
        )));
    }
    Ok(Edit {
        path,
        executable: executable && content.is_some(),
        content,
        base64,
        kind,
    })
}

/// The proposal for a workspace's changes against the snapshot `message`
/// describes. `edits` must be sorted by path without duplicates.
pub fn proposal(
    repo_name: &str,
    channel: &str,
    intent: &str,
    message: &str,
    repo: &Repository,
    edits: Vec<Edit>,
) -> Result<Command, Refusal> {
    let (_, hashes) =
        parse(message).ok_or_else(|| Refusal("workspace is not an o5s projection".into()))?;
    if edits.is_empty() || edits.len() > MAX_FILES {
        return Err(Refusal(format!("1..{MAX_FILES} changed files required")));
    }
    let dependencies = dependencies(repo, channel, &hashes)?;
    let Some((graph, files)) = graph_edits(repo, &dependencies, edits.clone())? else {
        return Ok(Command::Propose {
            repo: repo_name.into(),
            intent: intent.into(),
            dependencies,
            edits,
        });
    };
    Ok(Command::ProposeGraph {
        repo: repo_name.into(),
        intent: intent.into(),
        dependencies,
        edits: graph,
        files,
    })
}

/// Graph edits for text documents, and whole-file edits for binary files,
/// symlinks, submodules and paths that hold legacy documents, against the
/// snapshot `deps`. `None` keeps a legacy repository's representation: one
/// that has whole-file documents and none of whose graph documents changed.
/// This is the rule `o5s propose` applies.
fn graph_edits(
    repo: &Repository,
    deps: &[u64],
    edits: Vec<Edit>,
) -> Result<Option<(Vec<GraphEdit>, Vec<Edit>)>, Refusal> {
    let graph_paths: BTreeSet<&str> = repo
        .patches
        .values()
        .flat_map(|p| &p.graph_edits)
        .map(|e| e.path.as_str())
        .collect();
    let legacy_paths: BTreeSet<&str> = repo
        .patches
        .values()
        .flat_map(|p| &p.edits)
        .map(|e| e.path.as_str())
        .collect();
    if !legacy_paths.is_empty() && !edits.iter().any(|e| graph_paths.contains(e.path.as_str())) {
        return Ok(None);
    }
    let mut graph = Vec::new();
    let mut files = Vec::new();
    for edit in edits {
        let path = edit.path.as_str();
        let special = edit.base64 || edit.kind != Kind::File;
        if legacy_paths.contains(path) || (!graph_paths.contains(path) && special) {
            files.push(edit);
            continue;
        }
        let Some(text) = &edit.content else {
            graph.push(GraphEdit {
                path: edit.path,
                remove: true,
                ..GraphEdit::default()
            });
            continue;
        };
        if special {
            return Err(Refusal(format!(
                "{path} is a text graph document; binary content, a symlink or a submodule needs a new path"
            )));
        }
        let operations = operations(&repo.document_at(path, deps)?, path, text)?;
        graph.push(GraphEdit {
            path: edit.path,
            operations,
            executable: edit.executable,
            ..GraphEdit::default()
        });
    }
    Ok(Some((graph, files)))
}

/// The graph operations that turn a document's snapshot into `updated`.
/// Unchanged atoms at either end are kept; the atoms between them are deleted
/// and the new text is inserted as lines, so later edits have line anchors.
fn operations(graph: &DocumentGraph, path: &str, updated: &str) -> Result<Vec<Operation>, Refusal> {
    let view = graph.materialize();
    let unambiguous = view.conflicts.is_empty() && view.layers.iter().all(|l| l.len() == 1);
    let (true, Some(rendered)) = (unambiguous, view.bytes) else {
        return Err(Refusal(format!(
            "{path} has unresolved conflicts; resolve them with explicit graph operations"
        )));
    };
    let atoms: Vec<(NodeId, &[u8])> = view
        .layers
        .iter()
        .map(|l| {
            let id = l[0];
            (id, graph.atom(id).unwrap_or_default())
        })
        .collect();
    if atoms.iter().any(|(_, b)| b.is_empty())
        || atoms.iter().flat_map(|(_, b)| b.iter()).ne(&rendered)
    {
        return Err(Refusal(format!(
            "{path}: graph atoms do not match its rendered bytes"
        )));
    }
    let target = updated.as_bytes();
    let (mut start, mut left) = (0, 0);
    while start < atoms.len() && target[left..].starts_with(atoms[start].1) {
        left += atoms[start].1.len();
        start += 1;
    }
    let (mut end, mut right) = (atoms.len(), target.len());
    while end > start
        && right >= left + atoms[end - 1].1.len()
        && target[..right].ends_with(atoms[end - 1].1)
    {
        right -= atoms[end - 1].1.len();
        end -= 1;
    }
    let mut ops = Vec::new();
    if start < end {
        ops.push(Operation::Delete {
            targets: atoms[start..end].iter().map(|(id, _)| *id).collect(),
        });
    }
    if left < right {
        let anchor = |i: usize| Anchor::Node(atoms[i].0);
        ops.push(Operation::Insert {
            left: if start == 0 {
                Anchor::Start
            } else {
                anchor(start - 1)
            },
            right: if end == atoms.len() {
                Anchor::End
            } else {
                anchor(end)
            },
            atoms: target[left..right]
                .split_inclusive(|b| *b == b'\n')
                .map(<[u8]>::to_vec)
                .collect(),
        });
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_with_and_without_generations() {
        let h = "a".repeat(64);
        let g = "b".repeat(64);
        for generation in [None, Some(0), Some(42)] {
            let m = message(generation, &[&h, &g]);
            assert_eq!(parse(&m), Some((generation, vec![h.as_str(), g.as_str()])));
            // Artifacts and `git show` drop the trailing newline.
            assert_eq!(
                parse(m.trim_end()),
                Some((generation, vec![h.as_str(), g.as_str()]))
            );
        }
        assert_eq!(parse(&message(Some(1), &[])), Some((Some(1), vec![])));
        assert_eq!(parse(message(None, &[]).trim_end()), Some((None, vec![])));
        assert_eq!(parse("unrelated"), None);
        assert_eq!(
            parse(&format!("{TITLE} (generation x)\n\nPatch hashes:\n")),
            None
        );
        assert_eq!(
            parse(&format!("{TITLE}\n\nPatch hashes:\nnot-a-hash\n")),
            None
        );
    }

    #[test]
    fn edits_keep_text_and_encode_binary() {
        use crate::Kind;
        let t = edit("a.txt".into(), Some(b"hi\n".to_vec()), true, Kind::File).unwrap();
        assert_eq!(
            (t.content.as_deref(), t.executable, t.base64),
            (Some("hi\n"), true, false)
        );
        let b = edit("b.bin".into(), Some(vec![0xff, 0]), false, Kind::File).unwrap();
        assert_eq!((b.content.as_deref(), b.base64), (Some("/wA="), true));
        let r = edit("c".into(), None, true, Kind::Symlink).unwrap();
        assert_eq!((r.content, r.executable), (None, false));
        assert!(edit("../x".into(), None, false, Kind::File).is_err());
        let l = edit(
            "l".into(),
            Some(b"../target".to_vec()),
            false,
            Kind::Symlink,
        )
        .unwrap();
        assert_eq!(l.kind, Kind::Symlink);
        assert!(edit("l".into(), Some(b"a\0b".to_vec()), false, Kind::Symlink).is_err());
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert!(
            edit(
                "m".into(),
                Some(id.as_bytes().to_vec()),
                false,
                Kind::Submodule
            )
            .is_ok()
        );
        assert!(edit("m".into(), Some(b"main".to_vec()), false, Kind::Submodule).is_err());
    }
}
