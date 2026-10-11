# calculator-worker

The h5i calculator (`../kernel`, proofs in `../proofs`) on Cloudflare Workers,
with axum (as in workers-rs `examples/axum`) and PostgreSQL through Hyperdrive
(as in workers-rs `examples/tokio-postgres`). Same API, tokens and tables as
h5i's `examples/app/calculator/server`, so the two can share a database.

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
cargo run --release -p calculator-worker --example admin -- ddl | psql postgres://h5i:h5i@127.0.0.1:55440/h5i
echo H5I_APP_SECRET=dev-secret > .dev.vars
CLOUDFLARE_HYPERDRIVE_LOCAL_CONNECTION_STRING_HYPERDRIVE=postgres://h5i:h5i@127.0.0.1:55440/h5i npx wrangler dev

TOKEN=$(H5I_APP_SECRET=dev-secret cargo run -q --release -p calculator-worker --example admin -- issue 1:1)
curl -X POST localhost:8787/rpc -H "Authorization: Bearer $TOKEN" -d '{"cmd":"set","value":6}'
curl -X POST localhost:8787/rpc -H "Authorization: Bearer $TOKEN" -d '{"cmd":"apply","op":"mul","arg":7}'
```

## Deploy

```sh
npx wrangler login
admin ddl | psql "$DATABASE_URL"              # directly, not through Hyperdrive
npx wrangler hyperdrive create o5s-calculator --connection-string="$DATABASE_URL"
# put the printed id in wrangler.toml
npx wrangler secret put H5I_APP_SECRET
npx wrangler deploy
```
