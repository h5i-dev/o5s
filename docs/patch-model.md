# Document graph operations

`crates/kernel/src/model.rs` defines immutable operations for one document. Its `DocumentGraph` validates causal references, retains deleted anchors and materializes bytes only when the surviving graph has an unambiguous order. Validation, commit and materialization (text and conflicts) are made by the extracted `document` kernel and proved against an identity-level model (`DocRefine.apply_refines`, `DocMatRefine.materialize_refines`, `extracted_view_converges`; see [proofs](proofs.md)). The native adapter that interns identities as dense indices is not extracted.

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
| `Retract { order }` | Withdraw one observed `Order` operation's edge | Unobserved patch, or an operation that is not an `Order` (`UnknownOrder`) |

All operations in a patch apply atomically for returned validation errors. Each operation validates before mutating. A journal records successful additions and reverses them if a later operation is refused, including edge provenance, reverse anchor indices and newly inserted restoration or acknowledgement entries. Entries that existed before the patch remain present. The patch record is published only after all operations succeed. The native journal has refusal tests but no Lean proof; durable storage still requires a transaction with the same failure behavior.

## Deletion, restoration and later arrivals

An atom is live exactly when no uncancelled deletion tag covers it. Two concurrent deletions therefore combine without selecting one author. Restoring one deletion leaves the other effective. A later deletion creates another tag and can hide the atom again. Multiple restorations of the same tag have the same effect as one restoration.

Materialization assigns each deletion a local dense index and builds one restoration flag per deletion, plus the covering-index list for each atom. It calls `tags::visible` on those lists. The scanners visit each covering entry once; they do not search the full restoration history for each target. Constructing the lists still uses native ordered-map lookups. Each scanner requires every covering index to be within the restoration table. Rust vector lengths fit `usize`; the scanner allocates no output buffer. Mapping operation identities to those indices and matching restoration flags to operations remain native adapter obligations.

Deletion removes visibility, not the anchor or its edges. An insertion that observes the deletion may deliberately use the dead anchor. Such an insertion remains live. A concurrent insertion using that anchor produces a `DeleteInsert` conflict until the deletion is restored or a `KeepInsertion` operation acknowledges that exact pair. A resolution naming one pair does not acknowledge a different deletion or a later insertion.

This is observed-tag restoration. It does not remove a patch, rewrite dependencies or reverse an arbitrary collection of operations. Channel removal and general undo still need a separate specification.

## Order conflicts and materialization

Materialization is the extracted `document::materialize` (`crates/kernel/src/document.rs`). The document holds dense indices assigned in arrival order: anchor 0 is the start, anchor 1 the end, and atoms follow in creation order. It constructs adjacency rows and marks live content atoms. For each live source it calls `indexed::boundary` with outgoing edges from dead vertices. The returned live-boundary edges preserve ordering paths through deleted atoms. Sentinels do not become output bytes.

The materializer counts incoming boundary edges and selects its initial frontier with `order::frontier_counts`, a linear scan over the remaining dense indices. Its Kahn loop decrements successor counts as each frontier is removed. A frontier containing multiple atoms records an `Order` conflict. The native adapter sorts each layer by node identity, so diagnostics do not depend on arrival order; that list does not choose an output order. `Materialized::bytes` remains absent whenever any conflict exists.

An `Order` operation can establish order between known concurrent insertions. An insertion arriving afterward remains ambiguous unless existing edges already determine its position. Concurrent reviewers can also introduce opposite order edges. Both records remain present, and materialization reports a `Cycle` containing atoms still blocked after all available frontiers have been removed. That blocked set can include atoms downstream of the cycle; it is not a minimal cycle witness.

A cycle is recovered by retracting one of the order operations on it: `Retract { order }` withdraws that operation's edge, and the remaining order stands. Retractions form a set, like deletion tags, so their delivery order does not matter. When validating a later patch, only retractions by patches it observed count, so validation still reads only the patch's causal past. Materialization counts every retraction.

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
| Then retract `y` before `x` | `x` before `y`; the cycle is gone |
| Receive a patch before its dependency | No mutation; retryable missing-dependency result |
| Valid deletion followed by an invalid order in one patch | Entire patch rejected; deletion has no effect |

## Files, modes and binary content

A path is either a graph document or a legacy whole-file/line document, for its whole history. The registry kernel records the model of each path at its first proposal and refuses the other kind afterwards.

- **Graph documents.** Every graph edit states whether the file exists (`"remove": true` removes it) and whether it is executable. These states form a causal register per path. A patch supersedes the nearest earlier edits of the path in its causal past, and concurrent heads that disagree, such as a removal and an edit, are an explicit conflict. A later patch observing both resolves it. An edit without operations creates an empty document, removes one, or changes only its mode. Removal keeps the graph, so restoring the file can continue from its atoms. Atoms are UTF-8.
- **Whole-file edits.** `{"path","content","executable","base64"}`. A `null` content removes the file. With `"base64": true` the content is standard padded base64 of arbitrary bytes. Concurrent edits conflict when content, mode or encoding differ. Line edits apply only to text files and keep the base's mode.
- **Mixed proposals.** `propose_graph` accepts `files` (whole-file edits) beside graph edits, so a proposal can carry text documents and binary files together. A path cannot be both in one proposal.
- **Symbolic links and submodules.** A whole-file edit with `"kind": "symlink"` holds the link target: non-empty, at most 4,096 bytes, no NUL. One with `"kind": "submodule"` holds a 40-digit lowercase commit id. Neither may be executable or base64. Both are registers like any whole-file edit, so a concurrent retarget conflicts. Line edits and graph documents are text only.
- **Identity.** The new fields appear in a patch body only when set, and mixed proposals use the body tag `o5s-mixed-patch-v1`. Every earlier patch keeps its SHA-256 identity.
- **Moves.** A graph edit with `"from": "old/path"` moves that document to its `path`, keeping its atoms and history. A document's identity is the path where it was created, and its location is a register of its own:
  - creation, a move, or an edit that settles a conflicting location writes it; ordinary edits do not;
  - an edit names a path as its author saw it, and resolves to the document whose location, in the author's causal past, is that path;
  - an edit of the old path made concurrently with a move therefore lands in the moved document;
  - concurrent moves to different paths show a conflict at each path until an edit that observes both settles one;
  - a new file at a path a document moved away from is a new document.

  The CLI proposes a move when a graph document disappears at one path and a new text file shares at least half of its lines. Whole-file (legacy) paths, binary files, symlinks and submodules move as a removal and an addition, as in Git.

Views list executable paths in `executable`, base64 paths in `binary`, links in `symlinks` and submodules in `submodules`. Git export (native and Cloudflare Artifacts) and `o5s fetch` write modes 100644, 100755, 120000 (link target blob) and 160000 (gitlink to the commit; no object). The CLI reads links with `readlink` and submodules from the index's gitlinks, and refuses any path that passes through a symlink.

## Evidence and remaining work

The model tests cover stable anchors, atomic rejection, duplicate delivery, tag restoration, pair-specific acknowledgement, late arrivals, concurrent order cycles and all 120 permutations of five independent changes. Refusal tests reject a patch after each prefix of mixed operations and compare the entire state, including duplicate set entries and shared edge provenance. A differential test checks indexed frontier reconstruction against full-history reachability for 64 deletion masks. Run them with `cargo test -p o5s-kernel model::tests`.

The current boundary search reuses visited marks and stops at live atoms. It avoids a separate full graph search for every pair of live atoms. A reverse anchor index restricts delete/insert conflict checks to insertions that actually use a deleted target; unrelated insertions are skipped. Anchor validation checks an available direct causal edge before expanding concurrent branches. Causal ancestry checks still traverse dependency records for each candidate pair and compute a dependency closure for each admitted patch.

Each materialization still reconstructs all adjacency rows and every live boundary row. There is no affected-component cache or persistent adjacency index in this document adapter. The `document_history` example measures actual admission and materialization for independent and sequential histories, and repeats admission with a whole-document clone wrapped around the same production function. That comparison isolates copying overhead; it does not compare against a historical binary. Run `cargo run -p o5s-kernel --example document_history --release -- 2000`. These local in-memory measurements do not establish network or durable multi-agent throughput.

One release-mode run on the development host produced these results. Each patch inserts one two-byte atom. Independent patches share one base; sequential patches depend on their predecessor. The host exposes an AMD EPYC 7502 CPU and was also running development tasks. Times are milliseconds, measured once per case.

| Additional patches | History | Journal admission | Clone-wrapper admission | Journal materialization |
|---|---|---:|---:|---:|
| 2,000 | Independent | 17.376 | 1,202.570 | 4.172 |
| 2,000 | Sequential | 421.106 | 1,840.243 | 1.870 |
| 8,000 | Independent | 43.570 | 39,673.679 | 8.609 |
| 8,000 | Sequential | 8,685.275 | Not completed | 8.057 |

The 8,000-patch sequential clone-wrapper case was stopped after the other results were recorded. Sequential admission still computes the complete dependency closure for every patch and therefore accumulates quadratic ancestry work. Removing whole-document copying does not resolve that cost. A bounded causal index and affected-component materialization remain required before claiming long-history scalability.

`proofs/Tags.lean` proves the extracted visibility scanner returns true exactly when no active covering tag remains. It also proves that changing a different restoration flag cannot revive an atom with an active covering tag. Both results require valid covering indices; they do not prove the adapter's dense-ID mapping or the operation admission rules.

The Lean boundary theorems justify the traversal under their dense-row and liveness assumptions. They do not yet prove that this adapter constructs those rows correctly, that operation application preserves its invariants, or that these conflict layers implement a complete patch theory. Initial indegree construction and subsequent Kahn updates also need invariant proofs. The M3 proof work must follow the Rust functions called here and discharge those adapter obligations. A separate abstract model would leave the application unverified.

Further work must specify atom splitting, identity-preserving renames, symlinks, submodules, cycle recovery and canonical wire envelopes. Storage and replication must also connect reviewed patch identities to admitted graph operations before claiming end-to-end verified merge behavior.
