# o5s-worker

This retained arithmetic regression example uses axum on Cloudflare Workers
and PostgreSQL through Hyperdrive. Its kernel/proofs share the workspace
with the new patch platform, but its `/rpc` arithmetic API and database are
separate from `src`. For the patch platform's edge adapter, use
[`../cloudflare`](../cloudflare/README.md).

`h5i-app-pg` and `h5i-app-http` do not build for `wasm32-unknown-unknown`
(tokio's runtime, `deadpool-postgres`, tokio-postgres's default `runtime`
feature), so `src/engine.rs` redoes their glue over tokio-postgres's `js`
feature and a Workers `Socket`. The verified parts are the same crates:
`h5i-app-sql` plans the writes, `h5i-app-pgsql` renders all SQL on tenant
tables, `h5i-app-json` writes every body, `h5i-app-token` parses tokens.

Differences from `h5i_app_pg::Engine`, because Hyperdrive has no advisory
locks and pools connections per transaction:

- no per-tenant advisory lock: SERIALIZABLE alone keeps commits correct, but
  hot tenants retry more (50 concurrent writes to one memory: 40 committed,
  10 gave up after 20 attempts with 503);
- the schema is set per transaction (`SET LOCAL search_path`), not per session;
- `install_schema` is not run by the worker: apply `admin ddl` directly.

## Requirements

- `rustup target add wasm32-unknown-unknown`
- `worker-build` 0.8 (`cargo install worker-build@^0.8`; it needs the OpenSSL
  headers, or build it with the `openssl` crate's `vendored` feature)
- Node.js and `npx wrangler` (v4)

## Local

```sh
docker run -d --name o5s-calc-pg -p 127.0.0.1:55440:5432 -e POSTGRES_USER=h5i -e POSTGRES_PASSWORD=h5i postgres:17
cargo run --release -p o5s-worker --example admin -- ddl | psql postgres://h5i:h5i@127.0.0.1:55440/h5i
echo H5I_APP_SECRET=dev-secret > .dev.vars
CLOUDFLARE_HYPERDRIVE_LOCAL_CONNECTION_STRING_HYPERDRIVE=postgres://h5i:h5i@127.0.0.1:55440/h5i npx wrangler dev

TOKEN=$(H5I_APP_SECRET=dev-secret cargo run -q --release -p o5s-worker --example admin -- issue 1:1)
curl -X POST localhost:8787/rpc -H "Authorization: Bearer $TOKEN" -d '{"cmd":"set","value":6}'
curl -X POST localhost:8787/rpc -H "Authorization: Bearer $TOKEN" -d '{"cmd":"apply","op":"mul","arg":7}'
```

## Deploy

`build.sh` is wrangler's build command: it installs rustup, the wasm target
and worker-build when they are missing (Workers Builds' image has no Rust),
then runs `worker-build --release`. With Workers Builds (Git integration),
set the root directory to `worker` and keep the deploy command
`npx wrangler deploy`; the Worker's name must be `name` in `wrangler.toml`.
Builds are cut off after 20 minutes.

No deployment is configured for this repository. The Hyperdrive ID in
`wrangler.toml` is a placeholder. Provision a database and replace that ID
before deliberately deploying this compatibility example. Apply `admin ddl`
with a role allowed to create the schema.

By hand:

```sh
npx wrangler login
admin ddl | psql "$DATABASE_URL"              # directly, not through Hyperdrive
npx wrangler hyperdrive create o5s-arithmetic --connection-string="$DATABASE_URL"
# put the printed id in wrangler.toml
npx wrangler secret put H5I_APP_SECRET
npx wrangler deploy
```
