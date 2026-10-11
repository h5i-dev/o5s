# o5s

A local patch collaboration prototype for concurrent agents, built with Rust, axum and h5i-app. Changes are immutable patches with stable SHA-256 identities and explicit dependencies. Channels select dependency-closed patch sets. Concurrent edits remain visible; a resolution is another reviewed patch. Git provides working copies and exported projections.

The Axum application lives in `crates/server/src/`, with the verified Rust kernel in `crates/kernel/`, collaboration logic in `crates/platform/`, Lean proofs in `proofs/` and Cloudflare edge adapter in `cloudflare/`.

## Run locally

Requires Rust stable, Git and Python 3. No Cloudflare account or database is needed.

```sh
./scripts/dev.sh
```

Open **http://127.0.0.1:3000**. The script prints author, reviewer and check-reporter bearer tokens. Paste a token and connect. Create a repository, record patches, switch to an independent reviewer to approve and report passing checks, then integrate. The token is kept only in the page's memory. Local state and the generated development secret live under `.o5s/`, which is ignored by Git.

To seed a complete merge demonstration while the server runs:

```sh
python3 scripts/demo.py
```

Select the returned repository in the UI. The demo creates two concurrent changes, shows that opposite integration orders produce the same conflict, adds a resolution patch, verifies both channels converge and exports a real local bare Git repository. Export stops if any conflict remains. Clone the returned local path using ordinary Git. No push is performed.

`O5S_DATA` changes the storage directory; `O5S_BIND` changes the listen address; `H5I_APP_SECRET` overrides the development secret. Tokens can be issued with `cargo run -p o5s-server -- token TENANT USER` under the same secret. All members of a tenant currently share repository access.

## User and agent CLI

Install with `cargo install --path crates/cli --locked`. Use ordinary `git init`, `git clone` and `git worktree add`, then run `o5s connect NAME` inside that repository. The CLI supports proposals, independent review/check reports, batch admission, conflict resolution and conflict-free fetch to `refs/o5s/main` without changing HEAD or working files. Graph operations use `propose-graph` and stable node identities from `document`. See [CLI usage](docs/cli.md).

## Verify

```sh
./scripts/verify.sh
h5i app mutate . --only merge_
```

The verification script runs Rust workspace tests, the Cloudflare adapter tests (Node 24+), extracts the current kernel and runs the h5i-app Lean axiom gate. Proof tooling requires `h5i`, `lake` and the pinned extraction toolchain; `h5i app doctor .` checks these dependencies. It may download Lean packages on the first run.

See [production roadmap](docs/roadmap.md), [implementation status](docs/status.md), [proof scope](docs/proofs.md), [scaling and research](docs/scaling.md), [architecture](docs/architecture.md), [validation](docs/validation.md) and [Cloudflare wiring](cloudflare/README.md). Apache-2.0; no Pijul implementation code is incorporated.

## Current boundary

This prototype runs locally. Admission, the reference and indexed graph boundary constructors, dense-adjacency traversal with reusable marks, range conflict detection and checked multi-range byte rendering are formally verified; authentication, hashing, persistence, causal file materialization and Git export are tested Rust code. Whole-file edits preserve concurrent alternatives. Line-range proposals on a shared immutable base automatically merge disjoint edits and retain overlapping changes as conflicts. Choose “Line range” in the proposal form, specify the base patch and zero-based half-open range, and enter replacement text. Passing checks are an authenticated independent agent's report, not an automatically executed CI job. The local single-writer snapshot store is not a distributed production database. The 10,000-agent tests cover the in-memory kernel and materializer, not 10,000 simultaneous network clients. Batched integration persists once; `/api/view` reuses an append-only cache without sending historical patch records. The admin UI shares the h5i UI palette, puts conflicts before history, and offers filtered review queues and a resolution composer.

Cloudflare calls are implemented and mock-tested; live Artifacts provisioning, Git push, Workers deployment and preview workflows have not been run. The server also persists immutable block-graph operations: insertion, deletion tags, restoration, reviewed order and delete/insert acknowledgements. Admitted graph patches materialize through the same kernel used by the document API and Git export. Ambiguous graphs produce conflicts and cannot be exported. Graph documents and legacy line/file documents currently require separate edit paths; migration is unfinished. The component-local summary experiment remains separate from the runtime document cache. Distributed replication, automatic semantic conflict resolution and Git smart-HTTP are future work.

`POST /api/worktree` with `{ "repo": "demo", "channel": "main" }` creates a detached local Git workspace from a conflict-free admitted snapshot. Reusing the same snapshot preserves local edits and reports `dirty`; it never overwrites an altered HEAD. The admin toolbar exposes this operation.
