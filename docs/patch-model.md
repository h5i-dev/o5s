# Document graph operations

`crates/kernel/src/model.rs` defines immutable operations for one document. Its `DocumentGraph` validates causal references, retains deleted anchors and materializes bytes only when the surviving graph has an unambiguous order. Materialization calls the extracted `indexed::boundary`, `order::frontier_counts` and `tags::visible` functions. Validation, the dense-index adapter and the complete text/conflict semantics still need extraction and Lean proofs.

This module covers a document subset of M1 and M2 in [the production roadmap](roadmap.md). File identity, path changes, mode changes and operation-block splitting remain unspecified. It does not implement Pijul's complete data model.

## Identity and causal context

A `PatchId` holds 32 bytes. The registry must calculate and verify the hash of a canonical patch envelope before calling `DocumentGraph::apply`. The document module treats the supplied identity as opaque. It rejects a different record carrying an existing identity, but cannot detect a dishonest first record by itself.

An `OperationId` consists of the patch identity and its zero-based operation ordinal. A `NodeId` consists of an insertion operation identity and its zero-based atom ordinal. An atom contains immutable nonempty bytes. Authors choose atom boundaries; they can use lines or smaller blocks. A client can insert between atoms but cannot address an offset inside one atom. `Start` and `End` are document anchors rather than content atoms.

Each patch names its direct dependencies. All dependencies must be present before application. Missing prerequisites produce a retryable `MissingDependencies` result without changing the document. A caller may stage that patch elsewhere, retrieve the prerequisites and retry it. Applying an identical record twice returns `AlreadyPresent`.

Operations can refer to nodes and tags created by their dependency closure. Receiving some unrelated patch first does not make its nodes visible to a later patch. Anchor-order validation follows only edges whose creating operations belong to that causal context. This restriction prevents delivery order from deciding whether a patch is valid.

References to a patch's own identity are rejected. Hashing an envelope that contains its own full hash would require a self-referential identity. Supporting references between operations in one patch requires a relative-reference wire form and canonical expansion rules; those have not been implemented.

## Operations

| Operation | Meaning | Rejection conditions |
|---|---|---|
| `Insert { left, right, atoms }` | Add a chain of stable atoms between two observed anchors; preserve existing edges | Unknown or unobserved anchor, empty atom, reversed sentinel, equal anchors, or no causal path from left to right |
| `Delete { targets }` | Add a new deletion tag over exactly the named observed atoms | Empty target set, duplicate target, unknown or unobserved atom |
| `Restore { deletion }` | Cancel one observed deletion tag | Unknown deletion or a tag outside the causal context |
| `Order { before, after }` | Add order evidence for two observed atoms | Unknown or unobserved atom, or an order edge closing a cycle already visible to the author |
| `KeepInsertion { deletion, insertion }` | Acknowledge retention of one insertion anchored on an atom covered by one named deletion | Unknown or unobserved operations, or a deletion that covers neither insertion anchor |

All operations in a patch apply atomically. The implementation currently validates against a cloned document and publishes the clone only after every operation succeeds. Production storage must replace this copying strategy with a durable transaction that has the same failure behavior.

## Deletion, restoration and later arrivals

An atom is live exactly when no uncancelled deletion tag covers it. Two concurrent deletions therefore combine without selecting one author. Restoring one deletion leaves the other effective. A later deletion creates another tag and can hide the atom again. Multiple restorations of the same tag have the same effect as one restoration.

Materialization assigns each deletion a local dense index and builds one restoration flag per deletion, plus the covering-index list for each atom. It calls `tags::visible` on those lists. The scanners visit each covering entry once; they do not search the full restoration history for each target. Constructing the lists still uses native ordered-map lookups. Each scanner requires every covering index to be within the restoration table. Rust vector lengths fit `usize`; the scanner allocates no output buffer. Mapping operation identities to those indices and matching restoration flags to operations remain native adapter obligations.

Deletion removes visibility, not the anchor or its edges. An insertion that observes the deletion may deliberately use the dead anchor. Such an insertion remains live. A concurrent insertion using that anchor produces a `DeleteInsert` conflict until the deletion is restored or a `KeepInsertion` operation acknowledges that exact pair. A resolution naming one pair does not acknowledge a different deletion or a later insertion.

This is observed-tag restoration. It does not remove a patch, rewrite dependencies or reverse an arbitrary collection of operations. Channel removal and general undo still need a separate specification.

## Order conflicts and materialization

Materialization assigns temporary dense indices to all anchors, constructs adjacency rows and marks live content atoms. For each live source it calls `indexed::boundary` with outgoing edges from dead vertices. The returned live-boundary edges preserve ordering paths through deleted atoms. Sentinels do not become output bytes.

The materializer counts incoming boundary edges and selects its initial frontier with `order::frontier_counts`, a linear scan over the remaining dense indices. Native Kahn updates decrement successor counts as each frontier is removed. A frontier containing multiple atoms records an `Order` conflict. Its diagnostic layer lists those identities in their canonical ID order; that list does not choose an output order. `Materialized::bytes` remains absent whenever any conflict exists.

An `Order` operation can establish order between known concurrent insertions. An insertion arriving afterward remains ambiguous unless existing edges already determine its position. Concurrent reviewers can also introduce opposite order edges. Both records remain present, and materialization reports a `Cycle` containing atoms still blocked after all available frontiers have been removed. That blocked set can include atoms downstream of the cycle; it is not a minimal cycle witness.

The current operation set cannot retract order evidence. Recovering from a cycle requires a specified replacement or cancellation operation before this subset can serve general review workflows. Hiding all cycle members through explicit deletions can remove them from output, but that is not a substitute for order-edge cancellation.

## Minimal histories

Each insertion below names the initial patch as its dependency unless stated otherwise. The unit tests use exact stable node IDs and compare delivery permutations.

| History | Expected result |
|---|---|
| Initial atoms `a`, `b`, `c`; insert `x` between `a` and `b` | `a x b c` |
| Independently insert `x` and `y` between `a` and `b` | Order conflict; no output bytes |
| Insert `x` between `a` and `b`, and `y` between `b` and `c` | `a x b y c` |
| Delete `b` | `a c`; `b` remains an anchor |
| Observe deletion of `b`, then insert `x` between `b` and `c` | `a x c` |
| Concurrent deletion of `b` and insertion anchored on `b` | Delete/insert conflict; no output bytes |
| Restore one of two deletions covering `b` | `b` remains hidden |
| Restore both deletions covering `b` | `b` becomes visible |
| Resolve `x` before `y`, then receive concurrent `z` at the original gap | New order conflict involving `z` |
| Concurrently resolve `x` before `y` and `y` before `x` | Cycle conflict; no output bytes |
| Receive a patch before its dependency | No mutation; retryable missing-dependency result |
| Valid deletion followed by an invalid order in one patch | Entire patch rejected; deletion has no effect |

## Evidence and remaining work

The model tests cover stable anchors, atomic rejection, duplicate delivery, tag restoration, pair-specific acknowledgement, late arrivals, concurrent order cycles and all 120 permutations of five independent changes. A differential test checks indexed frontier reconstruction against full-history reachability for 64 deletion masks. Run them with `cargo test -p o5s-kernel model::tests`.

The current boundary search reuses visited marks and stops at live atoms. It avoids a separate full graph search for every pair of live atoms. A reverse anchor index restricts delete/insert conflict checks to insertions that actually use a deleted target; unrelated insertions are skipped. Causal ancestry checks still traverse dependency records for each candidate pair.

Each materialization still reconstructs all adjacency rows and every live boundary row. There is no affected-component cache, persistent adjacency index or measured thousands-of-agents throughput for this document path.

`proofs/Tags.lean` proves the extracted visibility scanner returns true exactly when no active covering tag remains. It also proves that changing a different restoration flag cannot revive an atom with an active covering tag. Both results require valid covering indices; they do not prove the adapter's dense-ID mapping or the operation admission rules.

The Lean boundary theorems justify the traversal under their dense-row and liveness assumptions. They do not yet prove that this adapter constructs those rows correctly, that operation application preserves its invariants, or that these conflict layers implement a complete patch theory. Initial indegree construction and subsequent Kahn updates also need invariant proofs. The M3 proof work must follow the Rust functions called here and discharge those adapter obligations. A separate abstract model would leave the application unverified.

Further work must specify atom splitting, document/file identity, creation and removal, rename conflicts, byte encoding policy, binary objects, modes, symlinks, submodules, cycle recovery and canonical wire envelopes. Storage and replication must also connect reviewed patch identities to admitted graph operations before claiming end-to-end verified merge behavior.
