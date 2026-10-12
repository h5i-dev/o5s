//! Persisted graph operations use the same kernel document materializer on all
//! channel, API and Git export paths. Native envelope validation is not proved.
use crate::{Alternative, Patch, Refusal, Repository, valid_path};
use o5s_kernel::model::{DocumentGraph, Operation, PatchId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// The document was at this path in the proposal's snapshot and moves to
    /// `path`, keeping its identity: concurrent edits of the old path apply
    /// to it at the new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Whether a graph document exists and is executable, as one register value.
pub type FileState = (bool, bool);

/// A graph document's identity: the path it was created at, or for a path
/// created again after its document moved away, that path and the creating
/// patch. Edits name paths as their author saw them; `Docs` resolves them.
pub type DocId = String;

/// The document each graph edit is about, resolved through its patch's
/// causal past and memoized by `(patch, path)`, with whether the edit writes
/// the document's location. Resolve patches in ascending handle order
/// (`prime`), so every dependency is resolved first.
///
/// A document's location is a register of its own, written when the
/// document is created, moved (`from`), or edited by a patch that sees its
/// location in conflict. Ordinary edits leave it alone, so an edit of the
/// old path concurrent with a move follows the document.
#[derive(Clone, Debug, Default)]
pub struct Docs {
    memo: HashMap<(u64, String), (DocId, bool)>,
    /// Every patch below this handle has been resolved.
    primed: u64,
}

/// The document identities a repository view has resolved, shared with its
/// clones: an entry describes a committed patch, whose handle never names
/// another patch, so a later view of the same repository reuses it and
/// resolves only the patches committed since. Without it every proposal and
/// document read resolved the whole history again.
#[derive(Clone, Default)]
pub(crate) struct Memo(std::sync::Arc<std::sync::Mutex<Docs>>);

impl PartialEq for Memo {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Memo {}
impl std::fmt::Debug for Memo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Memo")
    }
}

impl Docs {
    /// The document of patch `id`'s edit at `path`, once resolved.
    pub fn get(&self, id: u64, path: &str) -> Option<&DocId> {
        self.memo.get(&(id, path.to_string())).map(|(d, _)| d)
    }

    /// Whether patch `id`'s edit at `path` writes its document's location.
    pub fn moves(&self, id: u64, path: &str) -> bool {
        self.memo
            .get(&(id, path.to_string()))
            .is_some_and(|(_, w)| *w)
    }

    /// Resolve every graph edit of patches up to `upto`, in handle order.
    pub fn prime(&mut self, repo: &Repository, upto: u64) -> Result<(), Refusal> {
        if upto < self.primed {
            return Ok(());
        }
        let ids: Vec<u64> = repo
            .patches
            .range(self.primed..=upto)
            .filter(|(_, p)| !p.graph_edits.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let p = &repo.patches[&id];
            for e in &p.graph_edits {
                let source = e.from.as_deref().unwrap_or(&e.path);
                let (doc, created) = self.resolve(repo, &p.dependencies, source, &p.hash)?;
                let settles = self
                    .locations(repo, &p.dependencies, &doc)?
                    .values()
                    .collect::<BTreeSet<_>>()
                    .len()
                    > 1;
                let writes = created || e.from.is_some() || settles;
                self.memo.insert((id, e.path.clone()), (doc, writes));
            }
        }
        self.primed = self.primed.max(upto.saturating_add(1));
        Ok(())
    }

    /// The location heads of `doc` in the causal past of `deps`: the nearest
    /// patches that wrote its location, and the path each wrote.
    pub fn locations(
        &self,
        repo: &Repository,
        deps: &[u64],
        doc: &str,
    ) -> Result<BTreeMap<u64, String>, Refusal> {
        let writes = |p: &Patch| {
            p.graph_edits.iter().any(|e| {
                self.moves(p.id, &e.path) && self.get(p.id, &e.path).is_some_and(|d| d == doc)
            })
        };
        let mut out = BTreeMap::new();
        for y in nearest_by(repo, deps, writes)? {
            for e in &repo.patches[&y].graph_edits {
                if self.moves(y, &e.path) && self.get(y, &e.path).is_some_and(|d| d == doc) {
                    out.insert(y, e.path.clone());
                }
            }
        }
        Ok(out)
    }

    /// The document at `path` in the causal past of `deps`, and whether it
    /// is new: then it is created by the patch with hash `creator`.
    pub fn resolve(
        &self,
        repo: &Repository,
        deps: &[u64],
        path: &str,
        creator: &str,
    ) -> Result<(DocId, bool), Refusal> {
        let touches = |p: &Patch| {
            p.graph_edits
                .iter()
                .any(|e| e.path == path || e.from.as_deref() == Some(path))
        };
        let near = nearest_by(repo, deps, touches)?;
        let mut candidates = BTreeSet::new();
        for y in &near {
            for e in &repo.patches[y].graph_edits {
                if e.path == path {
                    let d = self.get(*y, path).ok_or_else(|| {
                        Refusal("graph document resolved out of causal order".into())
                    })?;
                    candidates.insert(d.clone());
                }
            }
        }
        // A document is at `path` if one of its location heads says so.
        let mut here = Vec::new();
        for d in candidates {
            if self.locations(repo, deps, &d)?.values().any(|p| p == path) {
                here.push(d);
            }
        }
        match here.len() {
            1 => Ok((here.pop().unwrap(), false)),
            0 if near.is_empty() => Ok((path.to_string(), true)),
            0 => Ok((format!("{path}#{creator}"), true)),
            _ => Err(Refusal(format!(
                "{path}: two documents are at this path concurrently; resolve the move first"
            ))),
        }
    }
}

/// Dense dependency rows of a repository: row `h` lists patch `h`'s
/// dependencies (handles are dense and every dependency is older).
pub(crate) fn rows(repo: &Repository) -> Vec<Vec<usize>> {
    let n = repo
        .patches
        .keys()
        .next_back()
        .map_or(0, |h| *h as usize + 1);
    let mut rows = vec![Vec::new(); n];
    for (h, p) in &repo.patches {
        rows[*h as usize] = p.dependencies.iter().map(|d| *d as usize).collect();
    }
    rows
}

/// The nearest patches satisfying `stop` on each causal path from `initial`
/// (the extracted `causality::frontier`), in ascending order.
fn nearest_by(
    repo: &Repository,
    initial: &[u64],
    stop: impl Fn(&Patch) -> bool,
) -> Result<Vec<u64>, Refusal> {
    let rows = rows(repo);
    if initial.iter().any(|h| !repo.patches.contains_key(h)) {
        return Err(Refusal("unknown graph dependency".into()));
    }
    let stops = (0..rows.len())
        .map(|h| repo.patches.get(&(h as u64)).is_some_and(&stop))
        .collect();
    let roots = initial.iter().map(|h| *h as usize).collect();
    let found = o5s_kernel::causality::frontier(&rows, &stops, &roots, vec![false; rows.len()]);
    let mut out: Vec<u64> = found.queue.into_iter().map(|h| h as u64).collect();
    out.sort_unstable();
    Ok(out)
}

/// Whether patch `p` edits document `doc` (with operations, if `ops`).
fn edits_doc(docs: &Docs, p: &Patch, doc: &str, ops: bool) -> bool {
    p.graph_edits.iter().any(|e| {
        (!ops || !e.operations.is_empty()) && docs.get(p.id, &e.path).is_some_and(|d| d == doc)
    })
}

/// The handles of the nearest patches editing document `doc` on each causal
/// path from `initial`.
pub fn nearest(
    repo: &Repository,
    docs: &Docs,
    initial: &[u64],
    doc: &str,
) -> Result<Vec<u64>, Refusal> {
    nearest_by(repo, initial, |p| edits_doc(docs, p, doc, false))
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
// dependencies carry the rest of that path's observed history. Edits without
// operations (empty files, removal, mode) add nothing to the graph; their
// causal past carries its history.
fn dependencies(
    repo: &Repository,
    docs: &Docs,
    initial: &[u64],
    doc: &str,
) -> Result<Vec<PatchId>, Refusal> {
    let near = nearest_by(repo, initial, |p| edits_doc(docs, p, doc, true))?;
    let mut ids = near
        .iter()
        .map(|h| identity(&repo.patches[h].hash))
        .collect::<Result<Vec<_>, _>>()?;
    ids.sort();
    Ok(ids)
}

pub fn apply_graph_patch(
    graph: &mut DocumentGraph,
    repo: &Repository,
    docs: &Docs,
    patch: &Patch,
    edit: &GraphEdit,
) -> Result<(), Refusal> {
    if edit.operations.is_empty() {
        return Ok(());
    }
    let doc = docs
        .get(patch.id, &edit.path)
        .ok_or_else(|| Refusal("graph document not resolved".into()))?;
    graph
        .apply(o5s_kernel::model::Patch {
            id: identity(&patch.hash)?,
            dependencies: dependencies(repo, docs, &patch.dependencies, doc)?,
            operations: edit.operations.clone(),
        })
        .map_err(|error| Refusal(format!("invalid document operation: {error:?}")))?;
    Ok(())
}

impl Repository {
    /// Document identities resolved for every patch up to `top`, from this
    /// view's memo.
    fn docs(&self, top: Option<u64>) -> Result<Docs, Refusal> {
        let mut docs = self.docs.0.lock().unwrap();
        if let Some(top) = top {
            docs.prime(self, top)?;
        }
        Ok(docs.clone())
    }

    pub fn validate_graph_edits(
        &self,
        deps: &[u64],
        edits: &[GraphEdit],
        hash: &str,
    ) -> Result<(), Refusal> {
        let mut paths = BTreeSet::new();
        let docs = self.docs(deps.iter().max().copied())?;
        let mut moved = BTreeSet::new();
        let edited: BTreeSet<&String> = edits.iter().map(|e| &e.path).collect();
        if edits
            .iter()
            .any(|e| e.from.as_ref().is_some_and(|f| edited.contains(f)))
        {
            return Err(Refusal(
                "a moved document's source cannot also be edited".into(),
            ));
        }
        for edit in edits {
            // An edit without operations only creates an empty file, removes
            // one or changes its mode.
            if !valid_path(&edit.path) || !paths.insert(&edit.path) || edit.operations.len() > 4096
            {
                return Err(Refusal("invalid graph edit path or operation count".into()));
            }
            // A move needs a document at its source, which nothing else in
            // the proposal touches.
            let source = edit.from.as_deref().unwrap_or(&edit.path);
            let (doc, _) = docs.resolve(self, deps, source, hash)?;
            if let Some(from) = &edit.from {
                if !valid_path(from)
                    || from == &edit.path
                    || !moved.insert(from.clone())
                    || nearest(self, &docs, deps, &doc)?.is_empty()
                {
                    return Err(Refusal(format!(
                        "{from}: a move needs a graph document at its source"
                    )));
                }
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
            let mut graph = self.document_at(source, deps)?;
            graph
                .apply(o5s_kernel::model::Patch {
                    id: identity(hash)?,
                    dependencies: dependencies(self, &docs, deps, &doc)?,
                    operations: edit.operations.clone(),
                })
                .map_err(|error| Refusal(format!("invalid document operation: {error:?}")))?;
        }
        Ok(())
    }

    /// Replay only the caller's causal view. Unobserved anchors cannot become
    /// valid merely because another agent has submitted them to the registry.
    /// `path` names the document as the snapshot `tips` sees it.
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
        let docs = self.docs(observed.last().copied())?;
        let (doc, _) = docs.resolve(self, tips, path, "")?;
        let mut graph = DocumentGraph::default();
        for id in observed {
            let patch = &self.patches[&id];
            for edit in &patch.graph_edits {
                if docs.get(id, &edit.path) == Some(&doc) {
                    apply_graph_patch(&mut graph, self, &docs, patch, edit)?;
                }
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
                kind: crate::Kind::File,
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
                kind: crate::Kind::File,
            })),
            None => Ok(crate::PathOutput::Conflict(alternatives())),
        },
        _ => Ok(crate::PathOutput::Conflict(alternatives())),
    }
}

/// What one graph document shows: its output at its location, or, when its
/// location heads disagree (concurrent moves), a conflict at every path a
/// head names.
pub(crate) fn render_document(
    graph: &DocumentGraph,
    heads: &BTreeMap<u64, FileState>,
    locations: &BTreeMap<u64, String>,
) -> Result<Vec<(String, crate::PathOutput)>, Refusal> {
    let paths: BTreeSet<&String> = locations.values().collect();
    if paths.len() == 1 {
        let path = (*paths.first().unwrap()).clone();
        return Ok(vec![(path, render_output(graph, heads)?)]);
    }
    let text = graph
        .materialize()
        .bytes
        .and_then(|b| String::from_utf8(b).ok());
    let executable = heads.values().any(|(_, x)| *x);
    Ok(paths
        .into_iter()
        .map(|path| {
            let alternatives = locations
                .iter()
                .map(|(patch, at)| Alternative {
                    patch: *patch,
                    content: if at == path { text.clone() } else { None },
                    executable,
                    base64: false,
                    kind: crate::Kind::File,
                })
                .collect();
            (path.clone(), crate::PathOutput::Conflict(alternatives))
        })
        .collect())
}

/// Combine the documents' outputs by path. Two documents with output at one
/// path (a move onto a path re-created concurrently) conflict there.
pub(crate) fn by_path(
    outputs: &BTreeMap<DocId, Vec<(String, crate::PathOutput)>>,
    heads: &BTreeMap<DocId, BTreeMap<u64, FileState>>,
) -> (
    BTreeMap<String, crate::PathOutput>,
    BTreeMap<String, BTreeMap<u64, FileState>>,
) {
    let mut out: BTreeMap<String, crate::PathOutput> = BTreeMap::new();
    let mut by: BTreeMap<String, BTreeMap<u64, FileState>> = BTreeMap::new();
    for (doc, rendered) in outputs {
        for (path, _) in rendered {
            by.entry(path.clone())
                .or_default()
                .extend(heads[doc].iter().map(|(id, st)| (*id, *st)));
        }
        for (path, o) in rendered {
            let merged = match (out.remove(path), o) {
                (None, o) | (Some(crate::PathOutput::Deleted), o) => o.clone(),
                (Some(prev), crate::PathOutput::Deleted) => prev,
                (Some(prev), o) => {
                    let alternatives =
                        |x: &crate::PathOutput, ids: &BTreeMap<u64, FileState>| match x {
                            crate::PathOutput::Conflict(a) => a.clone(),
                            crate::PathOutput::File(c) => ids
                                .keys()
                                .map(|patch| Alternative::of(*patch, &Some(c.clone())))
                                .collect(),
                            crate::PathOutput::Deleted => Vec::new(),
                        };
                    let mut all = alternatives(&prev, &by[path]);
                    all.extend(alternatives(o, &by[path]));
                    all.sort_by_key(|a| a.patch);
                    all.dedup();
                    crate::PathOutput::Conflict(all)
                }
            };
            out.insert(path.clone(), merged);
        }
    }
    (out, by)
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
                    kind: crate::Kind::File,
                })
                .collect(),
        );
    }
}
