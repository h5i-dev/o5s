# o5s on Cloudflare Workers

The o5s API and admin UI as a Rust Worker (`workers-rs` with axum). PostgreSQL is reached through Hyperdrive. The registry kernel, command planning, narrow loads and repository views are the same crates the native server runs (`o5s_pgwire::service`), so the two deployments share one database schema and behaviour. Each command takes a transaction-scoped advisory lock on its repository, which works with Hyperdrive's transaction pooling (see `docs/storage.md`).

Git export goes to Cloudflare Artifacts. `POST /api/export {"repo","channel"}`, and every successful merge after its reply, pushes the channel's conflict-free snapshot as a commit on `refs/heads/<channel>` of the Artifacts repository `<tenant>-<repo>`. The repository is created if missing; it is the same name the JavaScript adapter in `cloudflare/` serves for `info`, `fork` and `token`.

The binding cannot write, so the Worker builds the Git objects and packfile itself (`crates/gitpush`) and pushes over Git's v1 smart HTTP with a 5-minute write token. Each commit message records the registry generation (`o5s verified-admission projection (generation N)`) and the admitted patch hashes. An export never replaces a commit of a newer generation, and does nothing when the remote already holds the same snapshot. The push names the remote's current commit as its old value, so concurrent exports retry instead of overwriting each other. Each push carries every object of the snapshot (no deltas), so its size grows with the repository.

Agent workspaces need a filesystem and answer 501; use the native server or `o5s fetch`. The Worker is named `o5s`, like the dashboard Worker that Workers Builds deploys, and its configuration is `wrangler.toml` at the repository root, so the Workers Builds root directory stays the default `/`.

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

Create the tables once as the database owner; Hyperdrive roles usually cannot run DDL:

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
