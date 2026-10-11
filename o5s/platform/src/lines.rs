//! Stable line coordinates refer to one immutable base patch, never to arrival
//! order. Derived previews preserve their base and ranges for concurrent merge.
use crate::{Edit, Refusal, Repository, refuse, valid_path};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LineEdit {
    pub path: String,
    pub base: u64,
    pub start: u64,
    pub end: u64,
    pub content: String,
}
#[derive(Clone, Debug)]
pub struct LineVersion {
    pub base: u64,
    pub original: std::sync::Arc<str>,
    pub changes: Vec<LineEdit>,
}
impl LineVersion {
    pub fn render(&self) -> Result<String, Refusal> {
        render(&self.original, &self.changes)
    }
    pub fn merge_all<'a>(mut versions: impl Iterator<Item = Option<&'a Self>>) -> Option<Self> {
        let first = versions.next()??;
        let mut changes = std::collections::BTreeMap::new();
        for version in std::iter::once(Some(first)).chain(versions) {
            let version = version?;
            if version.base != first.base
                || (!std::sync::Arc::ptr_eq(&version.original, &first.original)
                    && version.original != first.original)
            {
                return None;
            }
            for edit in &version.changes {
                let key = (edit.start, edit.end);
                if let Some(existing) = changes.get(&key) {
                    if existing == edit {
                        continue;
                    }
                    return None;
                }
                if changes
                    .range(..key)
                    .next_back()
                    .is_some_and(|(_, e)| overlaps(e, edit))
                    || changes
                        .range(key..)
                        .next()
                        .is_some_and(|(_, e)| overlaps(e, edit))
                {
                    return None;
                }
                changes.insert(key, edit.clone());
            }
        }
        Some(Self {
            base: first.base,
            original: first.original.clone(),
            changes: changes.into_values().collect(),
        })
    }
}
fn overlaps(a: &LineEdit, b: &LineEdit) -> bool {
    o5s_kernel::interval::conflict(a.start, a.end, b.start, b.end)
}
fn render(original: &str, changes: &[LineEdit]) -> Result<String, Refusal> {
    let lines: Vec<_> = original.split_inclusive('\n').collect();
    let mut boundaries = vec![0usize];
    for line in &lines {
        boundaries.push(boundaries.last().unwrap() + line.len());
    }
    let mut cursor = 0;
    for e in changes {
        let start = usize::try_from(e.start).map_err(|_| refuse("invalid line range"))?;
        let end = usize::try_from(e.end).map_err(|_| refuse("invalid line range"))?;
        if start < cursor || end < start || end > lines.len() {
            return Err(refuse("invalid or overlapping line range"));
        }
        cursor = end;
    }
    let original = original.as_bytes().to_vec();
    let chunks: Vec<_> = changes
        .iter()
        .map(|e| {
            (
                boundaries[e.start as usize],
                boundaries[e.end as usize],
                e.content.as_bytes().to_vec(),
            )
        })
        .collect();
    let _capacity = chunks
        .iter()
        .try_fold(original.len(), |n, (_, _, text)| n.checked_add(text.len()))
        .ok_or_else(|| refuse("materialization capacity exceeded"))?;
    let bytes = o5s_kernel::interval::render_chunks(&original, &chunks);
    String::from_utf8(bytes).map_err(|_| refuse("invalid UTF-8 materialization"))
}
impl Repository {
    pub fn line_version(&self, path: &str, changes: &[LineEdit]) -> Result<LineVersion, Refusal> {
        self.line_version_shared(path, changes, None)
    }
    pub(crate) fn line_version_shared(
        &self,
        path: &str,
        changes: &[LineEdit],
        shared: Option<std::sync::Arc<str>>,
    ) -> Result<LineVersion, Refusal> {
        let first = changes.first().ok_or_else(|| refuse("empty line edits"))?;
        let base = self
            .patches
            .get(&first.base)
            .ok_or_else(|| refuse("line base not found"))?;
        let original = shared.map(Ok).unwrap_or_else(|| {
            base.edits
                .iter()
                .find(|e| e.path == path)
                .and_then(|e| e.content.as_deref())
                .map(std::sync::Arc::<str>::from)
                .ok_or_else(|| refuse("line base file not found"))
        })?;
        if changes
            .iter()
            .any(|e| e.base != first.base || e.path != path)
        {
            return Err(refuse("one base per edited file required"));
        }
        Ok(LineVersion {
            base: first.base,
            original,
            changes: changes.to_vec(),
        })
    }
    pub fn prepare_lines(
        &self,
        changes: &mut [LineEdit],
        deps: &mut Vec<u64>,
    ) -> Result<Vec<Edit>, Refusal> {
        if changes.is_empty() || changes.len() > 1024 {
            return Err(refuse("1..1024 line edits required"));
        }
        changes.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
        if changes
            .iter()
            .any(|e| !valid_path(&e.path) || e.start > e.end)
        {
            return Err(refuse("invalid line edit"));
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < changes.len() {
            let end = i + changes[i..].partition_point(|e| e.path == changes[i].path);
            let group = &changes[i..end];
            if group.windows(2).any(|es| overlaps(&es[0], &es[1])) {
                return Err(refuse("overlapping line edits"));
            }
            let version = self.line_version(&group[0].path, group)?;
            deps.push(version.base);
            out.push(Edit {
                path: group[0].path.clone(),
                content: Some(version.render()?),
            });
            i = end;
        }
        deps.sort_unstable();
        deps.dedup();
        // A base is an exact causal snapshot. Never discard earlier edits by
        // accepting ranges against an obsolete base in the same dependency view.
        let mut active = std::collections::BTreeSet::new();
        let mut stack = deps.clone();
        while let Some(id) = stack.pop() {
            let patch = self
                .patches
                .get(&id)
                .ok_or_else(|| refuse("unknown dependency"))?;
            if active.insert(id) {
                stack.extend(patch.dependencies.iter().copied());
            }
        }
        for edit in &out {
            let mut heads = std::collections::BTreeSet::new();
            for id in &active {
                let patch = &self.patches[id];
                if patch.edits.iter().any(|e| e.path == edit.path) {
                    // Search only for current heads, stopping once all have been
                    // found. A linear edit history examines one edge per patch.
                    let mut targets = heads.clone();
                    let min = targets.first().copied().unwrap_or(u64::MAX);
                    let mut visited = std::collections::BTreeSet::new();
                    let mut stack = patch.dependencies.clone();
                    while !targets.is_empty() {
                        let Some(d) = stack.pop() else { break };
                        if d < min || !visited.insert(d) {
                            continue;
                        }
                        if targets.remove(&d) {
                            heads.remove(&d);
                        }
                        if !targets.is_empty() {
                            stack.extend(self.patches[&d].dependencies.iter().copied());
                        }
                    }
                    heads.insert(*id);
                }
            }
            let base = changes.iter().find(|e| e.path == edit.path).unwrap().base;
            if heads != std::collections::BTreeSet::from([base]) {
                return Err(refuse(
                    "line base must be the sole causal file head; resolve or choose the latest base",
                ));
            }
        }
        Ok(out)
    }
}
