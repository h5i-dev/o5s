# Proofs and the trust boundary

Every function named here is Rust in the subset Charon and Aeneas translate to Lean 4. `h5i app extract` writes the translation to `proofs/generated/` (kernels) and `crates/registry/proofs/generated/` (registry), and CI checks that the committed Lean is exactly what the current Rust extracts to. `h5i app check` builds the proofs and refuses any `sorry`, `native_decide` or `axiom`. It also refuses a theorem that depends on anything beyond `propext`, `Classical.choice` and `Quot.sound`.

Statements still to be proved live in `specs/`. They are type-checked against the extracted code (`lake build Specs` in `proofs/`) but are outside the gate. When one is proved, it moves into the gated directory and its theorems are added to `h5i-app.toml`.

## What runs extracted code

| Decision | Extracted function | Caller |
|---|---|---|
| Every registry commit: repository directory, patch registration, reviews, check reports, forks, admission | `o5s_registry::transition`, `commit` | both storage engines, native server and Worker |
| Admission of one patch | `patch::merge` | the registry kernel |
| Validating, committing and materializing a graph document | `document::validate`, `commit`, `materialize` | `model::DocumentGraph` (the native adapter interns identities) |
| Graph dependencies and nearest file heads of a proposal | `causality::frontier` | `documents::nearest`, `dependencies` |
| Whether a line base is the file's sole causal head | `causality::sole_head` | `Repository::prepare_lines` |
| One proposal's line ranges, merging concurrent line versions, rendering | `lines::ranges_valid`, `merge`, `render` | `prepare_lines`, `LineVersion::merge_all`, `render` |
| Live-boundary summaries, reachability, frontiers, deletion tags, line boundaries, byte rendering | `indexed::*`, `order::*`, `tags::visible`, `coordinates::line_boundaries`, `interval::*` | the above |

## Proved and gated

**Registry** (`crates/registry/proofs`, 397 theorems in 13 modules):

| Guarantee | Theorem |
|---|---|
| The extracted `transition` computes a pure model of every command | `Refine.transition_refines` |
| Every reachable state keeps the invariant: dependency-closed channels, independent reviews and checks, unique and fresh ids | `Invariant.inv_preserved`, `reachable_inv` |
| Registered patches never change; channel membership only grows | `patches_immutable`, `members_append_only` |
| Only approved patches with a passing check report are admitted | `Guarantees.merge_gate` |
| A repository is visible only to its own organization | `isolation`, `scopes_separate` |
| A per-command partial load computes what the whole repository specifies | `frame`, `Narrow.narrow_load` |
| The store holds what committing computes | `Storage.stored` |
| The PostgreSQL loader (`pgLoad`, a model of `service.rs::load` branch by branch) returns narrow loads, so `narrow_load` holds for what PostgreSQL actually returns. It needs dependency rows stored in key order, which the native `prepare` ensures and the kernel does not yet require | `Served.pg_load_narrow`, `pg_served` |

**Kernels** (`proofs`, 518 theorems in 17 modules), among them:

| Guarantee | Theorem |
|---|---|
| Admission follows review, check and dependency guards; deliveries converge as sets | `Patch.merge_spec`, `delivery_converges` |
| Live-boundary summaries, indexed reachability and boundaries are exact | `Constructor.boundary_spec`, `Indexed.reachable_spec`, `Boundary.boundary_segment` |
| Interval conflicts, splices and checked multi-range rendering | `Interval.conflict_spec`, `Splice.extracted_disjoint_commutes`, `Checked.checked_render_spec` |
| Line boundaries, Kahn frontiers, deletion tags | `Coordinates.line_boundaries_ordered`, `Order.frontier_counts_exact`, `Tags.visible_iff_no_active_tag` |
| Line merging: success means no reversed range and no two different ranges conflict, failure is never spurious, and any two sorted arrangements of the same ranges merge identically | `LinesModel.merge_ok`, `merge_fails`, `merge_order_independent` |
| The extracted `lines` functions compute that model: `sorted`, `merge`, `ranges_valid` (exactly sorted, no reversed range, no two conflicting ranges), `chunks` and `render`; a successful merge of sorted, in-bounds ranges always renders | `Lines.merge_spec`, `merge_correct`, `merge_order_independent`, `ranges_valid_spec`, `render_spec`, `merged_renders` |
| `causality::frontier` returns exactly the nearest stopping patches; `sole_head` decides that a base is the file's only causal head | `Causality.frontier_spec`, `frontier_covers`, `sole_head_spec` |
| Graph documents, on an identity-based model (`DocModel`): validation reads only the patch's causal past; independent admissions commute; a refusal other than a missing dependency is refused in every history; admissibility does not depend on the delivery order; any two complete topological deliveries of the same patches have the same view | `DocModel.validate_local`, `apply_comm`, `rejection_stable`, `admissible_any_order`, `view_converges` |
| The specified view depends only on the patch set, always exists and is unique; bytes exist exactly when the live atoms are totally ordered and no delete/insert conflict is open, and follow `Prec`; a cycle of deleted atoms blocks nothing; restoring and acknowledging are specific; retracting an order removes exactly its edge | `view_perm`, `view_exists`, `view_unique`, `clean_iff`, `bytes_order`, `dead_cycle_harmless`, `restore_clears`, `ack_pair_specific`, `restore_other_cannot_revive`, `retract_breaks` |

## Stated, not yet proved (`specs/`)

| File | Statements |
|---|---|
| `specs/Document.lean` | The extracted `validate` accepts exactly what the model accepts, with the same error; a validated patch always commits; the decoded `materialize` result is the specified view; two replicas with the same patches in different orders materialize the same view (`extracted_view_converges`, proved from `materialize_refines` and the model) |

Their assumptions are explicit: patch identities are unique in a history (SHA-256 collision resistance), histories are dependency-closed (the adapter refuses missing dependencies), the adapter's sizes stay below `Usize.max`, and PostgreSQL's `ORDER BY` and `WHERE` behave as specified. `DocModel`'s theorems take the first two as hypotheses; `replay` establishes both.

## Native and trusted

- Authentication (HMAC tokens), SHA-256 identities, canonical patch bodies, JSON and base64.
- The document adapter's interning and the sorting of diagnostics by identity. Its equality compares the identity-level state, so tests compare replicas across delivery orders.
- Parsing commands, path and name checks, UTF-8 decoding, and building views from registry rows.
- Git behaviour: the native exporter, `crates/gitpush` (tested against `git receive-pack`), the CLI's use of Git, and Cloudflare Artifacts.
- Storage engines: the write-ahead log and the PostgreSQL service (`docs/storage.md`), apart from what `Storage.stored` and `Narrow` cover.

Allocation failure, compiler correctness and the correctness of Charon and Aeneas themselves are outside every theorem.

## Mutation tests

`h5i-app.toml` lists source mutations, such as reversing the approval guard or moving an interval boundary, that must compile and extract and then fail the proofs. `h5i app mutate . --only <prefix>` runs them.
