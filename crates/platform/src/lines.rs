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
    /// Merge concurrent versions of one base: all their ranges, sorted by
    /// coordinates, go through the extracted `lines::merge`, which keeps
    /// identical edits once and refuses conflicting ones. The result does not
    /// depend on the order of `versions` (`LinesModel.merge_order_independent`).
    pub fn merge_all<'a>(mut versions: impl Iterator<Item = Option<&'a Self>>) -> Option<Self> {
        let first = versions.next()??;
        let mut all: Vec<&LineEdit> = first.changes.iter().collect();
        for version in versions {
            let version = version?;
            if version.base != first.base
                || (!std::sync::Arc::ptr_eq(&version.original, &first.original)
                    && version.original != first.original)
            {
                return None;
            }
            all.extend(version.changes.iter());
        }
        // Equal coordinates sort by content so the kernel sees identical
        // edits next to each other; any order of equal keys gives the same
        // answer, this one just keeps the output deterministic.
        all.sort_by(|a, b| (a.start, a.end, &a.content).cmp(&(b.start, b.end, &b.content)));
        let kept = o5s_kernel::lines::merge(&ranges(all.iter().copied()))?;
        Some(Self {
            base: first.base,
            original: first.original.clone(),
            changes: kept.into_iter().map(|i| all[i].clone()).collect(),
        })
    }
}
fn ranges<'a>(edits: impl Iterator<Item = &'a LineEdit>) -> Vec<o5s_kernel::lines::Range> {
    edits
        .map(|e| (e.start, e.end, e.content.as_bytes().to_vec()))
        .collect()
}
/// Render through the extracted kernel: line boundaries, range-to-byte
/// conversion and the checked renderer. `changes` must be sorted and free of
/// conflicts, as `prepare_lines` and `merge_all` produce them.
fn render(original: &str, changes: &[LineEdit]) -> Result<String, Refusal> {
    let all: Vec<usize> = (0..changes.len()).collect();
    let bytes =
        o5s_kernel::lines::render(&original.as_bytes().to_vec(), &ranges(changes.iter()), &all)
            .ok_or_else(|| refuse("invalid line range, or materialization capacity exceeded"))?;
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
            if !o5s_kernel::lines::ranges_valid(&ranges(group.iter())) {
                return Err(refuse("overlapping line edits"));
            }
            let version = self.line_version(&group[0].path, group)?;
            let base = self
                .patches
                .get(&version.base)
                .and_then(|p| p.edits.iter().find(|e| e.path == group[0].path))
                .ok_or_else(|| refuse("line base file not found"))?;
            if base.base64 || !base.kind.is_file() {
                return Err(refuse(
                    "line edits need a text file; replace binary files whole",
                ));
            }
            let executable = base.executable;
            deps.push(version.base);
            out.push(Edit {
                path: group[0].path.clone(),
                content: Some(version.render()?),
                executable,
                base64: false,
                kind: crate::Kind::File,
            });
            i = end;
        }
        deps.sort_unstable();
        deps.dedup();
        if deps.iter().any(|d| !self.patches.contains_key(d)) {
            return Err(refuse("unknown dependency"));
        }
        // A base is an exact causal snapshot. Never discard earlier edits by
        // accepting ranges against an obsolete base in the same dependency
        // view: the base must be the file's sole causal head
        // (the extracted `causality::sole_head`).
        let rows = crate::documents::rows(self);
        let roots: Vec<usize> = deps.iter().map(|d| *d as usize).collect();
        let mut marks = vec![false; rows.len()];
        for edit in &out {
            let edits = (0..rows.len())
                .map(|h| {
                    self.patches
                        .get(&(h as u64))
                        .is_some_and(|p| p.edits.iter().any(|e| e.path == edit.path))
                })
                .collect();
            let base = changes.iter().find(|e| e.path == edit.path).unwrap().base;
            let (sole, m) =
                o5s_kernel::causality::sole_head(&rows, &edits, &roots, base as usize, marks);
            marks = m;
            if !sole {
                return Err(refuse(
                    "line base must be the sole causal file head; resolve or choose the latest base",
                ));
            }
        }
        Ok(out)
    }
}
