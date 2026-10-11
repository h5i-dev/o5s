# Implementation status

This repository is a local MVP, not the complete distributed Git platform described in the project brief. Verification of individual kernels does not establish correctness of the entire application.

| Area | Implemented | Remaining boundary |
|---|---|---|
| Patch collaboration | Every registry decision by the extracted `o5s_registry::transition`: immutable identities, dependency-closed channels, independent approvals/check reports, atomic batch admission, tenant isolation | Authenticated identity and SHA-256 identities are supplied by native code; checks are reports, not an executed CI pipeline |
| Text merge | Whole-file/line conflict alternatives and persisted block-graph insertion, deletion tags, restoration and reviewed order; resolutions are proposals | Native causal-head and interval-index correctness is tested; semantic conflict resolution is manual |
| Graph acceleration | Dense indexed traversal, reusable marks, affected-component live-boundary summaries, extracted reference/refinement proofs | Graph documents call the extracted boundary traversal; the component-local projection cache remains separate, and native graph adapters are unproved |
| Storage | Local checksummed write-ahead log; PostgreSQL with one SERIALIZABLE transaction per repository through `h5i_app_pg::Engine`, narrow per-command loads, idempotency keys, retries and multi-replica reads; backup/verify/import tools; metrics and readiness | Narrow loads rely on the pending `narrow_load` theorem; no cross-region replication; a hot repository is limited by per-transaction roundtrips |
| Git/worktrees | User/agent CLI with native Git worktrees, proposals, review, checks, conflict resolution and safe snapshot fetch; local bare Git export | No Git smart-HTTP server or automatic remote synchronization |
| Cloudflare | Rust Worker (`crates/worker`) running the same kernel and `o5s_pgwire` service over Hyperdrive, built with `worker-build` and exercised end to end in local `workerd`; JavaScript Artifacts adapter tested with mocks | No deployed run; Git projections to Artifacts need an outbox job |
| Formal verification | Registry: `apply`/`commit` refinement and storage round trip (`Storage.stored`); kernels: admission, reachability/boundary construction, interval conflicts, checked rendering, frontiers, tags | Registry invariant, frame, isolation and admission-gate theorems are stated with `sorry` in `crates/registry/pending/`; native authentication, hashing, Git behaviour and Cloudflare runtime are unproved |
| Scale | Rust HTTP driver: 1,000 authors at 64 in flight, local log 800–2,600 proposals/s, PostgreSQL 95–400/s; measured Git baseline | No 10,000-client, open-loop overload or multi-host measurement |

The extraction and proof gate cover the o5s kernels. The axiom gate checks actual extracted Rust refinements as well as supporting mathematical lemmas; a theorem count is not a count of end-to-end product guarantees.

The block-graph path is connected to persisted proposals, admitted channel views and Git exports. Ordinary Git edits to existing graph documents now produce graph operations with their original snapshot dependencies. Initial graph creation from ordinary Git is implemented. Legacy document migration, empty-file creation, graph path deletion and component-local updates remain unfinished. Native transaction/persistence behavior and distributed execution also need explicit specifications before end-to-end verification can be claimed. Research-derived compression or incremental ordering should be accepted only with measured benefit and a refinement argument for this model.

The [production roadmap](roadmap.md) defines the missing semantics, implementations, proofs and operational acceptance tests.
