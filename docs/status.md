# Implementation status

This repository is a local MVP, not the complete distributed Git platform described in the project brief. Verification of individual kernels does not establish correctness of the entire application.

| Area | Implemented | Remaining boundary |
|---|---|---|
| Patch collaboration | Immutable proposal identities, dependency-closed channels, independent approvals/check reports, atomic batch admission | Review/check facts and authenticated identity are supplied by native code; checks are reports, not an executed CI pipeline |
| Text merge | Whole-file/line conflict alternatives and persisted block-graph insertion, deletion tags, restoration and reviewed order; resolutions are proposals | Native causal-head and interval-index correctness is tested; semantic conflict resolution is manual |
| Graph acceleration | Dense indexed traversal, reusable marks, affected-component live-boundary summaries, extracted reference/refinement proofs | Graph documents call the extracted boundary traversal; the component-local projection cache remains separate, and native graph adapters are unproved |
| Storage | Tenant-scoped local JSON snapshots, atomic replacement and a single-writer lock | No distributed replication or production multi-writer storage |
| Git/worktrees | User/agent CLI with native Git worktrees, proposals, review, checks, conflict resolution and safe snapshot fetch; local bare Git export | No Git smart-HTTP server or automatic remote synchronization |
| Cloudflare | Worker proxy and tenant-scoped Artifacts calls, tested with mocks | No live provisioning, deployment, event/preview pipeline or account integration has been validated |
| Formal verification | Extracted Rust admission, reachability/boundary construction, interval conflicts, checked byte rendering and LF boundary scan | Does not prove native authentication, hashing, filesystem/Git behavior, complete text materialization or Cloudflare runtime behavior |
| Scale | In-memory 10,000-patch/range fixtures and long-history graph traversal experiments | Real authenticated HTTP workload driver and measured small runs; no 10,000-client or distributed throughput claim |

The extraction and proof gate cover the o5s kernels. The axiom gate checks actual extracted Rust refinements as well as supporting mathematical lemmas; a theorem count is not a count of end-to-end product guarantees.

The block-graph path is connected to persisted proposals, admitted channel views and Git exports. Automatic conversion of ordinary Git edits, legacy document migration and component-local updates remain unfinished. Native transaction/persistence behavior and distributed execution also need explicit specifications before end-to-end verification can be claimed. Research-derived compression or incremental ordering should be accepted only with measured benefit and a refinement argument for this model.

The [production roadmap](roadmap.md) defines the missing semantics, implementations, proofs and operational acceptance tests.
