//! Immutable block operations and conflict materialization for one document.
//!
//! The decisions are made by the extracted kernel `document`: validating and
//! committing a patch, and materializing the live text with its conflicts.
//! This adapter interns replicated identities (patch hashes, operation and
//! node ids) as dense indices in arrival order, translates references,
//! checks what only identities can decide (identity collisions, duplicate or
//! missing dependencies), and sorts diagnostics by identity so that they do
//! not depend on arrival order.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Portable identity supplied by a caller that verifies the canonical envelope.
/// This module does not authenticate authors or calculate cryptographic hashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PatchId(pub [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OperationId {
    pub patch: PatchId,
    pub ordinal: u32,
}

/// Atoms are immutable byte blocks chosen by the patch author. Insertion inside
/// an atom requires an explicit split operation, which this subset rejects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId {
    pub insertion: OperationId,
    pub ordinal: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Anchor {
    Start,
    Node(NodeId),
    End,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Insert {
        left: Anchor,
        right: Anchor,
        atoms: Vec<Vec<u8>>,
    },
    /// Each operation introduces a distinct deletion tag over named atoms.
    Delete { targets: Vec<NodeId> },
    /// Cancel exactly one observed deletion tag. Other tags remain effective.
    Restore { deletion: OperationId },
    /// Add reviewed order evidence for two existing atoms. New arrivals retain
    /// their ambiguity unless this evidence also determines their order.
    Order { before: NodeId, after: NodeId },
    /// Accept retention of one insertion concurrent with a named deletion.
    /// This acknowledgement cannot suppress another deletion or insertion.
    KeepInsertion {
        deletion: OperationId,
        insertion: OperationId,
    },
    /// Withdraw an observed order operation's edge, for example to break a
    /// cycle that concurrent order resolutions created. Other orders stand.
    Retract { order: OperationId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub id: PatchId,
    pub dependencies: Vec<PatchId>,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    IdentityCollision,
    InvalidDependencies,
    MissingDependencies(Vec<PatchId>),
    EmptyPatch,
    TooManyOperations,
    InvalidInsertion,
    UnknownAnchor(Anchor),
    UnobservedPatch(PatchId),
    UnorderedAnchors,
    EmptyDeletion,
    DuplicateTarget(NodeId),
    UnknownDeletion(OperationId),
    UnknownInsertion(OperationId),
    InvalidAcknowledgement,
    Cycle,
    UnknownOrder(OperationId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Inserted,
    AlreadyPresent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Conflict {
    /// Concurrent order resolutions can disagree and jointly introduce a
    /// cycle. Retain both operations and report every still-blocked atom.
    Cycle { blocked: Vec<NodeId> },
    /// These simultaneously available live atoms have no established order.
    Order { frontier: Vec<NodeId> },
    /// A concurrent insertion used an anchor removed by this active tag.
    DeleteInsert {
        deletion: OperationId,
        insertion: OperationId,
        anchors: Vec<NodeId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Materialized {
    /// Canonical diagnostic layers, never a silently chosen total order.
    pub layers: Vec<Vec<NodeId>>,
    pub conflicts: Vec<Conflict>,
    /// Present only when every live atom has an unambiguous order and all
    /// delete/insert conflicts have been reviewed or their tag restored.
    pub bytes: Option<Vec<u8>>,
}

/// What an interned operation created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Created {
    Insertion(usize),
    Deletion(usize),
    Order(usize),
    Other,
}

#[derive(Clone, Debug)]
pub struct DocumentGraph {
    /// Patches by identity, with their dense index.
    patches: BTreeMap<PatchId, (usize, Patch)>,
    /// Dense patch index to identity.
    ids: Vec<PatchId>,
    /// Atom identity to anchor index, and back (sentinels are not nodes).
    nodes: BTreeMap<NodeId, usize>,
    anchors: Vec<Anchor>,
    operations: BTreeMap<OperationId, Created>,
    /// Dense insertion, deletion and order numbers to operation identity.
    insertion_ids: Vec<OperationId>,
    deletion_ids: Vec<OperationId>,
    order_ids: Vec<OperationId>,
    doc: document::Doc,
}

impl Default for DocumentGraph {
    fn default() -> Self {
        Self {
            patches: BTreeMap::new(),
            ids: Vec::new(),
            nodes: BTreeMap::new(),
            anchors: vec![Anchor::Start, Anchor::End],
            operations: BTreeMap::new(),
            insertion_ids: Vec::new(),
            deletion_ids: Vec::new(),
            order_ids: Vec::new(),
            doc: document::empty(),
        }
    }
}

/// The state with every dense index replaced by its identity, so two
/// replicas that admitted the same patches in different orders compare equal.
#[derive(Debug, PartialEq, Eq)]
struct Canonical {
    patches: BTreeMap<PatchId, Patch>,
    atoms: BTreeMap<NodeId, Vec<u8>>,
    edges: BTreeSet<(Anchor, Anchor, Option<PatchId>)>,
    insertions: BTreeMap<OperationId, (Anchor, Anchor)>,
    deletions: BTreeMap<OperationId, BTreeSet<NodeId>>,
    restored: BTreeSet<OperationId>,
    acknowledged: BTreeSet<(OperationId, OperationId)>,
    retracted: BTreeSet<(OperationId, PatchId)>,
}

impl PartialEq for DocumentGraph {
    fn eq(&self, other: &Self) -> bool {
        self.canonical() == other.canonical()
    }
}
impl Eq for DocumentGraph {}

use crate::document;

impl DocumentGraph {
    pub fn patch_count(&self) -> usize {
        self.patches.len()
    }

    pub fn atom(&self, id: NodeId) -> Option<&[u8]> {
        self.nodes.get(&id).map(|a| self.doc.atoms[*a].as_slice())
    }

    fn node(&self, anchor: usize) -> NodeId {
        match self.anchors[anchor] {
            Anchor::Node(n) => n,
            _ => unreachable!("sentinels are never live atoms"),
        }
    }

    fn canonical(&self) -> Canonical {
        let patch = |p: usize| (p != document::NONE).then(|| self.ids[p]);
        Canonical {
            patches: self
                .patches
                .iter()
                .map(|(k, (_, p))| (*k, p.clone()))
                .collect(),
            atoms: self
                .nodes
                .iter()
                .map(|(n, a)| (*n, self.doc.atoms[*a].clone()))
                .collect(),
            edges: self
                .doc
                .edges
                .iter()
                .map(|(u, v, p)| (self.anchors[*u], self.anchors[*v], patch(*p)))
                .collect(),
            insertions: self
                .doc
                .insertions
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    (
                        self.insertion_ids[i],
                        (self.anchors[x.left], self.anchors[x.right]),
                    )
                })
                .collect(),
            deletions: self
                .doc
                .deletions
                .iter()
                .enumerate()
                .map(|(d, x)| {
                    (
                        self.deletion_ids[d],
                        x.targets.iter().map(|t| self.node(*t)).collect(),
                    )
                })
                .collect(),
            restored: (0..self.doc.restored.len())
                .filter(|d| self.doc.restored[*d])
                .map(|d| self.deletion_ids[d])
                .collect(),
            acknowledged: self
                .doc
                .acked
                .iter()
                .map(|(d, i)| (self.deletion_ids[*d], self.insertion_ids[*i]))
                .collect(),
            retracted: self
                .doc
                .retractions
                .iter()
                .map(|(e, p)| {
                    let order = self
                        .doc
                        .orders
                        .iter()
                        .position(|x| x == e)
                        .expect("order edge");
                    (self.order_ids[order], self.ids[*p])
                })
                .collect(),
        }
    }

    fn patch_index(&self, id: PatchId) -> usize {
        self.patches.get(&id).map_or(document::NONE, |(i, _)| *i)
    }

    fn node_ref(&self, n: NodeId) -> document::Ref {
        document::Ref {
            patch: self.patch_index(n.insertion.patch),
            index: self.nodes.get(&n).copied().unwrap_or(document::NONE),
        }
    }

    /// A reference to operation `o` as the `want`ed kind of object.
    fn op_ref(&self, o: OperationId, want: fn(Created) -> Option<usize>) -> document::Ref {
        let index = self
            .operations
            .get(&o)
            .and_then(|c| want(*c))
            .unwrap_or(document::NONE);
        document::Ref {
            patch: self.patch_index(o.patch),
            index,
        }
    }

    fn anchor_ref(&self, a: Anchor) -> document::Anchor {
        match a {
            Anchor::Start => document::Anchor::Start,
            Anchor::End => document::Anchor::End,
            Anchor::Node(n) => document::Anchor::Node(self.node_ref(n)),
        }
    }

    fn dense(&self, patch: &Patch) -> document::Patch {
        document::Patch {
            deps: patch
                .dependencies
                .iter()
                .map(|d| self.patch_index(*d))
                .collect(),
            ops: patch
                .operations
                .iter()
                .map(|o| match o {
                    Operation::Insert { left, right, atoms } => document::Op::Insert {
                        left: self.anchor_ref(*left),
                        right: self.anchor_ref(*right),
                        atoms: atoms.clone(),
                    },
                    Operation::Delete { targets } => document::Op::Delete {
                        targets: targets.iter().map(|t| self.node_ref(*t)).collect(),
                    },
                    Operation::Restore { deletion } => document::Op::Restore {
                        deletion: self.op_ref(*deletion, |c| match c {
                            Created::Deletion(d) => Some(d),
                            _ => None,
                        }),
                    },
                    Operation::Order { before, after } => document::Op::Order {
                        before: self.node_ref(*before),
                        after: self.node_ref(*after),
                    },
                    Operation::KeepInsertion {
                        deletion,
                        insertion,
                    } => document::Op::Keep {
                        deletion: self.op_ref(*deletion, |c| match c {
                            Created::Deletion(d) => Some(d),
                            _ => None,
                        }),
                        insertion: self.op_ref(*insertion, |c| match c {
                            Created::Insertion(i) => Some(i),
                            _ => None,
                        }),
                    },
                    Operation::Retract { order } => document::Op::Retract {
                        order: self.op_ref(*order, |c| match c {
                            Created::Order(o) => Some(o),
                            _ => None,
                        }),
                    },
                })
                .collect(),
        }
    }

    /// The `Error` a kernel refusal stands for, with the identities it names.
    fn error(patch: &Patch, r: document::Refusal) -> Error {
        let op = &patch.operations[r.op];
        let node = |n: &NodeId| Anchor::Node(*n);
        // The reference the refusal points at, as an anchor and its patch.
        let anchor = match (op, r.item) {
            (Operation::Insert { left, .. }, 0) => *left,
            (Operation::Insert { right, .. }, _) => *right,
            (Operation::Delete { targets }, k) => node(&targets[k]),
            (Operation::Order { before, .. }, 0) => node(before),
            (Operation::Order { after, .. }, _) => node(after),
            _ => Anchor::Start,
        };
        let operation = match (op, r.item) {
            (Operation::Restore { deletion }, _) => Some(*deletion),
            (Operation::KeepInsertion { deletion, .. }, 0) => Some(*deletion),
            (Operation::KeepInsertion { insertion, .. }, _) => Some(*insertion),
            (Operation::Retract { order }, _) => Some(*order),
            _ => None,
        };
        match r.kind {
            document::UNOBSERVED_PATCH => Error::UnobservedPatch(match (operation, anchor) {
                (Some(o), _) => o.patch,
                (None, Anchor::Node(n)) => n.insertion.patch,
                _ => unreachable!("sentinels are always observed"),
            }),
            document::UNKNOWN_ANCHOR => Error::UnknownAnchor(anchor),
            document::INVALID_INSERTION => Error::InvalidInsertion,
            document::UNORDERED_ANCHORS => Error::UnorderedAnchors,
            document::EMPTY_DELETION => Error::EmptyDeletion,
            document::DUPLICATE_TARGET => match anchor {
                Anchor::Node(n) => Error::DuplicateTarget(n),
                _ => unreachable!(),
            },
            document::UNKNOWN_DELETION => Error::UnknownDeletion(operation.expect("deletion")),
            document::UNKNOWN_INSERTION => Error::UnknownInsertion(operation.expect("insertion")),
            document::INVALID_ACKNOWLEDGEMENT => Error::InvalidAcknowledgement,
            document::CYCLE => Error::Cycle,
            document::UNKNOWN_ORDER => Error::UnknownOrder(operation.expect("order")),
            k => unreachable!("unknown refusal kind {k}"),
        }
    }

    /// Reject missing prerequisites without changing state. The exchange layer
    /// can retain the returned IDs and retry after those patches arrive.
    pub fn apply(&mut self, patch: Patch) -> Result<Applied, Error> {
        if let Some((_, existing)) = self.patches.get(&patch.id) {
            return if existing == &patch {
                Ok(Applied::AlreadyPresent)
            } else {
                Err(Error::IdentityCollision)
            };
        }
        let dependencies: BTreeSet<_> = patch.dependencies.iter().copied().collect();
        if dependencies.len() != patch.dependencies.len() || dependencies.contains(&patch.id) {
            return Err(Error::InvalidDependencies);
        }
        let missing: Vec<_> = dependencies
            .iter()
            .filter(|id| !self.patches.contains_key(id))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(Error::MissingDependencies(missing));
        }
        if patch.operations.is_empty() {
            return Err(Error::EmptyPatch);
        }
        if patch.operations.len() > u32::MAX as usize
            || patch.operations.iter().any(
                |o| matches!(o, Operation::Insert { atoms, .. } if atoms.len() > u32::MAX as usize),
            )
        {
            return Err(Error::TooManyOperations);
        }
        let dense = self.dense(&patch);
        let refusal = document::validate(&self.doc, &dense);
        if refusal.kind != 0 {
            return Err(Self::error(&patch, refusal));
        }
        // Mirror the numbering `commit` assigns.
        let index = self.ids.len();
        for (ordinal, operation) in patch.operations.iter().enumerate() {
            let id = OperationId {
                patch: patch.id,
                ordinal: ordinal as u32,
            };
            let created = match operation {
                Operation::Insert { atoms, .. } => {
                    for k in 0..atoms.len() {
                        let node = NodeId {
                            insertion: id,
                            ordinal: k as u32,
                        };
                        self.nodes.insert(node, self.anchors.len());
                        self.anchors.push(Anchor::Node(node));
                    }
                    self.insertion_ids.push(id);
                    Created::Insertion(self.insertion_ids.len() - 1)
                }
                Operation::Delete { .. } => {
                    self.deletion_ids.push(id);
                    Created::Deletion(self.deletion_ids.len() - 1)
                }
                Operation::Order { .. } => {
                    self.order_ids.push(id);
                    Created::Order(self.order_ids.len() - 1)
                }
                _ => Created::Other,
            };
            self.operations.insert(id, created);
        }
        self.doc = document::commit(std::mem::replace(&mut self.doc, document::empty()), &dense);
        debug_assert_eq!(self.doc.atoms.len(), self.anchors.len());
        self.ids.push(patch.id);
        self.patches.insert(patch.id, (index, patch));
        Ok(Applied::Inserted)
    }

    #[cfg(test)]
    fn reachable(&self, from: Anchor, to: Anchor) -> bool {
        let index = |a: Anchor| match a {
            Anchor::Start => document::START,
            Anchor::End => document::END,
            Anchor::Node(n) => self.nodes[&n],
        };
        document::reaches(&document::all_rows(&self.doc), index(from), index(to))
    }

    /// The live text and its conflicts. Diagnostics are sorted by identity.
    pub fn materialize(&self) -> Materialized {
        let view = document::materialize(&self.doc);
        let mut delete_inserts: Vec<_> = view
            .delete_inserts
            .iter()
            .map(|c| {
                let anchors: BTreeSet<_> = c.anchors.iter().map(|a| self.node(*a)).collect();
                (
                    self.deletion_ids[c.deletion],
                    self.insertion_ids[c.insertion],
                    anchors,
                )
            })
            .collect();
        delete_inserts.sort();
        let mut conflicts: Vec<_> = delete_inserts
            .into_iter()
            .map(|(deletion, insertion, anchors)| Conflict::DeleteInsert {
                deletion,
                insertion,
                anchors: anchors.into_iter().collect(),
            })
            .collect();
        let layers: Vec<Vec<NodeId>> = view
            .layers
            .iter()
            .map(|layer| {
                let mut nodes: Vec<_> = layer.iter().map(|a| self.node(*a)).collect();
                nodes.sort();
                nodes
            })
            .collect();
        for layer in &layers {
            if layer.len() > 1 {
                conflicts.push(Conflict::Order {
                    frontier: layer.clone(),
                });
            }
        }
        if !view.blocked.is_empty() {
            let mut blocked: Vec<_> = view.blocked.iter().map(|a| self.node(*a)).collect();
            blocked.sort();
            conflicts.push(Conflict::Cycle { blocked });
        }
        debug_assert_eq!(view.clean, conflicts.is_empty());
        Materialized {
            layers,
            bytes: view.clean.then_some(view.bytes),
            conflicts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u8) -> PatchId {
        PatchId([value; 32])
    }
    fn op(value: u8) -> OperationId {
        OperationId {
            patch: id(value),
            ordinal: 0,
        }
    }
    fn node(value: u8, ordinal: u32) -> NodeId {
        NodeId {
            insertion: op(value),
            ordinal,
        }
    }
    fn patch(value: u8, deps: &[u8], operations: Vec<Operation>) -> Patch {
        Patch {
            id: id(value),
            dependencies: deps.iter().map(|v| id(*v)).collect(),
            operations,
        }
    }
    fn insert(value: u8, deps: &[u8], left: Anchor, right: Anchor, atoms: &[&str]) -> Patch {
        patch(
            value,
            deps,
            vec![Operation::Insert {
                left,
                right,
                atoms: atoms.iter().map(|text| text.as_bytes().to_vec()).collect(),
            }],
        )
    }
    fn base() -> DocumentGraph {
        let mut state = DocumentGraph::default();
        state
            .apply(insert(
                1,
                &[],
                Anchor::Start,
                Anchor::End,
                &["a\n", "b\n", "c\n"],
            ))
            .unwrap();
        state
    }
    fn middle(value: u8, text: &str) -> Patch {
        insert(
            value,
            &[1],
            Anchor::Node(node(1, 0)),
            Anchor::Node(node(1, 1)),
            &[text],
        )
    }
    fn delete(value: u8, deps: &[u8], targets: &[NodeId]) -> Patch {
        patch(
            value,
            deps,
            vec![Operation::Delete {
                targets: targets.to_vec(),
            }],
        )
    }

    #[test]
    fn initial_and_sequential_editing_preserve_exact_bytes() {
        let mut state = base();
        state.apply(middle(2, "x\n")).unwrap();
        state
            .apply(insert(
                3,
                &[2],
                Anchor::Node(node(2, 0)),
                Anchor::Node(node(1, 1)),
                &["y\n"],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\ny\nb\nc\n".to_vec()));
        assert_eq!(state.atom(node(1, 1)), Some(b"b\n".as_slice()));
    }

    #[test]
    fn same_anchor_insertions_retain_an_order_conflict_in_both_deliveries() {
        let mut forward = base();
        let mut reverse = base();
        let a = middle(2, "x\n");
        let b = middle(3, "y\n");
        forward.apply(a.clone()).unwrap();
        forward.apply(b.clone()).unwrap();
        reverse.apply(b).unwrap();
        reverse.apply(a).unwrap();
        assert_eq!(forward, reverse);
        let view = forward.materialize();
        assert!(view.bytes.is_none());
        assert_eq!(
            view.conflicts,
            vec![Conflict::Order {
                frontier: vec![node(2, 0), node(3, 0)]
            }]
        );
    }

    #[test]
    fn disjoint_insertions_commute_without_conflicts() {
        let mut state = base();
        state.apply(middle(2, "x\n")).unwrap();
        state
            .apply(insert(
                3,
                &[1],
                Anchor::Node(node(1, 1)),
                Anchor::Node(node(1, 2)),
                &["y\n"],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\nb\ny\nc\n".to_vec()));
    }

    #[test]
    fn deletion_preserves_dead_interior_order_and_stable_anchors() {
        let mut state = base();
        state.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nc\n".to_vec()));
        state
            .apply(insert(
                3,
                &[2],
                Anchor::Node(node(1, 1)),
                Anchor::Node(node(1, 2)),
                &["x\n"],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\nc\n".to_vec()));
    }

    #[test]
    fn restoration_cancels_only_the_named_observed_tag() {
        let mut state = base();
        state.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        state.apply(delete(3, &[1], &[node(1, 1)])).unwrap();
        state
            .apply(patch(4, &[2], vec![Operation::Restore { deletion: op(2) }]))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nc\n".to_vec()));
        state
            .apply(patch(5, &[3], vec![Operation::Restore { deletion: op(3) }]))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nb\nc\n".to_vec()));
        state.apply(delete(6, &[4, 5], &[node(1, 1)])).unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nc\n".to_vec()));
    }

    #[test]
    fn concurrent_delete_insert_requires_a_pair_specific_resolution() {
        let mut state = base();
        state.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        state.apply(middle(3, "x\n")).unwrap();
        assert_eq!(
            state.materialize().conflicts,
            vec![Conflict::DeleteInsert {
                deletion: op(2),
                insertion: op(3),
                anchors: vec![node(1, 1)],
            }]
        );
        state
            .apply(patch(
                4,
                &[2, 3],
                vec![Operation::KeepInsertion {
                    deletion: op(2),
                    insertion: op(3),
                }],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\nc\n".to_vec()));
        state.apply(delete(5, &[1], &[node(1, 1)])).unwrap();
        assert_eq!(
            state.materialize().conflicts,
            vec![Conflict::DeleteInsert {
                deletion: op(5),
                insertion: op(3),
                anchors: vec![node(1, 1)],
            }]
        );
    }

    #[test]
    fn restoration_also_removes_conflicts_for_that_tag() {
        let mut state = base();
        state.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        state.apply(middle(3, "x\n")).unwrap();
        state
            .apply(patch(4, &[2], vec![Operation::Restore { deletion: op(2) }]))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\nb\nc\n".to_vec()));
    }

    #[test]
    fn order_resolution_does_not_hide_later_concurrent_insertions() {
        let mut state = base();
        state.apply(middle(2, "x\n")).unwrap();
        state.apply(middle(3, "y\n")).unwrap();
        state
            .apply(patch(
                4,
                &[2, 3],
                vec![Operation::Order {
                    before: node(2, 0),
                    after: node(3, 0),
                }],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\ny\nb\nc\n".to_vec()));
        state.apply(middle(5, "z\n")).unwrap();
        assert!(state.materialize().bytes.is_none());
        assert!(state.materialize().conflicts.contains(&Conflict::Order {
            frontier: vec![node(2, 0), node(5, 0)]
        }));
    }

    #[test]
    fn concurrent_opposite_order_resolutions_converge_to_explicit_cycle() {
        let mut a = base();
        a.apply(middle(2, "x\n")).unwrap();
        a.apply(middle(3, "y\n")).unwrap();
        let mut b = a.clone();
        let x = patch(
            4,
            &[2, 3],
            vec![Operation::Order {
                before: node(2, 0),
                after: node(3, 0),
            }],
        );
        let y = patch(
            5,
            &[2, 3],
            vec![Operation::Order {
                before: node(3, 0),
                after: node(2, 0),
            }],
        );
        a.apply(x.clone()).unwrap();
        a.apply(y.clone()).unwrap();
        b.apply(y).unwrap();
        b.apply(x).unwrap();
        assert_eq!(a, b);
        let view = a.materialize();
        assert!(view.bytes.is_none());
        assert!(
            matches!(&view.conflicts[0], Conflict::Cycle { blocked } if blocked.contains(&node(2, 0)) && blocked.contains(&node(3, 0)))
        );
    }

    #[test]
    fn retracting_one_order_recovers_from_a_cycle() {
        let mut a = base();
        a.apply(middle(2, "x\n")).unwrap();
        a.apply(middle(3, "y\n")).unwrap();
        let order = |v: u8, before: NodeId, after: NodeId| {
            patch(v, &[2, 3], vec![Operation::Order { before, after }])
        };
        a.apply(order(4, node(2, 0), node(3, 0))).unwrap();
        a.apply(order(5, node(3, 0), node(2, 0))).unwrap();
        assert!(a.materialize().bytes.is_none());
        let mut b = a.clone();
        // Retracting y-before-x leaves x before y.
        let retract = patch(6, &[4, 5], vec![Operation::Retract { order: op(5) }]);
        a.apply(retract.clone()).unwrap();
        assert_eq!(a.materialize().bytes, Some(b"a\nx\ny\nb\nc\n".to_vec()));
        // Only order operations retract, and only observed ones.
        assert_eq!(
            b.apply(patch(7, &[4, 5], vec![Operation::Retract { order: op(2) }])),
            Err(Error::UnknownOrder(op(2)))
        );
        assert_eq!(
            b.apply(patch(8, &[2], vec![Operation::Retract { order: op(5) }])),
            Err(Error::UnobservedPatch(id(5)))
        );
        b.apply(retract).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn causal_cycle_is_rejected_atomically() {
        let mut state = base();
        let original = state.clone();
        let bad = patch(
            2,
            &[1],
            vec![
                Operation::Delete {
                    targets: vec![node(1, 0)],
                },
                Operation::Order {
                    before: node(1, 2),
                    after: node(1, 0),
                },
            ],
        );
        assert_eq!(state.apply(bad), Err(Error::Cycle));
        assert_eq!(state, original);
    }

    #[test]
    fn opposite_orders_in_one_patch_are_a_causal_cycle() {
        let mut state = base();
        state.apply(middle(2, "x\n")).unwrap();
        state.apply(middle(3, "y\n")).unwrap();
        let original = state.clone();
        let result = state.apply(patch(
            4,
            &[2, 3],
            vec![
                Operation::Order {
                    before: node(2, 0),
                    after: node(3, 0),
                },
                Operation::Order {
                    before: node(3, 0),
                    after: node(2, 0),
                },
            ],
        ));
        assert_eq!(result, Err(Error::Cycle));
        assert_eq!(state, original);
    }

    #[test]
    fn unobserved_order_cannot_validate_new_anchors() {
        let mut state = base();
        state.apply(middle(2, "x\n")).unwrap();
        state.apply(middle(3, "y\n")).unwrap();
        state
            .apply(patch(
                4,
                &[2, 3],
                vec![Operation::Order {
                    before: node(2, 0),
                    after: node(3, 0),
                }],
            ))
            .unwrap();
        let between = insert(
            5,
            &[2, 3],
            Anchor::Node(node(2, 0)),
            Anchor::Node(node(3, 0)),
            &["z\n"],
        );
        assert_eq!(state.apply(between), Err(Error::UnorderedAnchors));
        state
            .apply(insert(
                5,
                &[4],
                Anchor::Node(node(2, 0)),
                Anchor::Node(node(3, 0)),
                &["z\n"],
            ))
            .unwrap();
        assert_eq!(
            state.materialize().bytes,
            Some(b"a\nx\nz\ny\nb\nc\n".to_vec())
        );
    }

    #[test]
    fn missing_dependencies_are_retryable_and_do_not_mutate_state() {
        let mut state = DocumentGraph::default();
        let pending = middle(2, "x\n");
        assert_eq!(
            state.apply(pending.clone()),
            Err(Error::MissingDependencies(vec![id(1)]))
        );
        assert_eq!(state, DocumentGraph::default());
        state = base();
        assert_eq!(state.apply(pending), Ok(Applied::Inserted));
    }

    #[test]
    fn identity_collision_and_unobserved_nodes_are_rejected() {
        let mut state = base();
        let original = insert(1, &[], Anchor::Start, Anchor::End, &["a\n", "b\n", "c\n"]);
        assert_eq!(state.apply(original), Ok(Applied::AlreadyPresent));
        assert_eq!(
            state.apply(insert(1, &[], Anchor::Start, Anchor::End, &["different"])),
            Err(Error::IdentityCollision)
        );
        assert_eq!(
            state.apply(insert(
                2,
                &[],
                Anchor::Node(node(1, 0)),
                Anchor::End,
                &["x"]
            )),
            Err(Error::UnobservedPatch(id(1)))
        );
        assert_eq!(state.patch_count(), 1);
    }

    #[test]
    fn duplicate_deletion_targets_and_empty_atoms_are_rejected() {
        let mut state = base();
        assert_eq!(
            state.apply(delete(2, &[1], &[node(1, 0), node(1, 0)])),
            Err(Error::DuplicateTarget(node(1, 0)))
        );
        assert_eq!(
            state.apply(insert(2, &[1], Anchor::Start, Anchor::End, &[""])),
            Err(Error::InvalidInsertion)
        );
    }

    #[test]
    fn self_references_are_rejected_without_a_relative_wire_form() {
        let mut state = DocumentGraph::default();
        let first = node(1, 0);
        let result = state.apply(patch(
            1,
            &[],
            vec![
                Operation::Insert {
                    left: Anchor::Start,
                    right: Anchor::End,
                    atoms: vec![b"x".to_vec()],
                },
                Operation::Delete {
                    targets: vec![first],
                },
                Operation::Restore {
                    deletion: OperationId {
                        patch: id(1),
                        ordinal: 1,
                    },
                },
            ],
        ));
        assert_eq!(result, Err(Error::UnobservedPatch(id(1))));
        assert_eq!(state, DocumentGraph::default());
    }

    #[test]
    fn journal_rolls_back_every_mutation_kind_after_later_refusal() {
        let mut original = base();
        original.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        original.apply(middle(3, "concurrent\n")).unwrap();
        let operations = vec![
            Operation::Insert {
                left: Anchor::Node(node(1, 0)),
                right: Anchor::Node(node(1, 1)),
                atoms: vec![b"new-a\n".to_vec(), b"new-b\n".to_vec()],
            },
            Operation::Delete {
                targets: vec![node(1, 0), node(1, 2)],
            },
            Operation::Restore { deletion: op(2) },
            Operation::Restore { deletion: op(2) },
            Operation::KeepInsertion {
                deletion: op(2),
                insertion: op(3),
            },
            Operation::KeepInsertion {
                deletion: op(2),
                insertion: op(3),
            },
            // Add new provenance to an edge that must survive rollback.
            Operation::Order {
                before: node(1, 0),
                after: node(1, 1),
            },
            Operation::Order {
                before: node(1, 0),
                after: node(1, 1),
            },
            // Also roll back an edge with no preexisting provenance.
            Operation::Order {
                before: node(1, 0),
                after: node(1, 2),
            },
        ];
        for end in 1..=operations.len() {
            let mut state = original.clone();
            let mut candidate = operations[..end].to_vec();
            candidate.push(Operation::Order {
                before: node(1, 0),
                after: node(1, 0),
            });
            assert_eq!(state.apply(patch(4, &[2, 3], candidate)), Err(Error::Cycle));
            // Includes reverse indices, provenance and idempotent set entries.
            assert_eq!(state, original, "refusal after operation {end}");
            assert_eq!(state.materialize(), original.materialize());
        }
    }

    #[test]
    fn journal_keeps_preexisting_restore_and_acknowledgement() {
        let mut state = base();
        state.apply(delete(2, &[1], &[node(1, 1)])).unwrap();
        state.apply(middle(3, "x\n")).unwrap();
        state
            .apply(patch(
                4,
                &[2, 3],
                vec![
                    Operation::Restore { deletion: op(2) },
                    Operation::KeepInsertion {
                        deletion: op(2),
                        insertion: op(3),
                    },
                ],
            ))
            .unwrap();
        let original = state.clone();
        assert_eq!(
            state.apply(patch(
                5,
                &[4],
                vec![
                    Operation::Restore { deletion: op(2) },
                    Operation::KeepInsertion {
                        deletion: op(2),
                        insertion: op(3)
                    },
                    Operation::Delete {
                        targets: vec![node(1, 0), node(1, 0)]
                    },
                ]
            )),
            Err(Error::DuplicateTarget(node(1, 0)))
        );
        assert_eq!(state, original);
        // Reusing the rejected identity succeeds: no admission record leaked.
        state
            .apply(patch(
                5,
                &[4],
                vec![Operation::Delete {
                    targets: vec![node(1, 2)],
                }],
            ))
            .unwrap();
        assert_eq!(state.materialize().bytes, Some(b"a\nx\nb\n".to_vec()));
    }

    fn permutations(values: &mut [Patch], at: usize, visit: &mut impl FnMut(&[Patch])) {
        if at == values.len() {
            visit(values);
            return;
        }
        for next in at..values.len() {
            values.swap(at, next);
            permutations(values, at + 1, visit);
            values.swap(at, next);
        }
    }

    #[test]
    fn all_120_deliveries_of_independent_changes_have_equal_state_and_view() {
        let initial = base();
        let mut changes = vec![
            middle(2, "x\n"),
            middle(3, "y\n"),
            delete(4, &[1], &[node(1, 1)]),
            delete(5, &[1], &[node(1, 2)]),
            insert(
                6,
                &[1],
                Anchor::Start,
                Anchor::Node(node(1, 0)),
                &["prefix\n"],
            ),
        ];
        let mut expected = None;
        let mut count = 0;
        permutations(&mut changes, 0, &mut |order| {
            let mut state = initial.clone();
            for change in order {
                state.apply(change.clone()).unwrap();
            }
            let result = (state.clone(), state.materialize());
            if let Some(expected) = &expected {
                assert_eq!(&result, expected);
            } else {
                expected = Some(result);
            }
            count += 1;
        });
        assert_eq!(count, 120);
    }

    #[test]
    fn indexed_boundary_frontiers_match_full_history_reachability() {
        for deleted in 0..64 {
            let mut state = base();
            state.apply(middle(2, "x\n")).unwrap();
            state.apply(middle(3, "y\n")).unwrap();
            state
                .apply(insert(
                    4,
                    &[1],
                    Anchor::Node(node(1, 1)),
                    Anchor::Node(node(1, 2)),
                    &["z\n"],
                ))
                .unwrap();
            let nodes = [
                node(1, 0),
                node(1, 1),
                node(1, 2),
                node(2, 0),
                node(3, 0),
                node(4, 0),
            ];
            let targets: Vec<_> = nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| deleted & (1 << i) != 0)
                .map(|(_, n)| *n)
                .collect();
            if !targets.is_empty() {
                state.apply(delete(5, &[2, 3, 4], &targets)).unwrap();
            }
            let mut remaining: BTreeSet<_> =
                nodes.into_iter().filter(|n| !targets.contains(n)).collect();
            let mut expected = Vec::new();
            while !remaining.is_empty() {
                let frontier: Vec<_> = remaining
                    .iter()
                    .filter(|n| {
                        !remaining.iter().any(|other| {
                            other != *n && state.reachable(Anchor::Node(*other), Anchor::Node(**n))
                        })
                    })
                    .copied()
                    .collect();
                assert!(!frontier.is_empty());
                for n in &frontier {
                    remaining.remove(n);
                }
                expected.push(frontier);
            }
            assert_eq!(
                state.materialize().layers,
                expected,
                "deletion mask {deleted}"
            );
        }
    }
}
