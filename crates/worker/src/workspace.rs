//! The changes between two Git trees, read through any object store: what an
//! agent's commits in its Artifacts workspace change relative to the snapshot
//! it started from. Unchanged subtrees (equal ids) are never read.
use o5s_platform::{Edit, Kind as PKind, Refusal, workspace};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Tree,
    File { executable: bool },
    Symlink,
    Submodule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub hash: String,
}

impl Kind {
    /// From Artifacts' `ArtifactsTreeEntryType`.
    pub fn parse(t: &str) -> Option<Kind> {
        Some(match t {
            "tree" => Kind::Tree,
            "blob" => Kind::File { executable: false },
            "exec" => Kind::File { executable: true },
            "symlink" => Kind::Symlink,
            "gitlink" => Kind::Submodule,
            _ => return None,
        })
    }
}

/// Read access to Git objects by id. Workers run one request per thread,
/// so the futures need not be `Send`.
#[allow(async_fn_in_trait)]
pub trait Objects {
    async fn tree(&self, hash: &str) -> Result<Vec<Entry>, String>;
    async fn blob(&self, hash: &str) -> Result<Vec<u8>, String>;
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Whole-file edits turning tree `base` into tree `head`, sorted by path,
/// within the workspace limits, including symlinks and submodules.
pub async fn changes<O: Objects>(o: &O, base: &str, head: &str) -> Result<Vec<Edit>, Refusal> {
    let unavailable = |e: String| Refusal(format!("workspace read failed: {e}"));
    let mut edits: BTreeMap<String, Edit> = BTreeMap::new();
    let mut total = 0usize;
    // (path, base tree, head tree) still to compare.
    let mut stack = vec![(
        String::new(),
        Some(base.to_string()),
        Some(head.to_string()),
    )];
    while let Some((prefix, b, h)) = stack.pop() {
        if b == h {
            continue;
        }
        let read = async |t: &Option<String>| -> Result<BTreeMap<String, Entry>, Refusal> {
            Ok(match t {
                None => BTreeMap::new(),
                Some(t) => o
                    .tree(t)
                    .await
                    .map_err(unavailable)?
                    .into_iter()
                    .map(|e| (e.name.clone(), e))
                    .collect(),
            })
        };
        let (b, h) = (read(&b).await?, read(&h).await?);
        let mut names: Vec<&String> = b.keys().chain(h.keys()).collect();
        names.sort();
        names.dedup();
        for name in names {
            let path = join(&prefix, name);
            let (old, new) = (b.get(name), h.get(name));
            if old == new {
                continue;
            }
            let subtree = |e: Option<&Entry>| match e {
                Some(Entry {
                    kind: Kind::Tree,
                    hash,
                    ..
                }) => Some(hash.clone()),
                _ => None,
            };
            let (old_tree, new_tree) = (subtree(old), subtree(new));
            if old_tree.is_some() || new_tree.is_some() {
                stack.push((path.clone(), old_tree, new_tree));
            }
            let leaf =
                |e: Option<&Entry>| matches!(e, Some(Entry { kind, .. }) if *kind != Kind::Tree);
            match new {
                Some(Entry {
                    kind: Kind::Submodule,
                    hash,
                    ..
                }) => {
                    let e = workspace::edit(
                        path.clone(),
                        Some(hash.clone().into_bytes()),
                        false,
                        PKind::Submodule,
                    )?;
                    edits.insert(path, e);
                }
                Some(Entry {
                    kind: kind @ (Kind::File { .. } | Kind::Symlink),
                    hash,
                    ..
                }) => {
                    let bytes = o.blob(hash).await.map_err(unavailable)?;
                    total = total.saturating_add(bytes.len());
                    if total > workspace::MAX_BYTES {
                        return Err(Refusal("workspace proposal exceeds limit".into()));
                    }
                    let (executable, k) = match kind {
                        Kind::File { executable } => (*executable, PKind::File),
                        _ => (false, PKind::Symlink),
                    };
                    edits.insert(
                        path.clone(),
                        workspace::edit(path, Some(bytes), executable, k)?,
                    );
                }
                // A file replaced by a directory, or removed.
                Some(Entry {
                    kind: Kind::Tree, ..
                })
                | None => {
                    if leaf(old) {
                        edits.insert(
                            path.clone(),
                            workspace::edit(path, None, false, PKind::File)?,
                        );
                    }
                }
            }
            if edits.len() > workspace::MAX_FILES {
                return Err(Refusal(format!(
                    "1..{} changed files required",
                    workspace::MAX_FILES
                )));
            }
        }
    }
    Ok(edits.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use o5s_gitpush as git;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Objects built by `o5s_gitpush`, read back by parsing tree bodies.
    #[derive(Default)]
    struct Store {
        objects: BTreeMap<String, git::Object>,
        reads: RefCell<usize>,
    }

    impl Store {
        fn add(&mut self, files: &[(&str, &[u8], bool)]) -> String {
            let map = files
                .iter()
                .map(|(p, b, x)| {
                    let mode = if *x {
                        git::Mode::Executable
                    } else {
                        git::Mode::Regular
                    };
                    (p.to_string(), (b.to_vec(), mode))
                })
                .collect();
            let (id, objects) = git::tree(&map).unwrap();
            for o in objects {
                self.objects.insert(git::hex(&o.id()), o);
            }
            git::hex(&id)
        }
    }

    impl Objects for Store {
        async fn tree(&self, hash: &str) -> Result<Vec<Entry>, String> {
            *self.reads.borrow_mut() += 1;
            let mut data = &self.objects.get(hash).ok_or("missing tree")?.data[..];
            let mut out = Vec::new();
            while !data.is_empty() {
                let sp = data.iter().position(|b| *b == b' ').unwrap();
                let nul = data.iter().position(|b| *b == 0).unwrap();
                let mode = std::str::from_utf8(&data[..sp]).unwrap();
                let name = std::str::from_utf8(&data[sp + 1..nul]).unwrap().to_string();
                let id: git::Oid = data[nul + 1..nul + 21].try_into().unwrap();
                let kind = match mode {
                    "40000" => Kind::Tree,
                    "100755" => Kind::File { executable: true },
                    "120000" => Kind::Symlink,
                    "160000" => Kind::Submodule,
                    _ => Kind::File { executable: false },
                };
                out.push(Entry {
                    name,
                    kind,
                    hash: git::hex(&id),
                });
                data = &data[nul + 21..];
            }
            Ok(out)
        }
        async fn blob(&self, hash: &str) -> Result<Vec<u8>, String> {
            Ok(self.objects.get(hash).ok_or("missing blob")?.data.clone())
        }
    }

    fn run<F: Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    fn summary(edits: &[Edit]) -> Vec<(String, Option<String>, bool, bool)> {
        edits
            .iter()
            .map(|e| (e.path.clone(), e.content.clone(), e.executable, e.base64))
            .collect()
    }

    #[test]
    fn diffs_files_directories_modes_and_binary() {
        let mut s = Store::default();
        let base = s.add(&[
            ("README", b"hello\n", false),
            ("src/a.rs", b"a\n", false),
            ("src/deep/b.rs", b"b\n", false),
            ("tools/run.sh", b"#!/bin/sh\n", false),
            ("gone/x", b"x", false),
            ("file-then-dir", b"f", false),
        ]);
        let head = s.add(&[
            ("README", b"hello\n", false),
            ("src/a.rs", b"A\n", false),
            ("src/deep/b.rs", b"b\n", false),
            ("src/new.rs", b"new\n", false),
            ("tools/run.sh", b"#!/bin/sh\n", true),
            ("img.bin", &[0xff, 0x00], false),
            ("file-then-dir/inner", b"i", false),
        ]);
        let edits = run(changes(&s, &base, &head)).unwrap();
        let t = |s: &str| Some(s.to_string());
        assert_eq!(
            summary(&edits),
            vec![
                ("file-then-dir".into(), None, false, false),
                ("file-then-dir/inner".into(), t("i"), false, false),
                ("gone/x".into(), None, false, false),
                ("img.bin".into(), t("/wA="), false, true),
                ("src/a.rs".into(), t("A\n"), false, false),
                ("src/new.rs".into(), t("new\n"), false, false),
                ("tools/run.sh".into(), t("#!/bin/sh\n"), true, false),
            ]
        );
        // Unchanged trees are equal ids and are not read again.
        *s.reads.borrow_mut() = 0;
        assert!(run(changes(&s, &base, &base)).unwrap().is_empty());
        assert_eq!(*s.reads.borrow(), 0);
    }

    #[test]
    fn reads_symlinks_and_submodules_and_refuses_oversized_proposals() {
        let mut s = Store::default();
        let base = s.add(&[("a", b"a", false)]);
        // A symlink to "a" and a submodule at commit 07…07.
        let blob = git::Object {
            kind: git::Kind::Blob,
            data: b"a".to_vec(),
        };
        s.objects.insert(git::hex(&blob.id()), blob.clone());
        let mut data = b"100644 a\0".to_vec();
        data.extend(blob.id());
        data.extend(b"120000 link\0");
        data.extend(blob.id());
        data.extend(b"160000 sub\0");
        data.extend([7u8; 20]);
        let tree = git::Object {
            kind: git::Kind::Tree,
            data,
        };
        let id = git::hex(&tree.id());
        s.objects.insert(id.clone(), tree);
        let edits = run(changes(&s, &base, &id)).unwrap();
        let kinds: Vec<_> = edits
            .iter()
            .map(|e| (e.path.as_str(), e.content.as_deref(), e.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("link", Some("a"), PKind::Symlink),
                (
                    "sub",
                    Some("0707070707070707070707070707070707070707"),
                    PKind::Submodule
                )
            ]
        );
        let big = vec![b'x'; workspace::MAX_BYTES + 1];
        let head = s.add(&[("a", &big, false)]);
        assert!(
            run(changes(&s, &base, &head))
                .unwrap_err()
                .0
                .contains("limit")
        );
    }
}
