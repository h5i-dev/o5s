# Proofs and the trust boundary

`h5i app extract o5s` extracts `o5s/kernel/src/patch.rs::merge` through Charon and Aeneas into `o5s/proofs/generated/O5sKernel.lean`. The platform's `h5i_app_core::Kernel` adapter calls that same Rust function. This is a refinement proof of executable admission code, rather than a separately implemented toy admission function.

`Patch.lean` proves:

| Theorem | Statement |
|---|---|
| `merge_spec` | Success/refusal exactly follow review, check and dependency guards; successful output is set insertion. |
| `merge_authorized` | A successful extracted execution has approval, passing checks and all dependencies present. |
| `no_lost_patch` | Every earlier patch remains after successful admission. |
| `dependency_closed` | An existing dependency-closed channel stays closed when the registry-supplied dependency list is admitted. |
| `successful_orders_converge` | Two patches successfully admitted in either order give equal set membership, using actual extracted executions. |
| `insert_idempotent` | Repeated insertion has the same membership. |
| `delivery_converges` | Arbitrarily long finite deliveries with equal patch membership converge in the set specification. |

Set equality means equal membership, not equal vector order or equal Git commit hashes. Dependent patches must be delivered in an admissible order; a missing dependency is refused. The capacity precondition excludes a vector length beyond `Usize.max`. Allocation failure, compiler correctness and extraction correctness are outside the theorem.

The registry assigns local numeric handles to full SHA-256 records; these handles are not hashes and are not shared across independent replicas. Content, author, intent and dependency hashes enter the immutable identity. Reviews attach to immutable records. Approvals/check facts and dependencies are supplied by the authenticated server, never by the caller of the merge command. The proof assumes these inputs correctly describe that record. Cryptographic identity, reviewer independence and tenant authorization are not themselves proved in Lean.

## Graph projection

`kernel/src/graph.rs::reachable` delegates to h5i-app's graph primitive and is also extracted. `Projection.lean` proves `live_reachability`: an exact live-boundary summary preserves reachability between live endpoints, including cyclic graphs. `incremental_summary` proves that replacing only touched-source edges preserves the summary invariant **provided** the old and newly computed summaries are correct and dead-interior reachability outside the touched components is unchanged. `extracted_projection` relates two successful executions of the actual extracted Rust search, under that summary invariant, live endpoints, a singleton start and vector-capacity bounds.

`Constructor.lean` now proves `boundary_spec` for the actual extracted Rust constructor: its output contains precisely live endpoints reached by nonempty dead-interior walks. The proof covers the edge filters, initial successor list, search and live filter, under a vector-capacity bound. `summary_rows_spec` additionally proves the outer Rust loop produces the correct row for every live vertex, in order.

The optimized native summary constructor and weak-component finder in `platform/src/projection.rs` are **not extracted or proved**. Exhaustive tests check every directed three-vertex graph and liveness assignment against the extracted Rust primitive and the universally proved boundary constructor; additional tests cover edge insertion through deleted vertices, restoration, component isolation and a 10,000-vertex dead chain. The proof defines the invariant the constructor must satisfy; these tests supply implementation evidence, not a universal constructor proof. This graph module is separate from whole-file materialization. It does not prove file order, conflict rendering, byte-range editing or the completeness of Pijul's algorithm.

## Runtime scope

The file view uses causal multi-value registers: an edit supersedes ancestor edits on its path, concurrent values are retained, and equal concurrent values collapse in the displayed file while provenance stays in the registry. File/directory collisions and delete-versus-modify conflicts are explicit. Integration order independence, conflict preservation and resolution behavior are integration-tested. The materializer has not been extracted or proved; the set theorem must not be advertised as a proof of all merge behavior.

`h5i app check` scans hand-written proof modules and gates every theorem against only `propext`, `Classical.choice` and `Quot.sound`. The manifest additionally requires key admission theorems by name. Declared mutation tests reverse the approval, check and dependency guards in the actual Rust source; each must compile/extract and then fail the Lean proof. An invalid edit or compilation failure is not a caught semantic mutant.

The retained `Spec`, `Proofs`, `Storage` and generated `Schema` are regression proofs for the former arithmetic sample. Their PostgreSQL theorem does not describe the new local JSON snapshot store. The native store serializes transactions under a process-wide mutex, uses a process-lifetime file lock, writes a candidate snapshot and atomically replaces the state file. Tests cover concurrent API requests, rejected admission, restart and competing process locks.

Pinned Lean/Aeneas/h5i versions are recorded in `o5s/proofs/lean-toolchain`, `lakefile.lean` and `lake-manifest.json`. Generated Lean files are committed and regenerated, not manually edited. If interrupted builds exhaust disk and leave a corrupt `.olean`, remove the affected generated build artifact and rebuild; do not change the theorem to bypass the gate.

## Immutable-base line edits

`interval::conflict`, `interval::splice` and `interval::append_range` are extracted Rust. The native line engine calls conflict detection and the zero-copy-prefix append primitive. `Interval.lean` proves exact conflict detection, symmetry, strict-gap safety, the append-slice equation and the byte-splice equation `take(start) ++ replacement ++ drop(end)`. The splice theorem requires ordered, bounded byte offsets and sufficient vector capacity. Native code checks ranges and converts line offsets to UTF-8 byte boundaries. Those conversions, multiple-range composition, the neighbor index and rendering across versions remain tested trust-boundary code; the single-splice proof is not a theorem about the complete text merge engine.

Each line proposal names an immutable base patch and zero-based half-open ranges. The server adds its base dependencies and rejects obsolete or conflicted causal bases. Disjoint concurrent ranges on the same base combine; overlapping distinct replacements, insertion/delete boundary ambiguity, and distinct bases remain conflicts. A newer base may itself be a line proposal's preview. This is a stable-coordinate merge model, not an implementation of Pijul's byte-interval graph algorithm.
