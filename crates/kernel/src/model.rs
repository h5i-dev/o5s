//! Immutable block operations and conflict materialization for one document.
//!
//! Native validation and dense-index construction surround the extracted
//! live-boundary traversal. The outer adapter and complete operation semantics
//! do not yet have Lean proofs.
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Insertion {
    left: Anchor,
    right: Anchor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentGraph {
    patches: BTreeMap<PatchId, Patch>,
    atoms: BTreeMap<NodeId, Vec<u8>>,
    edges: BTreeMap<Anchor, BTreeSet<Anchor>>,
    edge_sources: BTreeMap<(Anchor, Anchor), BTreeSet<OperationId>>,
    insertions: BTreeMap<OperationId, Insertion>,
    insertions_at: BTreeMap<NodeId, BTreeSet<OperationId>>,
    deletions: BTreeMap<OperationId, BTreeSet<NodeId>>,
    restored: BTreeSet<OperationId>,
    acknowledged: BTreeSet<(OperationId, OperationId)>,
}

impl Default for DocumentGraph {
    fn default() -> Self {
        Self {
            patches: BTreeMap::new(),
            atoms: BTreeMap::new(),
            edges: BTreeMap::from([(Anchor::Start, BTreeSet::from([Anchor::End]))]),
            edge_sources: BTreeMap::new(),
            insertions: BTreeMap::new(),
            insertions_at: BTreeMap::new(),
            deletions: BTreeMap::new(),
            restored: BTreeSet::new(),
            acknowledged: BTreeSet::new(),
        }
    }
}

impl DocumentGraph {
    pub fn patch_count(&self) -> usize {
        self.patches.len()
    }

    pub fn atom(&self, id: NodeId) -> Option<&[u8]> {
        self.atoms.get(&id).map(Vec::as_slice)
    }

    /// Reject missing prerequisites without changing state. The exchange layer
    /// can retain the returned IDs and retry after those patches arrive.
    pub fn apply(&mut self, patch: Patch) -> Result<Applied, Error> {
        if let Some(existing) = self.patches.get(&patch.id) {
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
        if patch.operations.len() > u32::MAX as usize {
            return Err(Error::TooManyOperations);
        }
        let observed = self.dependency_closure(&patch.dependencies);
        // Clone-and-publish makes all operations in a patch atomic. This is a
        // specification strategy, not an acceptable production storage path.
        let mut next = self.clone();
        for (ordinal, operation) in patch.operations.iter().enumerate() {
            let id = OperationId {
                patch: patch.id,
                ordinal: ordinal as u32,
            };
            next.apply_operation(id, operation, &observed)?;
        }
        next.patches.insert(patch.id, patch);
        *self = next;
        Ok(Applied::Inserted)
    }

    fn dependency_closure(&self, roots: &[PatchId]) -> BTreeSet<PatchId> {
        let mut found = BTreeSet::new();
        let mut stack = roots.to_vec();
        while let Some(id) = stack.pop() {
            if found.insert(id) {
                if let Some(patch) = self.patches.get(&id) {
                    stack.extend(patch.dependencies.iter().copied());
                }
            }
        }
        found
    }

    fn observe(id: PatchId, observed: &BTreeSet<PatchId>) -> Result<(), Error> {
        if observed.contains(&id) {
            Ok(())
        } else {
            Err(Error::UnobservedPatch(id))
        }
    }

    fn anchor(&self, anchor: Anchor, observed: &BTreeSet<PatchId>) -> Result<(), Error> {
        if let Anchor::Node(node) = anchor {
            Self::observe(node.insertion.patch, observed)?;
            if !self.atoms.contains_key(&node) {
                return Err(Error::UnknownAnchor(anchor));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn reachable(&self, from: Anchor, to: Anchor) -> bool {
        self.reachable_in(from, to, None)
    }

    fn reachable_in(&self, from: Anchor, to: Anchor, observed: Option<&BTreeSet<PatchId>>) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(at) = stack.pop() {
            if at == to {
                return true;
            }
            if seen.insert(at) {
                if let Some(next) = self.edges.get(&at) {
                    stack.extend(
                        next.iter()
                            .filter(|next| {
                                observed.is_none_or(|known| {
                                    (at == Anchor::Start && **next == Anchor::End)
                                        || self.edge_sources.get(&(at, **next)).is_some_and(
                                            |sources| {
                                                sources.iter().any(|id| known.contains(&id.patch))
                                            },
                                        )
                                })
                            })
                            .copied(),
                    );
                }
            }
        }
        false
    }

    fn edge(&mut self, from: Anchor, to: Anchor, source: OperationId) {
        self.edges.entry(from).or_default().insert(to);
        self.edge_sources
            .entry((from, to))
            .or_default()
            .insert(source);
    }

    fn apply_operation(
        &mut self,
        id: OperationId,
        operation: &Operation,
        observed: &BTreeSet<PatchId>,
    ) -> Result<(), Error> {
        // References cannot use the current content hash, but order evidence
        // introduced by earlier operations in this same patch is causal.
        let mut causal_edges = observed.clone();
        causal_edges.insert(id.patch);
        match operation {
            Operation::Insert { left, right, atoms } => {
                self.anchor(*left, observed)?;
                self.anchor(*right, observed)?;
                if atoms.is_empty()
                    || atoms.len() > u32::MAX as usize
                    || atoms.iter().any(Vec::is_empty)
                    || *left == Anchor::End
                    || *right == Anchor::Start
                    || left == right
                {
                    return Err(Error::InvalidInsertion);
                }
                if !self.reachable_in(*left, *right, Some(&causal_edges)) {
                    return Err(Error::UnorderedAnchors);
                }
                let mut previous = *left;
                for (ordinal, bytes) in atoms.iter().enumerate() {
                    let node = NodeId {
                        insertion: id,
                        ordinal: ordinal as u32,
                    };
                    self.atoms.insert(node, bytes.clone());
                    self.edge(previous, Anchor::Node(node), id);
                    previous = Anchor::Node(node);
                }
                self.edge(previous, *right, id);
                self.insertions.insert(
                    id,
                    Insertion {
                        left: *left,
                        right: *right,
                    },
                );
                for anchor in [left, right] {
                    if let Anchor::Node(node) = anchor {
                        self.insertions_at.entry(*node).or_default().insert(id);
                    }
                }
            }
            Operation::Delete { targets } => {
                if targets.is_empty() {
                    return Err(Error::EmptyDeletion);
                }
                let mut unique = BTreeSet::new();
                for target in targets {
                    self.anchor(Anchor::Node(*target), observed)?;
                    if !unique.insert(*target) {
                        return Err(Error::DuplicateTarget(*target));
                    }
                }
                self.deletions.insert(id, unique);
            }
            Operation::Restore { deletion } => {
                Self::observe(deletion.patch, observed)?;
                if !self.deletions.contains_key(deletion) {
                    return Err(Error::UnknownDeletion(*deletion));
                }
                self.restored.insert(*deletion);
            }
            Operation::Order { before, after } => {
                self.anchor(Anchor::Node(*before), observed)?;
                self.anchor(Anchor::Node(*after), observed)?;
                if self.reachable_in(
                    Anchor::Node(*after),
                    Anchor::Node(*before),
                    Some(&causal_edges),
                ) {
                    return Err(Error::Cycle);
                }
                self.edge(Anchor::Node(*before), Anchor::Node(*after), id);
            }
            Operation::KeepInsertion {
                deletion,
                insertion,
            } => {
                Self::observe(deletion.patch, observed)?;
                Self::observe(insertion.patch, observed)?;
                let deleted = self
                    .deletions
                    .get(deletion)
                    .ok_or(Error::UnknownDeletion(*deletion))?;
                let inserted = self
                    .insertions
                    .get(insertion)
                    .ok_or(Error::UnknownInsertion(*insertion))?;
                if ![inserted.left, inserted.right]
                    .iter()
                    .any(|a| matches!(a, Anchor::Node(n) if deleted.contains(n)))
                {
                    return Err(Error::InvalidAcknowledgement);
                }
                self.acknowledged.insert((*deletion, *insertion));
            }
        }
        Ok(())
    }

    fn concurrent(&self, a: PatchId, b: PatchId) -> bool {
        a != b
            && !self.dependency_closure(&[a]).contains(&b)
            && !self.dependency_closure(&[b]).contains(&a)
    }

    /// Reconstruct the live partial order by traversing the full history.
    /// Deleted atoms remain anchors and contribute ordering paths.
    pub fn materialize(&self) -> Materialized {
        let mut conflicts = Vec::new();
        for (deletion, targets) in &self.deletions {
            if self.restored.contains(deletion) {
                continue;
            }
            let candidates: BTreeSet<_> = targets
                .iter()
                .filter_map(|target| self.insertions_at.get(target))
                .flat_map(|insertions| insertions.iter())
                .copied()
                .collect();
            for insertion in candidates {
                let inserted = &self.insertions[&insertion];
                if !self.concurrent(deletion.patch, insertion.patch)
                    || self.acknowledged.contains(&(*deletion, insertion))
                {
                    continue;
                }
                let anchors: BTreeSet<_> = [inserted.left, inserted.right]
                    .iter()
                    .filter_map(|a| match a {
                        Anchor::Node(n) if targets.contains(n) => Some(*n),
                        _ => None,
                    })
                    .collect();
                if !anchors.is_empty() {
                    conflicts.push(Conflict::DeleteInsert {
                        deletion: *deletion,
                        insertion,
                        anchors: anchors.into_iter().collect(),
                    });
                }
            }
        }
        // Every graph anchor gets one dense index. Indices are derived here and
        // never enter replicated patch identities. Both sentinels are dead.
        let anchors: Vec<_> = std::iter::once(Anchor::Start)
            .chain(self.atoms.keys().copied().map(Anchor::Node))
            .chain(std::iter::once(Anchor::End))
            .collect();
        let indices: BTreeMap<_, _> = anchors
            .iter()
            .copied()
            .enumerate()
            .map(|(i, a)| (a, i))
            .collect();
        let rows: Vec<Vec<usize>> = anchors
            .iter()
            .map(|anchor| {
                self.edges
                    .get(anchor)
                    .into_iter()
                    .flat_map(|row| row.iter())
                    .map(|a| indices[a])
                    .collect()
            })
            .collect();
        // Dense deletion IDs are local projection indices, never replicated IDs.
        // Every covering entry indexes the flag appended for that deletion.
        let mut covering = vec![Vec::new(); anchors.len()];
        let mut restored = Vec::with_capacity(self.deletions.len());
        for (tag, (deletion, targets)) in self.deletions.iter().enumerate() {
            restored.push(self.restored.contains(deletion));
            for node in targets {
                covering[indices[&Anchor::Node(*node)]].push(tag);
            }
        }
        let live: Vec<bool> = anchors
            .iter()
            .enumerate()
            .map(|(index, anchor)| {
                matches!(anchor, Anchor::Node(_))
                    && crate::tags::visible(&covering[index], &restored)
            })
            .collect();
        let dead_rows = rows
            .iter()
            .zip(&live)
            .map(|(row, alive)| if *alive { Vec::new() } else { row.clone() })
            .collect();
        let mut marks = vec![false; anchors.len()];
        let mut boundaries = vec![Vec::new(); anchors.len()];
        let mut indegree = vec![0usize; anchors.len()];
        let mut remaining = BTreeSet::new();
        for source in 0..anchors.len() {
            if !live[source] {
                continue;
            }
            remaining.insert(source);
            let traversal = crate::indexed::boundary(&dead_rows, &rows[source], &live, marks);
            marks = traversal.marks;
            boundaries[source] = traversal.queue;
            for target in &boundaries[source] {
                indegree[*target] += 1;
            }
        }
        let remaining_indices = remaining.iter().copied().collect();
        let mut ready: BTreeSet<_> = crate::order::frontier_counts(&indegree, &remaining_indices)
            .into_iter()
            .collect();
        let mut layers = Vec::new();
        let mut output = Vec::new();
        while !remaining.is_empty() {
            if ready.is_empty() {
                let blocked = remaining
                    .iter()
                    .map(|i| match anchors[*i] {
                        Anchor::Node(n) => n,
                        _ => unreachable!(),
                    })
                    .collect();
                conflicts.push(Conflict::Cycle { blocked });
                break;
            }
            let frontier_indices: Vec<_> = std::mem::take(&mut ready).into_iter().collect();
            let frontier: Vec<_> = frontier_indices
                .iter()
                .map(|i| match anchors[*i] {
                    Anchor::Node(n) => n,
                    _ => unreachable!(),
                })
                .collect();
            if frontier.len() > 1 {
                conflicts.push(Conflict::Order {
                    frontier: frontier.clone(),
                });
            }
            for source in frontier_indices {
                remaining.remove(&source);
                if let Anchor::Node(node) = anchors[source] {
                    output.extend_from_slice(&self.atoms[&node]);
                }
                for target in &boundaries[source] {
                    indegree[*target] -= 1;
                    if indegree[*target] == 0 {
                        ready.insert(*target);
                    }
                }
            }
            layers.push(frontier);
        }
        let bytes = conflicts.is_empty().then_some(output);
        Materialized {
            layers,
            conflicts,
            bytes,
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
