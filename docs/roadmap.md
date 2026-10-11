# Production roadmap

The current repository implements a local collaboration prototype. It does not yet meet the requirement of thousands of agents editing a repository concurrently. The extracted kernels cover admission, graph searches and byte rendering, and persisted graph proposals now use the actual document kernel for editing and rendering. The native graph adapter and incremental count maintenance remain unproved. Completing a CLI does not close those gaps.

This plan orders the missing work by dependency. Every milestone needs executable evidence before its status changes to complete. Performance figures below are proposed acceptance targets, not measured results. There is no delivery date attached to them.

## Current implementation and missing guarantees

| Area | Existing code | Missing work |
|---|---|---|
| Patch identity | SHA-256 records, dependencies, local numeric handles in `crates/platform/src/lib.rs` | Canonical wire format, cross-replica identity, versioning and malformed-record rejection |
| Text merge | Persisted insertion/deletion/restoration/order graphs and legacy line edits | Empty-file creation, graph path deletion/rename, legacy migration and complete merge semantics |
| Graph projection | Derived live-boundary summaries in `crates/platform/src/projection.rs` | Component-local projection cache for document edits; proof of native adjacency/count maintenance |
| Extracted kernels | Actual admission, graph, interval, indexed traversal, coordinates, frontier and deletion-tag functions | Patch application and incremental updates over the complete edit model |
| Lean | Admission and projection/rendering refinements with explicit assumptions | End-to-end materialization equivalence, preservation by graph updates, transaction refinement |
| Persistence | Local write-ahead log and PostgreSQL transactions running the extracted registry kernel (`docs/storage.md`) | Schema migrations beyond table creation, cross-region replication, proofs of narrow loads |
| Clients | Admin graph inspector and Git-connected CLI with bounded history and automatic graph proposals | Versioned protocol, retry/idempotency, streaming updates and remaining file operations |
| Cloudflare | Worker adapter and mocked Artifacts operations | Deployed integration, synchronization jobs, durable delivery and failure recovery |
| Scale | Authenticated 1,000-author HTTP runs and actual 2,000/8,000-patch graph measurements | Network load, long-history memory, database throughput, overload behavior and recovery measurements |

`crates/kernel/src/lib.rs` is a module index. Its length is not an implementation metric. The small admission kernel deliberately takes identity and approval facts from native code; proving it does not prove those facts or the complete platform.

## M1: Specify the edit model

Write an executable reference model before extending the optimized graph. A patch needs a portable identity, dependencies, stable content anchors and operations whose behavior is defined when other patches arrive later. Local sequence numbers may index records but must never become replicated identities.

Specify insertion blocks, deletion, restoration, file creation/removal and path changes. Define whether restoration is an operation on a previous deletion, and how concurrent deletions combine. A resolution must name the competing operations it resolves. Define what happens when another concurrent operation arrives after a resolution. Undo must have its own semantics; removing a patch from a channel cannot silently violate dependency closure.

Concurrent insertions at the same anchor must retain their alternatives or have an explicit deterministic policy. A total order chosen by an implementation is not sufficient evidence that a conflict was resolved. Specify file/directory collisions, rename/rename, rename/delete, binary blobs, executable modes, symlinks and submodules. Unsupported operations need explicit protocol errors rather than conversion to plain text.

Create a table of these cases with minimal patch histories, expected conflict records and expected output bytes. Include histories with later edits based on an already merged result. Mark the current immutable-base line model as one supported subset during migration.

Completion requires a written operation specification, a slow reference implementation and test fixtures for every defined case. Each unresolved semantic question stays listed as a blocker for graph integration.

## M2: Apply and materialize one patch graph

Implement the reference graph in the pure Rust kernel using types that Charon/Aeneas can extract. Separate stored patch operations from derived graph edges and materialized views. Applying an already present patch must be idempotent; receiving a dependent patch before its prerequisites must yield a defined pending/refused result.

Connect the platform registry to this graph without changing immutable patch identities. Replace the separate whole-file, line-range and graph paths only after differential tests show that supported legacy cases preserve their behavior. Migration must either reconstruct an equivalent graph or retain old records behind a versioned reader.

The reference materializer must produce files and explicit conflicts, including ambiguous order and file structure. Define output equivalence separately from internal vector order and Git commit identity. Git projections must reject unresolved conflicts according to this specification.

Completion requires proposal, merge, resolution and export workflows calling the same graph implementation; replay of stored records must reproduce the same output after restart. The old renderer can remain a test oracle, but it cannot be described as the graph implementation.

## M3: Prove the application graph and its optimized projection

Use `h5i app extract` on the Rust functions called by production code. Establish the following properties with hypotheses stated in the theorem and checked by the caller:

- Patch application preserves unique identities, valid anchors and dependency closure.
- Independent admissible deliveries converge to equivalent graph state and conflict output; dependent deliveries respect prerequisites.
- Deletion/restoration and reviewed resolution preserve the specified concurrent alternatives.
- The extracted production graph materializer implements the text/conflict specification.
- The dense-index adapter and every adjacency update preserve their representation invariant.
- A component update identifies all sources whose dead-interior reachability can change, including component joins and restoration.
- Updated live-boundary summaries preserve reference reachability and materialization. Reachability alone is insufficient for a text-output theorem.
- A validated checkpoint and later replay produce the same graph and output as complete replay.

Existing component and boundary theorems supply parts of these arguments. Their summary/row assumptions must be discharged by the actual update implementation. State byte capacity, allocation, UTF-8 and extraction assumptions in the proof documentation; do not hide them in a theorem count.

Completion requires the extracted graph path to pass the axiom gate without `sorry` or added axioms, and a mapping from each public guarantee to its theorem and caller. Authentication, cryptographic hashing and database guarantees remain separate trust boundaries until their specifications are connected.

## M4: Remove history-wide work from ordinary edits

Measure reference replay, affected-component reconstruction and output separately. Keep stable dense indices, reverse adjacency, reusable visited marks and live-boundary rows. Cache by immutable patch set or a validated channel generation, not by a mutable name alone. Persist cache checkpoints with a graph/schema version and rebuild safely if validation fails.

Avoid copying all channel members on each admission. Use indexed membership and shared immutable storage. Keep operation blocks contiguous when their identity and anchors permit it; split a block only at an edited boundary. Compression must preserve reference behavior after later insertion, deletion and restoration.

Evaluate event-graph walking against the reference algorithm on histories with little concurrency and histories with a large active concurrent region. Accept it only if measurements show lower work or memory and the replay semantics remain specified. Do not transfer speedup figures from another library to o5s.

Component-local summaries have a worst case: many live sources can traverse the same deleted interior and create a dense boundary relation. Add fixtures for this case. Use limits and a defined fallback to reference reconstruction when retaining a summary would exceed its budget. A long connected history must not be assumed cheap merely because it is one component.

Completion requires counters for visited vertices/edges, rebuilt rows, rendered bytes and retained memory. Benchmarks must compare identical histories and outputs, publish the machine/build settings, and identify the cases where the optimized path loses.

## M5: Durable concurrent transactions

Replace serialized whole-database JSON snapshots with indexed PostgreSQL transactions. Command-specific rollback now avoids cloning patch bodies, but writes still serialize the complete database. Store immutable patches/blobs, dependencies, review facts, channel membership, audit records and an outbox with tenant/repository keys. State the serializable transition the native transaction must implement; h5i-app admission by itself does not prove the database layer.

Add constraints for duplicate identities and dangling dependencies. A merge batch must read the guarded review/check facts and publish all of its membership changes atomically. Record request idempotency keys in the same transaction as their effects. After a lost HTTP response, a client must be able to retrieve the original result rather than accidentally create a second operation.

Use bounded retry for serialization failures. Keep hashing, graph reconstruction and Git processes outside long database locks; publish their results with a generation check. Repository-level contention should not block unrelated tenants or repositories. Export must identify the exact admitted snapshot it represents.

Completion requires multi-process concurrency tests, kill/restart tests around commit boundaries, schema migration tests and a restored backup whose replay matches its source. No acknowledged operation may disappear during these tests. Failed/retried requests must have a documented outcome.

## M6: Replication and portable patch exchange

Define a versioned patch envelope and canonical hashing rules, including encoding, dependency order, operation order and blob references. Authenticate requests separately from immutable author/provenance records. Verify record hashes before admission.

Exchange dependency closures in bounded pages. Stage records with missing dependencies without exposing incomplete channel state. Detect corruption, cycles, unavailable blobs and incompatible protocol versions. A replica must not interpret another replica's numeric handle as a patch identity.

Choose the authority for channel admission explicitly. The initial production design should use a transactional authority per repository while replicas exchange immutable records and derived views. Distributed writable channels require a specified consistency model and additional convergence/review guarantees; they cannot be inferred from set union alone.

Completion requires two independently initialized stores exchanging the same patches in shuffled, duplicated and interrupted deliveries. Their conflict output and files must converge after prerequisites arrive. Include conflicting local numeric handles and partitions during integration.

## M7: Git clients, review and snapshot checks

Users create repositories and worktrees with `git init`, `git clone` and `git worktree add`. o5s connects those working copies to a collaboration repository, records proposals and fetches projections into `refs/o5s/*`. Fetching must leave HEAD, the index and working files unchanged. Native Git commands choose when to check out a projection.

Complete CLI coverage for authentication, connection, status, proposal, review, check reports, channel membership, conflicts and resolution. Emit stable JSON and distinct exit codes for authentication, rejected admission, transport failure and local Git errors. Protect saved tokens by origin and tenant. Coordinate metadata shared by native worktrees, and handle cancellation/retry without overwriting user work.

Review facts need immutable subject hashes and policy versions. A code change produces a new subject. Checks on individual patches must remain distinguishable from checks on the combined channel snapshot. Execute combined-snapshot checks in a configured runner, persist results with that snapshot identity, and reject stale results after channel changes. Reported checks must be labelled as reports in both API and UI.

Completion requires subprocess tests against the real API with separate author/reviewer/check identities, staged and unstaged edits, native worktrees, rejected merges, transport failures and conflicts. Add format/binary/mode support according to M1 before promising general Git compatibility.

## M8: Cloudflare delivery and Git projections

Keep the Worker as the edge adapter and Artifacts as the Git projection store. Specify the Axum origin's deployment, PostgreSQL access and service authentication before selecting a container environment. A Cloudflare account is required for live validation; local mocks remain useful but cannot establish service behavior.

Publish projection jobs through the transactional outbox. Workers/Queues consumers need deduplication, bounded retries and a dead-letter path. A projection records its source channel generation and patch set; a delayed job must not overwrite a newer projection. Store delivery status so operators can recover a failed export without replaying an unrelated merge.

Provision tenant-scoped Artifacts repositories and short-lived capabilities. Exercise clone/push, fork disposal, expired tokens, webhook/event delivery and branch preview creation on an actual account. Verify that origin outages and queue redelivery preserve the source snapshot and access boundary.

Completion requires a reproducible deployment, a live two-agent demonstration, projection reconciliation and failure-injection results. Until then, document the adapter as locally tested.

## M9: Measure thousands of concurrent agents

Build a load driver that uses the real authenticated HTTP API and durable storage. Run it from a separate process or host. Separate closed-loop latency tests from offered-load tests so client waiting does not hide saturation.

Use 1,000 and 10,000 active client identities with these workloads: unrelated repositories; separate files in one repository; disjoint edits in one file; overlapping edits in one file; a long dependency chain; a dense concurrent region; and histories containing repeated deletion/restoration. Include 1,000,000 historical operations. Publish edit sizes, conflict rate, batch size, offered request rate, database settings and hardware.

Initial proposed targets for a documented test machine are 1,000 accepted 1 KiB proposals per second, p95 proposal latency below 250 ms, p99 below 1 s, and a 100-patch admission batch below 500 ms at p95. These targets apply to the measured healthy workload; contention and conflict-heavy workloads need separate results. Revise the targets with evidence rather than removing failed measurements.

Measure CPU, resident memory, database writes, queue depth, retries, emitted bytes and recovery time. Conflicting alternatives must be paginated so a hot file cannot produce an unbounded response. Unchanged reads should avoid history replay; one local edit should avoid scanning unrelated history. Include history growth curves, not only a final throughput figure.

Completion requires reproducible runs with all semantic checks enabled, bounded queues under overload, and zero lost acknowledged operations during restart/failure injection. The existing 10,000-element fixtures remain algorithm tests.

## M10: Operate and use the system

Add token rotation/revocation, repository roles, quotas, audit retention and limits on patch size, dependency count, graph work and subprocess execution. The current tenant-wide access policy needs an explicit migration path. Validate filesystem paths and Git object import against the M1 operation policy.

Expose request/transaction/projection IDs, queue lag and materialization work in structured logs and metrics. Define health/readiness checks, database migration procedure, backup/restore procedure and an operator response for a stalled hot repository. Exercise disk-full, corrupted checkpoint, unavailable database and unavailable Artifacts cases.

Keep the h5i UI theme while completing keyboard navigation, focus handling, accessible conflict comparison, pagination and snapshot/check status. Compare alternatives beside their common base and keep the intended resolution visible while reviewing dependencies. Validate layout and interaction in a full browser at desktop and narrow widths.

Completion requires an installation/upgrade guide, tested recovery commands, accessibility checks and an end-to-end workflow usable without manually constructing API requests.

## Next session

Done in this round: the registry kernel (`crates/registry`) decides every committed registry change and is extracted to Lean. The local write-ahead log and PostgreSQL (`h5i_app_pg::Engine`, one SERIALIZABLE transaction per repository, narrow per-command loads, idempotency keys) both run it. Also done: storage parity, concurrency, replica, restart, backup, torn-tail and legacy-import tests; binary, executable, empty and removed files with mixed proposals; the generation-guarded Git export; metrics, request IDs and secret rotation; the Rust load driver and a measured Git baseline.

1. Proved so far for the registry: `apply`/`commit` refinement, the storage round trip, specs of every lookup and validation helper (`Helpers.lean`), and the `register` command spec. In `crates/registry/pending/`, `Commands.lean` holds the remaining command specs (`create` and `approve` are stated; `fork`, `propose`, `check` and `merge` are not yet written). Prove the pending registry statements in `crates/registry/pending/Theorems.lean`: `inv_preserved`, `frame`, `narrow_load` (which justifies the server's `load_for`), `isolation`, `scopes_separate`, `patches_immutable`, `members_append_only` and `merge_gate`. Then add a `PgServedFor` relation that models partial loads, and prove that the invariant holds on PostgreSQL. Move each proved statement into the gated `Proofs` library.
2. Cloudflare: port the PostgreSQL engine to Workers and Hyperdrive, as the earlier calculator Worker did, with Artifacts projections fed by an outbox. Then deploy and run the two-agent demonstration.
3. Raise single-repository PostgreSQL throughput: fewer roundtrips per transaction, group commit of independent commands, and incremental reads of events.
4. Remaining file semantics: identity-preserving renames, symlinks and submodules. Cycle recovery for order edges.
5. Repository roles, quotas and token revocation; open-loop overload tests; and checks executed as snapshot-bound CI rather than reports.

The local PostgreSQL 17 development container is `o5s-platform-pg`, bound to `127.0.0.1:15438`; `scripts/postgres.sh` starts an equivalent one.

## Development and evidence

Complete the production graph semantics in M1 and M2 first. CLI workflow tests and documentation can progress while compilation or extraction runs. M3 starts when those executable semantics exist; M4 optimization must be compared to them. Storage and protocol work can proceed against the specified model, but production readiness requires all milestones.

Run compilation, extraction and `h5i app mutate` in background jobs with preserved logs. Continue independent implementation while those jobs run. Run semantic mutants when a relevant proof or kernel changes; a previously caught mutant is not a reason to spend another development cycle waiting for the same result. Keep full proof gates for commits that change extraction or theorem scope.

Each completed item must link to its implementation, relevant tests/proofs and measured result. Update [status](status.md) with actual remaining limitations. Code size, elapsed time, theorem totals and a roadmap are insufficient evidence that a milestone is complete.
