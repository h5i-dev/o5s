//! Persisted graph operations use the same kernel document materializer on all
//! channel, API and Git export paths. Native envelope validation is not proved.
use crate::{Alternative, Patch, Refusal, Repository, valid_path};
use o5s_kernel::model::{DocumentGraph, Operation, PatchId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdit {
    pub path: String,
    pub operations: Vec<Operation>,
    /// The file is removed. Its graph remains, so a later edit can restore it.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub remove: bool,
    /// Git mode 100755. Every graph edit states the file's mode.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub executable: bool,
}

/// Whether a graph document exists and is executable, as one register value.
pub type FileState = (bool, bool);

/// The handles of the nearest patches editing `path` on each causal path from `initial`.
pub fn nearest(repo: &Repository, initial: &[u64], path: &str) -> Result<Vec<u64>, Refusal> {
    let mut pending = initial.to_vec();
    let mut visited = BTreeSet::new();
    let mut result = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let patch = repo
            .patches
            .get(&id)
            .ok_or_else(|| Refusal("unknown graph dependency".into()))?;
        if patch.graph_edits.iter().any(|edit| edit.path == path) {
            result.insert(id);
        } else {
            pending.extend(patch.dependencies.iter().copied());
        }
    }
    Ok(result.into_iter().collect())
}

pub fn identity(hash: &str) -> Result<PatchId, Refusal> {
    if hash.len() != 64 || !hash.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(Refusal("invalid graph patch hash".into()));
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16)
            .map_err(|_| Refusal("invalid graph patch hash".into()))?;
    }
    Ok(PatchId(bytes))
}

// Stop at the first operation patch on each causal path. Its own graph
// dependencies carry the rest of that path's observed history.
fn dependencies(repo: &Repository, initial: &[u64], path: &str) -> Result<Vec<PatchId>, Refusal> {
    let mut pending = initial.to_vec();
    let mut visited = BTreeSet::new();
    let mut result = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let patch = repo
            .patches
            .get(&id)
            .ok_or_else(|| Refusal("unknown graph dependency".into()))?;
        // Edits without operations (empty files, removal, mode) add nothing
        // to the graph; their causal past carries its history.
        if patch
            .graph_edits
            .iter()
            .any(|edit| edit.path == path && !edit.operations.is_empty())
        {
            result.insert(identity(&patch.hash)?);
        } else {
            pending.extend(patch.dependencies.iter().copied());
        }
    }
    Ok(result.into_iter().collect())
}

pub fn apply_graph_patch(
    graph: &mut DocumentGraph,
    repo: &Repository,
    patch: &Patch,
    edit: &GraphEdit,
) -> Result<(), Refusal> {
    if edit.operations.is_empty() {
        return Ok(());
    }
    graph
        .apply(o5s_kernel::model::Patch {
            id: identity(&patch.hash)?,
            dependencies: dependencies(repo, &patch.dependencies, &edit.path)?,
            operations: edit.operations.clone(),
        })
        .map_err(|error| Refusal(format!("invalid document operation: {error:?}")))?;
    Ok(())
}

impl Repository {
    pub fn validate_graph_edits(
        &self,
        deps: &[u64],
        edits: &[GraphEdit],
        hash: &str,
    ) -> Result<(), Refusal> {
        let mut paths = BTreeSet::new();
        for edit in edits {
            // An edit without operations only creates an empty file, removes
            // one or changes its mode.
            if !valid_path(&edit.path) || !paths.insert(&edit.path) || edit.operations.len() > 4096
            {
                return Err(Refusal("invalid graph edit path or operation count".into()));
            }
            for op in &edit.operations {
                if let Operation::Insert { atoms, .. } = op {
                    if atoms.iter().any(|atom| std::str::from_utf8(atom).is_err()) {
                        return Err(Refusal("graph text atoms must be UTF-8".into()));
                    }
                }
            }
            if edit.operations.is_empty() {
                continue;
            }
            let mut graph = self.document_at(&edit.path, deps)?;
            graph
                .apply(o5s_kernel::model::Patch {
                    id: identity(hash)?,
                    dependencies: dependencies(self, deps, &edit.path)?,
                    operations: edit.operations.clone(),
                })
                .map_err(|error| Refusal(format!("invalid document operation: {error:?}")))?;
        }
        Ok(())
    }

    /// Replay only the caller's causal view. Unobserved anchors cannot become
    /// valid merely because another agent has submitted them to the registry.
    pub fn document_at(&self, path: &str, tips: &[u64]) -> Result<DocumentGraph, Refusal> {
        let mut pending = tips.to_vec();
        let mut observed = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !observed.insert(id) {
                continue;
            }
            let patch = self
                .patches
                .get(&id)
                .ok_or_else(|| Refusal("unknown graph dependency".into()))?;
            if patch.dependencies.iter().any(|dep| *dep >= id) {
                return Err(Refusal("corrupt causal registry".into()));
            }
            pending.extend(patch.dependencies.iter().copied());
        }
        let mut graph = DocumentGraph::default();
        for id in observed {
            let patch = &self.patches[&id];
            for edit in patch.graph_edits.iter().filter(|edit| edit.path == path) {
                apply_graph_patch(&mut graph, self, patch, edit)?;
            }
        }
        Ok(graph)
    }
}

/// A graph document's output: its file-state heads must agree; a removed file
/// has no output; otherwise the graph must materialize unambiguously.
pub(crate) fn render_output(
    graph: &DocumentGraph,
    heads: &BTreeMap<u64, FileState>,
) -> Result<crate::PathOutput, Refusal> {
    let states: BTreeSet<FileState> = heads.values().copied().collect();
    let text = match graph.materialize().bytes {
        Some(bytes) => Some(
            String::from_utf8(bytes).map_err(|_| Refusal("graph document is not UTF-8".into()))?,
        ),
        None => None,
    };
    let alternatives = || {
        heads
            .iter()
            .map(|(patch, (present, executable))| Alternative {
                patch: *patch,
                content: if *present { text.clone() } else { None },
                executable: *executable,
                base64: false,
            })
            .collect()
    };
    match (states.len(), states.first()) {
        (1, Some((false, _))) => Ok(crate::PathOutput::Deleted),
        (1, Some((true, executable))) => match &text {
            Some(text) => Ok(crate::PathOutput::File(crate::Content {
                text: text.clone(),
                executable: *executable,
                base64: false,
            })),
            None => Ok(crate::PathOutput::Conflict(alternatives())),
        },
        _ => Ok(crate::PathOutput::Conflict(alternatives())),
    }
}

/// Graph document outputs replace whole-file outputs of the same path, and
/// a file cannot also be a directory.
pub(crate) fn overlay_outputs(
    outputs: &BTreeMap<String, crate::PathOutput>,
    heads: &BTreeMap<String, BTreeMap<u64, FileState>>,
    view: &mut crate::View,
) {
    for (path, output) in outputs {
        match output {
            crate::PathOutput::File(content) => view.insert_file(path, content),
            crate::PathOutput::Conflict(alternatives) => {
                view.remove_file(path);
                view.conflicts.insert(path.clone(), alternatives.clone());
            }
            crate::PathOutput::Deleted => {
                view.remove_file(path);
            }
        }
    }
    let paths: Vec<_> = view.files.keys().cloned().collect();
    let mut collisions = BTreeSet::new();
    for path in &paths {
        for (i, c) in path.char_indices() {
            if c == '/' && view.files.contains_key(&path[..i]) {
                collisions.insert(path.clone());
                collisions.insert(path[..i].to_string());
            }
        }
    }
    for path in collisions {
        let executable = view.executable.contains(&path);
        let base64 = view.binary.contains(&path);
        let content = view.remove_file(&path);
        let ids: Vec<u64> = heads
            .get(&path)
            .map(|h| h.keys().copied().collect())
            .unwrap_or_default();
        view.conflicts.insert(
            path,
            ids.into_iter()
                .map(|patch| Alternative {
                    patch,
                    content: content.clone(),
                    executable,
                    base64,
                })
                .collect(),
        );
    }
}
