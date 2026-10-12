//! Agent workspaces: Git snapshots of an admitted channel that an agent edits
//! with ordinary Git, and the proposal its changes become. The native server
//! keeps workspaces as local worktrees; the Worker keeps them as Artifacts
//! repositories. Both read the snapshot's patch set from its commit message
//! and turn changed files into one whole-file proposal through this module.
use crate::{Command, Edit, Refusal, Repository, base64_encode, valid_path};

/// The first line of every projection commit message.
pub const TITLE: &str = "o5s verified-admission projection";

/// At most this many changed files in one workspace proposal.
pub const MAX_FILES: usize = 1024;
/// At most this many bytes per file and in total.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The message of a projection commit: optionally its registry generation,
/// then the hashes of the admitted patches in channel order.
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

/// The registry handles of a snapshot's patches, each of which must be a
/// member of `channel`: a workspace proposal depends on exactly what it saw.
pub fn dependencies(
    repo: &Repository,
    channel: &str,
    hashes: &[&str],
) -> Result<Vec<u64>, Refusal> {
    let members = repo
        .channels
        .get(channel)
        .ok_or_else(|| Refusal("channel not found".into()))?;
    hashes
        .iter()
        .map(|h| {
            let id = repo
                .patches
                .values()
                .find(|p| p.hash == *h)
                .map(|p| p.id)
                .ok_or_else(|| Refusal("workspace patch identity not found".into()))?;
            if !members.contains(&id) {
                return Err(Refusal("workspace snapshot is outside this channel".into()));
            }
            Ok(id)
        })
        .collect()
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
    Ok(Command::Propose {
        repo: repo_name.into(),
        intent: intent.into(),
        dependencies: dependencies(repo, channel, &hashes)?,
        edits,
    })
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
