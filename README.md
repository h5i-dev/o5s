# o5s

A local patch collaboration prototype for concurrent agents, built with Rust, axum and h5i-app. Changes are immutable patches with stable SHA-256 identities and explicit dependencies. Channels select dependency-closed patch sets. Concurrent edits remain visible; a resolution is another reviewed patch. Git provides working copies and exported projections.

The Axum application lives in `crates/server/src/`. The verified registry kernel, which makes every committed registry decision, is in `crates/registry/`, with its Lean proofs in `crates/registry/proofs/`. The verified document and admission kernels are in `crates/kernel/` with proofs in `proofs/`. Collaboration logic is in `crates/platform/`, the agent CLI in `crates/cli/`, the load and Git-comparison driver in `crates/loadgen/`, and the Cloudflare edge adapter in `cloudflare/`.

## Run locally

Requires Rust stable, Git and Python 3. No Cloudflare account or database is needed.

```sh
./scripts/dev.sh              # local write-ahead log in .o5s/
./scripts/dev.sh --postgres   # PostgreSQL 17 in Docker (scripts/postgres.sh)
```

Open **http://127.0.0.1:3000**. The script prints author, reviewer and check-reporter bearer tokens. Paste a token and connect. Create a repository, record patches, switch to an independent reviewer to approve and report passing checks, then integrate. The token is kept only in the page's memory. Local state and the generated development secret live under `.o5s/`, which is ignored by Git.

Storage: without `O5S_DATABASE_URL` the server keeps a checksummed write-ahead log of committed kernel write sets (`o5s-server verify|backup|import-json`). With it, every command runs in one SERIALIZABLE PostgreSQL transaction per repository. Both run the same extracted kernel. See [storage and transactions](docs/storage.md) for idempotency keys, `/metrics`, `/readyz`, request IDs, secret rotation, backups and the export generation guard.

To seed a complete merge demonstration while the server runs:

```sh
python3 scripts/demo.py
```

Select the returned repository in the UI. The demo creates two concurrent changes, shows that opposite integration orders produce the same conflict, adds a resolution patch, verifies both channels converge and exports a real local bare Git repository. Export stops if any conflict remains. Clone the returned local path using ordinary Git. No push is performed.

`O5S_DATA` changes the storage directory; `O5S_BIND` changes the listen address; `H5I_APP_SECRET` overrides the development secret; `O5S_DATABASE_URL`, `O5S_DATABASE_SCHEMA` and `O5S_DATABASE_POOL` select PostgreSQL. Tokens can be issued with `cargo run -p o5s-server -- token TENANT USER` under the same secret. All members of a tenant currently share repository access.

## User and agent CLI

Install with `cargo install --path crates/cli --locked`. Use ordinary `git init`, `git clone` and `git worktree add`, then run `o5s connect NAME` inside that repository. The CLI supports proposals, independent review/check reports, batch admission, conflict resolution and conflict-free fetch to `refs/o5s/main` without changing HEAD or working files. Graph operations use `propose-graph` and stable node identities from `document`. See [CLI usage](docs/cli.md).

## Verify

```sh
./scripts/verify.sh
h5i app mutate . --only merge_
```

The verification script runs Rust workspace tests (the PostgreSQL ones when `O5S_TEST_DATABASE_URL` is set), the Cloudflare adapter tests (Node 24+), and both Lean projects: it extracts the kernels and the registry and runs the h5i-app axiom gate on each. Proof tooling requires `h5i`, `lake` and the pinned extraction toolchain; `h5i app doctor .` checks these dependencies. It may download Lean packages on the first run.

See [production roadmap](docs/roadmap.md), [implementation status](docs/status.md), [proof scope](docs/proofs.md), [scaling and research](docs/scaling.md), [architecture](docs/architecture.md), [validation](docs/validation.md) and [Cloudflare wiring](cloudflare/README.md). Apache-2.0; no Pijul implementation code is incorporated.

## Current boundary

The system runs locally on either storage backend. Every committed registry decision comes from the extracted `o5s_registry::transition`: repository directory, content identities, independent review and check reports, forks and dependency-closed admission. Lean proves that `apply` and the local `commit` compute the specified state, and that the store holds what committing computes (`Storage.stored`). The invariant, frame, isolation and admission theorems are stated in `crates/registry/pending/Theorems.lean`; their proofs are pending. Admission, graph boundary construction, interval conflicts, checked rendering, frontiers and deletion tags are proved in `proofs/`. Hashing, authentication, preparation of line/graph edits, materialization and Git export are tested native code.

Files may be text (graph documents or line/whole-file edits), binary (base64 whole-file edits) or executable; graph documents can be empty and removed, and removal against a concurrent edit is an explicit conflict. Symlinks and submodules are refused. See the [comparison with Git](docs/git-comparison.md) for measured multi-agent integration against `push`/`pull --rebase`, and [load testing](docs/load-testing.md) for storage throughput.

The API also runs on Cloudflare Workers ([crates/worker](crates/worker/README.md)): the same kernel and PostgreSQL service through Hyperdrive, built with `worker-build` and exercised end to end in local `workerd` with `scripts/worker-dev.sh`. The JavaScript Artifacts adapter is mock-tested. No deployment has been run. Distributed replication, automatic semantic conflict resolution and Git smart-HTTP are future work. See the [roadmap](docs/roadmap.md) and [status](docs/status.md).
