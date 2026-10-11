# Scaling: implemented work and remaining limits

The local platform supports immutable whole-file edits and line ranges anchored to immutable base patches. Disjoint line ranges on a shared base merge automatically; overlapping ranges and incompatible bases retain concurrent values for review. Performance work must preserve those values and stable patch identities.

## Implemented in the file view

`IncrementalView` keeps causal heads for each path and an append cursor for channels managed by admission. Refresh processes only new membership. It searches dependency ancestry only for current heads on edited paths, stops when those heads have been found, and prunes handles older than every target. Handles are monotonically assigned and dependencies must precede their child. Independent edits with no dependencies skip ancestry search entirely. No transitive-closure table is retained.

`merge_batch` admits a dependency-ordered batch atomically and persists one snapshot. A rejected member publishes none of the batch. Each member still runs the actual extracted admission kernel. `/api/view` returns files, conflicts, patch count and work counters without copying the patch registry or history of member IDs. The richer admin endpoint still returns historical records.

Reproduce the in-memory work experiment:

```sh
cargo run -p o5s-platform --example scaling
```

The fixtures use 10,000 patches/vertices. Counters are asserted in tests:

| Fixture | Observed work |
|---|---|
| Deep chain, all editing one path, first materialization | 10,000 patches and 9,999 searched dependency edges |
| Same chain, unchanged append-only refresh | Zero new patches, searched edges or changed paths |
| One appended edit depending on the previous head | One patch, one searched edge, one changed path |
| New path depending on that chain | No ancestry search for that path |
| 10,000 independent agents editing one path | Concurrent values retained; no dependency searches |
| Dead graph chain with only its endpoints live | One summary edge; two live vertices in subsequent reachability |

These counters exclude authentication, hashing, database cloning, snapshot I/O, serialization and output rendering. Printing 10,000 conflicting alternatives necessarily costs at least the output size. Shared ancestry and many heads can still make searches expensive. This is not a benchmark of 10,000 concurrent HTTP clients. Timings printed by the example describe the local run, not a portable throughput guarantee.

## Line-range merge

The range index checks immediate predecessor and successor intervals in a `BTreeMap`, avoiding a quadratic all-pairs overlap pass. Equal edits deduplicate. For k ranges, collecting ranges costs O(k log k); the renderer appends original gaps and replacements once, with work proportional to original and emitted bytes. The actual Rust overlap predicate and append loop have Lean proofs. Exhaustive triples of ranges on four lines, in all six arrival orders, check the index against a separate pairwise specification. A 10,000-agent range fixture merges into one file with exact expected bytes. These are in-memory fixtures, not HTTP throughput measurements.

Proposal preparation walks dependency closure to validate the chosen immutable base. It searches only for current causal heads while scanning that closure, avoiding a retained transitive-closure table. The registry still retains complete per-patch previews, so large common bases incur substantial memory and snapshot costs. No compressed history claim is made.

## Experimental graph summaries

`platform/src/projection.rs` retains original edges and derives shortcut edges from live vertices through dead interiors to the first live boundary. A batch finds affected weak components and rebuilds summaries only there. It supports edge insertion, deletion/restoration of vertex liveness, cycles and disconnected graphs; it does not invent an order between concurrent vertices. Unaffected components reuse their summaries.

The Lean invariant, conditional extracted-search equivalence and universally proved Rust reference constructor are described in [proofs](proofs.md). The indexed native constructor is checked against that reference on every three-vertex graph and liveness assignment; its indexing and component discovery are not universally proved. The summary experiment remains separate from the application's line-range format. Reachability alone does not establish edit semantics or rendering equivalence.

Rebuilding a large affected component can remain expensive. The constructor may traverse its dead subgraph once for each live boundary source, and dense boundary relations can need quadratic shortcut storage. Subsequent indexed reachability costs visited live vertices plus summary edges, not just the number of live lines. The original history remains stored so restoration and later edges remain meaningful. No history garbage collection is claimed.

## Research choices

- [Eg-walker, EuroSys 2025](https://arxiv.org/pdf/2409.14252) separates persistent causal event history from temporary merge state and compresses consecutive operations. Its collaborative-text event graph is different from Pijul's content graph. We adopt batching, lazy reconstruction and cached derived state, without claiming to implement its transformation algorithm or reproduce its reported speedups. Safe block compaction needs an edit/identity model and a proof before it can replace line vertices.
- [Loro's event graph walker](https://loro.dev/docs/concepts/event_graph_walker) and its [MIT implementation](https://github.com/loro-dev/loro) are useful references for version traversal and collaborative text. No Loro code or dependency is incorporated. The application currently needs explicit unresolved alternatives and reviewed resolutions; substituting a text CRDT would not by itself provide those review semantics.
- [Incremental Topological Ordering and Cycle Detection with Predictions](https://arxiv.org/html/2402.11028v1) studies prediction-assisted dynamic graph algorithms. Patch dependency handles already give a topological order by construction, so adding a dynamic ordering algorithm there would add complexity without removing a present bottleneck. Future line-graph ordering must also preserve concurrency; an arbitrary total order is not a conflict-resolution specification. This paper is not evidence that o5s currently implements a near-linear dynamic algorithm.
- [Pijul theory](https://pijul.org/manual/theory) motivates preserving original graph information and treating pseudo-edges as derived summaries. The implementation here is an independent reachability experiment, not a port of libpijul or a complete proof of its patch algebra.

The next production bottleneck is the whole-database snapshot transaction. Database indexing, shared immutable channel membership, pagination, persistent cache checkpoints and snapshot-bound combined-program checks should precede any claim of distributed agent-scale readiness.
