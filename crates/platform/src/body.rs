//! Canonical patch bodies. A body is the exact byte string whose SHA-256 is
//! the patch identity: authorship, intent, dependency identities and edits.
//! Dependencies and line bases are named by identity, never by local handle.
use crate::{Edit, Refusal, Repository, documents::GraphEdit, lines::LineEdit, refuse};
use sha2::{Digest, Sha256};

const FILES: &str = "o5s-patch-v1";
const LINES: &str = "o5s-line-patch-v1";
const GRAPH: &str = "o5s-graph-patch-v1";
/// Graph edits and whole-file edits in one patch.
const MIXED: &str = "o5s-mixed-patch-v1";

/// Lowercase hexadecimal SHA-256.
pub fn hash(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn identity(repo: &Repository, id: u64) -> Result<&str, Refusal> {
    repo.patches
        .get(&id)
        .map(|p| p.hash.as_str())
        .ok_or_else(|| refuse("unknown dependency"))
}

/// The canonical body. Dependencies must already be sorted and known.
pub fn encode(
    author: u64,
    intent: &str,
    dependencies: &[u64],
    edits: &[Edit],
    line_edits: &[LineEdit],
    graph_edits: &[GraphEdit],
    repo: &Repository,
) -> Result<Vec<u8>, Refusal> {
    let mut deps = dependencies
        .iter()
        .map(|id| identity(repo, *id))
        .collect::<Result<Vec<&str>, _>>()?;
    deps.sort_unstable();
    let encoded = if !graph_edits.is_empty() && !edits.is_empty() {
        if !line_edits.is_empty() {
            return Err(refuse("line edits cannot accompany graph edits"));
        }
        serde_json::to_vec(&(MIXED, author, intent, deps, edits, graph_edits))
    } else if !graph_edits.is_empty() {
        serde_json::to_vec(&(GRAPH, author, intent, deps, graph_edits))
    } else if line_edits.is_empty() {
        serde_json::to_vec(&(FILES, author, intent, deps, edits))
    } else {
        let ranges = line_edits
            .iter()
            .map(|e| Ok((&e.path, identity(repo, e.base)?, e.start, e.end, &e.content)))
            .collect::<Result<Vec<_>, Refusal>>()?;
        serde_json::to_vec(&(LINES, author, intent, deps, edits, ranges))
    };
    encoded.map_err(|e| Refusal(e.to_string()))
}

/// The edits a body records.
pub struct Decoded {
    pub intent: String,
    pub edits: Vec<Edit>,
    pub line_edits: Vec<LineEdit>,
    pub graph_edits: Vec<GraphEdit>,
}

/// Read a body back; line bases resolve to handles through `repo`.
pub fn decode(body: &[u8], repo: &Repository) -> Result<Decoded, Refusal> {
    let corrupt = |_| refuse("corrupt patch body");
    let fields: Vec<serde_json::Value> = serde_json::from_slice(body).map_err(corrupt)?;
    match fields.first().and_then(|v| v.as_str()) {
        Some(GRAPH) => {
            let (_, _, intent, _, graph_edits): (String, u64, String, Vec<String>, Vec<GraphEdit>) =
                serde_json::from_slice(body).map_err(corrupt)?;
            Ok(Decoded {
                intent,
                edits: Vec::new(),
                line_edits: Vec::new(),
                graph_edits,
            })
        }
        Some(MIXED) => {
            let (_, _, intent, _, edits, graph_edits): (
                String,
                u64,
                String,
                Vec<String>,
                Vec<Edit>,
                Vec<GraphEdit>,
            ) = serde_json::from_slice(body).map_err(corrupt)?;
            Ok(Decoded {
                intent,
                edits,
                line_edits: Vec::new(),
                graph_edits,
            })
        }
        Some(FILES) => {
            let (_, _, intent, _, edits): (String, u64, String, Vec<String>, Vec<Edit>) =
                serde_json::from_slice(body).map_err(corrupt)?;
            Ok(Decoded {
                intent,
                edits,
                line_edits: Vec::new(),
                graph_edits: Vec::new(),
            })
        }
        Some(LINES) => {
            type Range = (String, String, u64, u64, String);
            let (_, _, intent, _, edits, ranges): (
                String,
                u64,
                String,
                Vec<String>,
                Vec<Edit>,
                Vec<Range>,
            ) = serde_json::from_slice(body).map_err(corrupt)?;
            let line_edits = ranges
                .into_iter()
                .map(|(path, base, start, end, content)| {
                    let base = repo
                        .handle(&base)
                        .ok_or_else(|| refuse("line base not found"))?;
                    Ok(LineEdit {
                        path,
                        base,
                        start,
                        end,
                        content,
                    })
                })
                .collect::<Result<Vec<_>, Refusal>>()?;
            Ok(Decoded {
                intent,
                edits,
                line_edits,
                graph_edits: Vec::new(),
            })
        }
        _ => Err(refuse("unknown patch body version")),
    }
}
