# o5s on Cloudflare Workers

The o5s API and admin UI as a Rust Worker (`workers-rs` with axum). PostgreSQL is reached through Hyperdrive. The registry kernel, command planning, narrow loads and repository views are the same crates the native server runs (`o5s_pgwire::service`), so the two deployments share one database schema and behaviour. Each command takes a transaction-scoped advisory lock on its repository, which works with Hyperdrive's transaction pooling (see `docs/storage.md`).

Git export goes to Cloudflare Artifacts. `POST /api/export {"repo","channel"}`, and every successful merge after its reply, pushes the channel's conflict-free snapshot as a commit on `refs/heads/<channel>` of the Artifacts repository `<tenant>-<repo>`. The repository is created if missing. `GET /api/artifacts?repo=` returns its remote and a one-hour read token, for members of the repository's organization only, so agents can `git clone` the admitted snapshots.

The binding cannot write, so the Worker builds the Git objects and packfile itself (`crates/gitpush`) and pushes over Git's v1 smart HTTP with a 5-minute write token. Each commit message records the registry generation (`o5s verified-admission projection (generation N)`) and the hashes of the admitted set's causal heads, the admitted patches no admitted patch depends on; their causal past is the whole set (`Heads.heads_keep_past`), so the message stays short however long the history. An export never replaces a commit of a newer generation, and does nothing when the remote already holds the same snapshot. The push names the remote's current commit as its old value, so concurrent exports retry instead of overwriting each other. Each push carries every object of the snapshot (no deltas), so its size grows with the repository.

Agent workspaces are Artifacts repositories too, one per agent and repository: `<tenant>-<repo>.<user>` (o5s names cannot contain `.`, so they never collide with projections).

1. `POST /api/worktree {"repo","channel"}` puts the channel's snapshot on the branch `o5s/<channel>` of the caller's workspace repository, and returns its remote, that branch, the snapshot commit and a one-hour write token. A repeated call keeps the agent's own branches.
2. The agent clones with ordinary Git (`git -c http.extraHeader="Authorization: Bearer $TOKEN" clone -b o5s/main $REMOTE`), commits on a branch of its own (`work` by default), and pushes it.
3. `POST /api/worktree/propose {"repo","channel","intent","branch"}` finds the newest snapshot commit on that branch's first-parent history and diffs its tree against the branch head through the binding's `readTree` and `readBlob`, without cloning. Unchanged subtrees are never read. The changes become one proposal that depends on the snapshot's causal heads, and so on exactly what the agent saw, built as `o5s propose` builds it: text files become graph operations that keep unchanged lines' identities, so agents editing different lines of one file merge; binary files and repositories with legacy whole-file documents use whole-file edits. Limits and rules match the native server: at most 1,024 files and 4 MiB, binary files as base64, executable bits kept, symlinks and submodules refused.

The Worker is named `o5s`, like the dashboard Worker that Workers Builds deploys, and its configuration is `wrangler.toml` at the repository root, so the Workers Builds root directory stays the default `/`.

## Requirements

- `rustup target add wasm32-unknown-unknown`, `cargo install worker-build@^0.8`
- Node.js 24 and `npx wrangler@4`, or Docker (`scripts/worker-dev.sh` runs Wrangler in `node:24-slim`)

## Local

```sh
scripts/worker-dev.sh            # local PostgreSQL, wrangler dev on 127.0.0.1:18791
H5I_APP_SECRET=worker-dev-secret target/release/o5s-load o5s --url http://127.0.0.1:18791 --agents 50 --concurrency 8
```

On October 11, 2026 this ran in local `workerd` (Wrangler 4, Hyperdrive local mode against PostgreSQL 17). The `disjoint`, `files` and `repositories` workloads with 50 agents completed with zero failed requests and checked contents. A native server on the same database listed and rendered the repositories the Worker created. Local Hyperdrive opens a new connection per request, so these runs (about 20 integrations/s) say nothing about deployed throughput.

## Deploy

Create the tables as the database owner, and run it again before deploying a version that adds tables (the statements are `CREATE … IF NOT EXISTS`, so rerunning is safe). Hyperdrive roles usually cannot run DDL:

```sh
O5S_DATABASE_SCHEMA=o5s o5s-server ddl | psql "$OWNER_URL"
npx wrangler secret put H5I_APP_SECRET
npx wrangler deploy           # from the repository root; runs crates/worker/build.sh: rustup, wasm target and worker-build if missing
```

The root `wrangler.toml` binds the Hyperdrive configuration used by the earlier calculator deployment (`09738769027849ffb2cd540155d430c9`) and places the Worker near it. Tokens issued with `o5s-server token TENANT USER` under the same secret work on both deployments. 
## Deployment

Workers Builds deploys `main` as the `o5s` Worker at `https://o5s.koukyosyumei.workers.dev`, against PlanetScale Postgres (`us-east-1`) through Hyperdrive. The `o5s` schema was created with `o5s-server ddl` as the database owner, and `H5I_APP_SECRET` is set as a Worker secret.

On October 11, 2026, after the deployment:
- `/readyz` returned `{"ready":true}`.
- Tokens from `o5s-server token` were accepted.
- `o5s-load o5s --agents 20 --concurrency 4` admitted all 20 proposals with checked contents and no failed requests. It ran the `disjoint` workload (4.4 integrations/s, proposal p50 472 ms) and the `files` workload (4.8/s, p50 394 ms).

These runs confirm the deployment works; they are not throughput measurements. The test repositories were deleted afterwards.
