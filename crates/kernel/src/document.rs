//! One graph document in dense indices: validating and committing a patch,
//! and materializing the live text with its conflicts. Extracted to Lean.
//!
//! The native adapter (`model.rs`) interns replicated identities (patch
//! hashes, operation and node ids) as dense indices in arrival order and
//! translates every reference; this module never sees a hash. Anchors are
//! `0` (start), `1` (end) and then atoms in creation order. Patches are
//! indexed in arrival order, so every dependency index is smaller than its
//! dependent's.
//!
//! A patch is validated against the document as it was (plus the edges the
//! patch's own earlier operations add) and committed only when every
//! operation is valid, so a refused patch changes nothing.
use crate::indexed;

/// "None" for an index: an unknown patch or an unknown object of a known one.
pub const NONE: usize = usize::MAX;
pub const START: usize = 0;
pub const END: usize = 1;

/// Refusal kinds, mapped back to `model::Error` by the adapter.
pub const UNOBSERVED_PATCH: u64 = 1;
pub const UNKNOWN_ANCHOR: u64 = 2;
pub const INVALID_INSERTION: u64 = 3;
pub const UNORDERED_ANCHORS: u64 = 4;
pub const EMPTY_DELETION: u64 = 5;
pub const DUPLICATE_TARGET: u64 = 6;
pub const UNKNOWN_DELETION: u64 = 7;
pub const UNKNOWN_INSERTION: u64 = 8;
pub const INVALID_ACKNOWLEDGEMENT: u64 = 9;
pub const CYCLE: u64 = 10;
pub const UNKNOWN_ORDER: u64 = 11;

/// Something another patch created: that patch's index (`NONE` if this
/// document does not know the patch) and the object's index (`NONE` if the
/// patch did not create such an object).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref {
    pub patch: usize,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    End,
    /// `index` is an anchor index.
    Node(Ref),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Insert {
        left: Anchor,
        right: Anchor,
        atoms: Vec<Vec<u8>>,
    },
    /// Each `index` is an anchor index.
    Delete { targets: Vec<Ref> },
    /// `index` is a deletion number.
    Restore { deletion: Ref },
    /// Anchor indices.
    Order { before: Ref, after: Ref },
    /// A deletion number and an insertion number.
    Keep { deletion: Ref, insertion: Ref },
    /// An order number: withdraw that order operation's edge.
    Retract { order: Ref },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    /// Dense indices of the dependencies: distinct and all known.
    pub deps: Vec<usize>,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insertion {
    pub patch: usize,
    pub left: usize,
    pub right: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deletion {
    pub patch: usize,
    pub targets: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    /// Per patch, its dependencies.
    pub deps: Vec<Vec<usize>>,
    /// Per anchor, the patch that created it (`NONE` for the sentinels).
    pub creator: Vec<usize>,
    /// Per anchor, its bytes (empty for the sentinels).
    pub atoms: Vec<Vec<u8>>,
    /// `(from, to, patch)`: an order edge between anchors and the patch that
    /// added it. The sentinel edge `(START, END)` has patch `NONE`.
    pub edges: Vec<(usize, usize, usize)>,
    pub insertions: Vec<Insertion>,
    pub deletions: Vec<Deletion>,
    /// Per deletion, whether some patch restored it.
    pub restored: Vec<bool>,
    /// `(deletion, insertion)` pairs a patch acknowledged.
    pub acked: Vec<(usize, usize)>,
    /// Per order operation, the index of its edge in `edges`.
    pub orders: Vec<usize>,
    /// `(edge, patch)`: an order edge a patch retracted.
    pub retractions: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub kind: u64,
    /// The operation's position in the patch.
    pub op: usize,
    /// Which reference of the operation: 0 for the first (left, before,
    /// deletion), 1 for the second, or the position of a deletion target.
    pub item: usize,
}

/// The empty document: the two sentinels and the edge between them.
pub fn empty() -> Doc {
    let mut creator = Vec::new();
    creator.push(NONE);
    creator.push(NONE);
    let mut atoms = Vec::new();
    atoms.push(Vec::new());
    atoms.push(Vec::new());
    let mut edges = Vec::new();
    edges.push((START, END, NONE));
    Doc {
        deps: Vec::new(),
        creator,
        atoms,
        edges,
        insertions: Vec::new(),
        deletions: Vec::new(),
        restored: Vec::new(),
        acked: Vec::new(),
        orders: Vec::new(),
        retractions: Vec::new(),
    }
}

/// `n` false marks.
pub fn falses(n: usize) -> Vec<bool> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        out.push(false);
        i += 1;
    }
    out
}

/// `n` empty rows.
pub fn empty_rows(n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        out.push(Vec::new());
        i += 1;
    }
    out
}

/// Per known patch, whether it is in the causal past of `roots` (the roots
/// included).
pub fn observed(deps: &Vec<Vec<usize>>, roots: &Vec<usize>) -> Vec<bool> {
    let found = indexed::reachable(deps, roots, falses(deps.len()));
    let mut out = falses(deps.len());
    let mut i = 0;
    while i < found.queue.len() {
        out[found.queue[i]] = true;
        i += 1;
    }
    out
}

fn add_edge(mut rows: Vec<Vec<usize>>, from: usize, to: usize) -> Vec<Vec<usize>> {
    rows[from].push(to);
    rows
}

/// Per edge, whether a patch with `seen[patch]` (all patches when `seen`
/// is empty), or this patch itself (`own`), retracted it.
pub fn withdrawn(
    edges: usize,
    retractions: &Vec<(usize, usize)>,
    seen: &Vec<bool>,
    own: &Vec<usize>,
) -> Vec<bool> {
    let mut out = falses(edges);
    let mut i = 0;
    while i < retractions.len() {
        let (e, p) = retractions[i];
        if seen.len() == 0 || (p < seen.len() && seen[p]) {
            out[e] = true;
        }
        i += 1;
    }
    let mut j = 0;
    while j < own.len() {
        out[own[j]] = true;
        j += 1;
    }
    out
}

/// Adjacency rows over `n` anchors of the edges a patch may rely on: the
/// sentinel edge, edges of observed patches that no observed patch
/// retracted, and the patch's own `pending` edges.
pub fn causal_rows(
    edges: &Vec<(usize, usize, usize)>,
    off: &Vec<bool>,
    seen: &Vec<bool>,
    pending: &Vec<(usize, usize)>,
    n: usize,
) -> Vec<Vec<usize>> {
    let mut rows = empty_rows(n);
    let mut i = 0;
    while i < edges.len() {
        let e = edges[i];
        if !off[i] && (e.2 == NONE || (e.2 < seen.len() && seen[e.2])) {
            rows = add_edge(rows, e.0, e.1);
        }
        i += 1;
    }
    let mut j = 0;
    while j < pending.len() {
        rows = add_edge(rows, pending[j].0, pending[j].1);
        j += 1;
    }
    rows
}

/// Whether `to` is reachable from `from` (or equal to it) along `rows`.
pub fn reaches(rows: &Vec<Vec<usize>>, from: usize, to: usize) -> bool {
    let mut seeds = Vec::new();
    seeds.push(from);
    let found = indexed::reachable(rows, &seeds, falses(rows.len()));
    h5i_app_std::set::contains(&found.queue, &to)
}

/// Validation state across the operations of one patch.
pub struct Check {
    /// The first refusal, if any (`kind == 0` while none).
    pub refusal: Refusal,
    /// Edges added by this patch's earlier operations.
    pub pending: Vec<(usize, usize)>,
    /// Anchors this patch's earlier insertions created.
    pub created: usize,
    /// Edges this patch's earlier operations retracted.
    pub retracted: Vec<usize>,
}

fn refuse(kind: u64, op: usize, item: usize) -> Refusal {
    Refusal { kind, op, item }
}

/// `0` when the reference names an observed patch's existing object below
/// `count`, otherwise the refusal kind.
fn check_ref(r: Ref, seen: &Vec<bool>, count: usize, missing: u64) -> u64 {
    if r.patch == NONE || r.patch >= seen.len() || !seen[r.patch] {
        UNOBSERVED_PATCH
    } else if r.index == NONE || r.index >= count {
        missing
    } else {
        0
    }
}

/// An anchor's dense index, given that it is valid.
pub fn anchor_index(a: Anchor) -> usize {
    match a {
        Anchor::Start => START,
        Anchor::End => END,
        Anchor::Node(r) => r.index,
    }
}

fn check_anchor(a: Anchor, doc: &Doc, seen: &Vec<bool>) -> u64 {
    match a {
        Anchor::Node(r) => {
            let k = check_ref(r, seen, doc.atoms.len(), UNKNOWN_ANCHOR);
            if k == 0 && r.index < 2 {
                UNKNOWN_ANCHOR
            } else {
                k
            }
        }
        _ => 0,
    }
}

fn atoms_nonempty(atoms: &Vec<Vec<u8>>) -> bool {
    let mut ok = atoms.len() > 0;
    let mut i = 0;
    while i < atoms.len() {
        ok = ok && atoms[i].len() > 0;
        i += 1;
    }
    ok
}

/// The edges an insertion of `n` atoms between `left` and `right` adds when
/// its atoms get anchor indices `first..first + n`.
pub fn chain(
    mut out: Vec<(usize, usize)>,
    left: usize,
    right: usize,
    first: usize,
    n: usize,
) -> Vec<(usize, usize)> {
    let mut previous = left;
    let mut k = 0;
    while k < n {
        out.push((previous, first + k));
        previous = first + k;
        k += 1;
    }
    out.push((previous, right));
    out
}

fn check_insert(
    state: Check,
    doc: &Doc,
    seen: &Vec<bool>,
    op: usize,
    left: Anchor,
    right: Anchor,
    atoms: &Vec<Vec<u8>>,
) -> Check {
    let kl = check_anchor(left, doc, seen);
    let kr = check_anchor(right, doc, seen);
    let l = anchor_index(left);
    let r = anchor_index(right);
    if kl != 0 {
        Check {
            refusal: refuse(kl, op, 0),
            ..state
        }
    } else if kr != 0 {
        Check {
            refusal: refuse(kr, op, 1),
            ..state
        }
    } else if !atoms_nonempty(atoms) || l == END || r == START || l == r {
        Check {
            refusal: refuse(INVALID_INSERTION, op, 0),
            ..state
        }
    } else {
        let n = doc.atoms.len() + state.created;
        let off = withdrawn(doc.edges.len(), &doc.retractions, seen, &state.retracted);
        let rows = causal_rows(&doc.edges, &off, seen, &state.pending, n);
        if !reaches(&rows, l, r) {
            Check {
                refusal: refuse(UNORDERED_ANCHORS, op, 0),
                ..state
            }
        } else {
            let first = doc.atoms.len() + state.created;
            Check {
                refusal: state.refusal,
                pending: chain(state.pending, l, r, first, atoms.len()),
                created: state.created + atoms.len(),
                retracted: state.retracted,
            }
        }
    }
}

fn check_delete(state: Check, doc: &Doc, seen: &Vec<bool>, op: usize, targets: &Vec<Ref>) -> Check {
    if targets.len() == 0 {
        Check {
            refusal: refuse(EMPTY_DELETION, op, 0),
            ..state
        }
    } else {
        let mut refusal = state.refusal;
        let mut found: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < targets.len() {
            if refusal.kind == 0 {
                let k = check_anchor(Anchor::Node(targets[i]), doc, seen);
                if k != 0 {
                    refusal = refuse(k, op, i);
                } else if h5i_app_std::set::contains(&found, &targets[i].index) {
                    refusal = refuse(DUPLICATE_TARGET, op, i);
                } else {
                    found.push(targets[i].index);
                }
            }
            i += 1;
        }
        Check { refusal, ..state }
    }
}

fn check_restore(state: Check, doc: &Doc, seen: &Vec<bool>, op: usize, deletion: Ref) -> Check {
    let k = check_ref(deletion, seen, doc.deletions.len(), UNKNOWN_DELETION);
    if k != 0 {
        Check {
            refusal: refuse(k, op, 0),
            ..state
        }
    } else {
        state
    }
}

fn check_order(
    state: Check,
    doc: &Doc,
    seen: &Vec<bool>,
    op: usize,
    before: Ref,
    after: Ref,
) -> Check {
    let kb = check_anchor(Anchor::Node(before), doc, seen);
    let ka = check_anchor(Anchor::Node(after), doc, seen);
    if kb != 0 {
        Check {
            refusal: refuse(kb, op, 0),
            ..state
        }
    } else if ka != 0 {
        Check {
            refusal: refuse(ka, op, 1),
            ..state
        }
    } else {
        let n = doc.atoms.len() + state.created;
        let off = withdrawn(doc.edges.len(), &doc.retractions, seen, &state.retracted);
        let rows = causal_rows(&doc.edges, &off, seen, &state.pending, n);
        if reaches(&rows, after.index, before.index) {
            Check {
                refusal: refuse(CYCLE, op, 0),
                ..state
            }
        } else {
            let mut pending = state.pending;
            pending.push((before.index, after.index));
            Check { pending, ..state }
        }
    }
}

fn check_keep(
    state: Check,
    doc: &Doc,
    seen: &Vec<bool>,
    op: usize,
    deletion: Ref,
    insertion: Ref,
) -> Check {
    // Observation of both is checked before existence of either.
    let od = check_ref(deletion, seen, NONE, 0);
    let oi = check_ref(insertion, seen, NONE, 0);
    if od != 0 {
        Check {
            refusal: refuse(od, op, 0),
            ..state
        }
    } else if oi != 0 {
        Check {
            refusal: refuse(oi, op, 1),
            ..state
        }
    } else if deletion.index >= doc.deletions.len() {
        Check {
            refusal: refuse(UNKNOWN_DELETION, op, 0),
            ..state
        }
    } else if insertion.index >= doc.insertions.len() {
        Check {
            refusal: refuse(UNKNOWN_INSERTION, op, 1),
            ..state
        }
    } else {
        let targets = &doc.deletions[deletion.index].targets;
        let ins = &doc.insertions[insertion.index];
        if h5i_app_std::set::contains(targets, &ins.left)
            || h5i_app_std::set::contains(targets, &ins.right)
        {
            state
        } else {
            Check {
                refusal: refuse(INVALID_ACKNOWLEDGEMENT, op, 0),
                ..state
            }
        }
    }
}

fn check_retract(mut state: Check, doc: &Doc, seen: &Vec<bool>, op: usize, order: Ref) -> Check {
    let k = check_ref(order, seen, doc.orders.len(), UNKNOWN_ORDER);
    if k != 0 {
        Check {
            refusal: refuse(k, op, 0),
            ..state
        }
    } else {
        state.retracted.push(doc.orders[order.index]);
        state
    }
}

fn check_op(state: Check, doc: &Doc, seen: &Vec<bool>, op: usize, o: &Op) -> Check {
    if state.refusal.kind != 0 {
        state
    } else {
        match o {
            Op::Insert { left, right, atoms } => {
                check_insert(state, doc, seen, op, *left, *right, atoms)
            }
            Op::Delete { targets } => check_delete(state, doc, seen, op, targets),
            Op::Restore { deletion } => check_restore(state, doc, seen, op, *deletion),
            Op::Order { before, after } => check_order(state, doc, seen, op, *before, *after),
            Op::Keep {
                deletion,
                insertion,
            } => check_keep(state, doc, seen, op, *deletion, *insertion),
            Op::Retract { order } => check_retract(state, doc, seen, op, *order),
        }
    }
}

/// The first refusal of `p` against `doc`, or `kind == 0` if every operation
/// is valid. References must be observed: in the causal past of `p.deps`.
pub fn validate(doc: &Doc, p: &Patch) -> Refusal {
    let seen = observed(&doc.deps, &p.deps);
    let mut state = Check {
        refusal: refuse(0, 0, 0),
        pending: Vec::new(),
        created: 0,
        retracted: Vec::new(),
    };
    let mut i = 0;
    while i < p.ops.len() {
        state = check_op(state, doc, &seen, i, &p.ops[i]);
        i += 1;
    }
    state.refusal
}

fn commit_insert(
    mut doc: Doc,
    patch: usize,
    left: usize,
    right: usize,
    atoms: &Vec<Vec<u8>>,
) -> Doc {
    let first = doc.atoms.len();
    let mut k = 0;
    while k < atoms.len() {
        doc.creator.push(patch);
        doc.atoms
            .push(h5i_app_std::bytes::slice(&atoms[k], 0, atoms[k].len()));
        k += 1;
    }
    let links = chain(Vec::new(), left, right, first, atoms.len());
    let mut j = 0;
    while j < links.len() {
        doc.edges.push((links[j].0, links[j].1, patch));
        j += 1;
    }
    doc.insertions.push(Insertion { patch, left, right });
    doc
}

fn ref_indices(rs: &Vec<Ref>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rs.len() {
        out.push(rs[i].index);
        i += 1;
    }
    out
}

fn commit_op(mut doc: Doc, patch: usize, o: &Op) -> Doc {
    match o {
        Op::Insert { left, right, atoms } => {
            commit_insert(doc, patch, anchor_index(*left), anchor_index(*right), atoms)
        }
        Op::Delete { targets } => {
            doc.deletions.push(Deletion {
                patch,
                targets: ref_indices(targets),
            });
            doc.restored.push(false);
            doc
        }
        Op::Restore { deletion } => {
            doc.restored[deletion.index] = true;
            doc
        }
        Op::Order { before, after } => {
            doc.orders.push(doc.edges.len());
            doc.edges.push((before.index, after.index, patch));
            doc
        }
        Op::Keep {
            deletion,
            insertion,
        } => {
            if !has_pair(&doc.acked, deletion.index, insertion.index) {
                doc.acked.push((deletion.index, insertion.index));
            }
            doc
        }
        Op::Retract { order } => {
            let e = doc.orders[order.index];
            doc.retractions.push((e, patch));
            doc
        }
    }
}

/// Add a validated patch as the next patch index.
pub fn commit(mut doc: Doc, p: &Patch) -> Doc {
    let patch = doc.deps.len();
    let mut deps = Vec::new();
    let mut d = 0;
    while d < p.deps.len() {
        deps.push(p.deps[d]);
        d += 1;
    }
    doc.deps.push(deps);
    let mut i = 0;
    while i < p.ops.len() {
        doc = commit_op(doc, patch, &p.ops[i]);
        i += 1;
    }
    doc
}

/// Two patches are concurrent when neither is in the other's causal past.
pub fn concurrent(deps: &Vec<Vec<usize>>, a: usize, b: usize) -> bool {
    if a == b {
        false
    } else {
        let mut ra = Vec::new();
        ra.push(a);
        let mut rb = Vec::new();
        rb.push(b);
        let pa = observed(deps, &ra);
        let pb = observed(deps, &rb);
        !pa[b] && !pb[a]
    }
}

/// A concurrent insertion anchored on an atom an active deletion removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteInsert {
    pub deletion: usize,
    pub insertion: usize,
    /// The insertion's anchors among the deletion's targets.
    pub anchors: Vec<usize>,
}

/// The insertion anchors `left`, `right` among a deletion's `targets`.
pub fn shared_anchors(targets: &Vec<usize>, left: usize, right: usize) -> Vec<usize> {
    let mut anchors = Vec::new();
    if h5i_app_std::set::contains(targets, &left) {
        anchors.push(left);
    }
    if h5i_app_std::set::contains(targets, &right) {
        anchors.push(right);
    }
    anchors
}

/// Whether `pairs` contains `(a, b)`, compared field by field so that the
/// extracted code needs no opaque tuple equality.
pub fn has_pair(pairs: &Vec<(usize, usize)>, a: usize, b: usize) -> bool {
    let mut found = false;
    let mut i = 0;
    while i < pairs.len() {
        found = found || (pairs[i].0 == a && pairs[i].1 == b);
        i += 1;
    }
    found
}

/// The conflicts of deletion `d` with every insertion.
fn deletion_conflicts(mut out: Vec<DeleteInsert>, doc: &Doc, d: usize) -> Vec<DeleteInsert> {
    let patch = doc.deletions[d].patch;
    let mut i = 0;
    while i < doc.insertions.len() {
        let anchors = shared_anchors(
            &doc.deletions[d].targets,
            doc.insertions[i].left,
            doc.insertions[i].right,
        );
        if anchors.len() > 0
            && !has_pair(&doc.acked, d, i)
            && concurrent(&doc.deps, patch, doc.insertions[i].patch)
        {
            out.push(DeleteInsert {
                deletion: d,
                insertion: i,
                anchors,
            });
        }
        i += 1;
    }
    out
}

/// Every unacknowledged insertion concurrent with an unrestored deletion
/// whose targets include one of its anchors.
pub fn delete_inserts(doc: &Doc) -> Vec<DeleteInsert> {
    let mut out = Vec::new();
    let mut d = 0;
    while d < doc.deletions.len() {
        if !doc.restored[d] {
            out = deletion_conflicts(out, doc, d);
        }
        d += 1;
    }
    out
}

/// Per anchor, whether it is a live atom: every deletion covering it has
/// been restored. The sentinels are never live.
pub fn liveness(doc: &Doc) -> Vec<bool> {
    let mut covering = empty_rows(doc.atoms.len());
    let mut d = 0;
    while d < doc.deletions.len() {
        let mut t = 0;
        while t < doc.deletions[d].targets.len() {
            covering = add_edge(covering, doc.deletions[d].targets[t], d);
            t += 1;
        }
        d += 1;
    }
    let mut live = Vec::new();
    let mut a = 0;
    while a < doc.atoms.len() {
        live.push(a >= 2 && crate::tags::visible(&covering[a], &doc.restored));
        a += 1;
    }
    live
}

/// All edges no patch retracted, as adjacency rows.
pub fn all_rows(doc: &Doc) -> Vec<Vec<usize>> {
    let off = withdrawn(doc.edges.len(), &doc.retractions, &Vec::new(), &Vec::new());
    let mut rows = empty_rows(doc.atoms.len());
    let mut i = 0;
    while i < doc.edges.len() {
        if !off[i] {
            rows = add_edge(rows, doc.edges[i].0, doc.edges[i].1);
        }
        i += 1;
    }
    rows
}

/// Rows of dead anchors only: the interiors a live boundary passes through.
pub fn dead_rows(rows: &Vec<Vec<usize>>, live: &Vec<bool>) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut a = 0;
    while a < rows.len() {
        if live[a] {
            out.push(Vec::new());
        } else {
            let mut row = Vec::new();
            let mut k = 0;
            while k < rows[a].len() {
                row.push(rows[a][k]);
                k += 1;
            }
            out.push(row);
        }
        a += 1;
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// Kahn layers of live anchors; a layer of two or more is an order
    /// conflict.
    pub layers: Vec<Vec<usize>>,
    /// Live anchors left when no anchor was ready: on or after a cycle.
    pub blocked: Vec<usize>,
    pub delete_inserts: Vec<DeleteInsert>,
    /// The concatenated live atoms, meaningful only when `clean`.
    pub bytes: Vec<u8>,
    /// No order, cycle or delete/insert conflict.
    pub clean: bool,
}

/// Materialization state of the Kahn loop.
pub struct Kahn {
    pub indegree: Vec<usize>,
    pub remaining: Vec<bool>,
    pub left: usize,
    pub ready: Vec<usize>,
    pub layers: Vec<Vec<usize>>,
    pub bytes: Vec<u8>,
    pub blocked: bool,
}

fn emit(mut k: Kahn, doc: &Doc, boundaries: &Vec<Vec<usize>>) -> Kahn {
    let frontier = k.ready;
    let mut next = Vec::new();
    let mut i = 0;
    while i < frontier.len() {
        let s = frontier[i];
        k.remaining[s] = false;
        k.left -= 1;
        k.bytes = crate::interval::append_range(k.bytes, &doc.atoms[s], 0, doc.atoms[s].len());
        let mut t = 0;
        while t < boundaries[s].len() {
            let v = boundaries[s][t];
            k.indegree[v] -= 1;
            if k.indegree[v] == 0 {
                next.push(v);
            }
            t += 1;
        }
        i += 1;
    }
    k.layers.push(frontier);
    k.ready = next;
    k
}

/// The live text: liveness from deletion tags, live-to-live order through
/// dead interiors (`indexed::boundary`), Kahn layers, and conflicts.
pub fn materialize(doc: &Doc) -> View {
    let delete_inserts = delete_inserts(doc);
    let live = liveness(doc);
    let rows = all_rows(doc);
    let dead = dead_rows(&rows, &live);
    let n = doc.atoms.len();
    let mut marks = falses(n);
    let mut boundaries = Vec::new();
    let mut indegree = Vec::new();
    let mut a = 0;
    while a < n {
        indegree.push(0);
        a += 1;
    }
    let mut remaining = Vec::new();
    let mut left = 0;
    let mut s = 0;
    while s < n {
        if live[s] {
            let t = indexed::boundary(&dead, &rows[s], &live, marks);
            marks = t.marks;
            let mut j = 0;
            while j < t.queue.len() {
                indegree[t.queue[j]] += 1;
                j += 1;
            }
            boundaries.push(t.queue);
            remaining.push(s);
            left += 1;
        } else {
            boundaries.push(Vec::new());
        }
        s += 1;
    }
    let ready = crate::order::frontier_counts(&indegree, &remaining);
    let mut k = Kahn {
        indegree,
        remaining: live.clone(),
        left,
        ready,
        layers: Vec::new(),
        bytes: Vec::new(),
        blocked: false,
    };
    while k.left > 0 && !k.blocked {
        if k.ready.len() == 0 {
            k.blocked = true;
        } else {
            k = emit(k, doc, &boundaries);
        }
    }
    let mut blocked = Vec::new();
    let mut b = 0;
    while b < n {
        if k.blocked && k.remaining[b] {
            blocked.push(b);
        }
        b += 1;
    }
    let mut ordered = true;
    let mut l = 0;
    while l < k.layers.len() {
        ordered = ordered && k.layers[l].len() == 1;
        l += 1;
    }
    let clean = ordered && !k.blocked && delete_inserts.len() == 0;
    View {
        layers: k.layers,
        blocked,
        delete_inserts,
        bytes: k.bytes,
        clean,
    }
}
