//! Persisted graph operations use the same kernel document materializer on all
//! channel, API and Git export paths. Native envelope validation is not proved.
use crate::{Alternative, Edit, Patch, Refusal, Repository, valid_path};
use o5s_kernel::model::{DocumentGraph, Operation, PatchId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdit {
    pub path: String,
    pub operations: Vec<Operation>,
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
        if patch.graph_edits.iter().any(|edit| edit.path == path) {
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
    pub fn reject_graph_legacy_mix(&self, edits: &[Edit]) -> Result<(), Refusal> {
        if edits.iter().any(|edit| {
            self.patches
                .values()
                .any(|p| p.graph_edits.iter().any(|e| e.path == edit.path))
        }) {
            return Err(Refusal("graph document requires graph operations".into()));
        }
        Ok(())
    }

    pub fn validate_graph_edits(
        &self,
        deps: &[u64],
        edits: &[GraphEdit],
        hash: &str,
    ) -> Result<(), Refusal> {
        let mut paths = BTreeSet::new();
        for edit in edits {
            if !valid_path(&edit.path)
                || !paths.insert(&edit.path)
                || edit.operations.is_empty()
                || edit.operations.len() > 4096
            {
                return Err(Refusal("invalid graph edit path or operation count".into()));
            }
            if self
                .patches
                .values()
                .any(|p| p.edits.iter().any(|e| e.path == edit.path))
            {
                return Err(Refusal(
                    "legacy document requires explicit graph migration".into(),
                ));
            }
            for op in &edit.operations {
                if let Operation::Insert { atoms, .. } = op {
                    if atoms.iter().any(|atom| std::str::from_utf8(atom).is_err()) {
                        return Err(Refusal("graph text atoms must be UTF-8".into()));
                    }
                }
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

pub(crate) fn render_output(
    graph: &DocumentGraph,
    patches: &[u64],
) -> Result<crate::PathOutput, Refusal> {
    let materialized = graph.materialize();
    if let Some(bytes) = materialized.bytes {
        let text =
            String::from_utf8(bytes).map_err(|_| Refusal("graph document is not UTF-8".into()))?;
        Ok(crate::PathOutput::File(text))
    } else {
        Ok(crate::PathOutput::Conflict(
            patches
                .iter()
                .map(|id| Alternative {
                    patch: *id,
                    content: None,
                })
                .collect(),
        ))
    }
}

pub(crate) fn overlay_outputs(
    outputs: &BTreeMap<String, crate::PathOutput>,
    patches: &BTreeMap<String, Vec<u64>>,
    files: &mut BTreeMap<String, String>,
    conflicts: &mut BTreeMap<String, Vec<Alternative>>,
) {
    for (path, output) in outputs {
        match output {
            crate::PathOutput::File(text) => {
                files.insert(path.clone(), text.clone());
            }
            crate::PathOutput::Conflict(alternatives) => {
                files.remove(path);
                conflicts.insert(path.clone(), alternatives.clone());
            }
            crate::PathOutput::Deleted => {
                files.remove(path);
            }
        }
    }
    let paths: Vec<_> = files.keys().cloned().collect();
    let mut collisions = BTreeSet::new();
    for path in &paths {
        for (i, c) in path.char_indices() {
            if c == '/' && files.contains_key(&path[..i]) {
                collisions.insert(path.clone());
                collisions.insert(path[..i].to_string());
            }
        }
    }
    for path in collisions {
        let content = files.remove(&path);
        let ids = patches.get(&path).cloned().unwrap_or_default();
        conflicts.insert(
            path,
            ids.into_iter()
                .map(|patch| Alternative {
                    patch,
                    content: content.clone(),
                })
                .collect(),
        );
    }
}
