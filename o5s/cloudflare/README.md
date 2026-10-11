# Cloudflare edge and Artifacts adapter

This ESM Worker proxies the local platform API/UI to an axum origin. `/api/artifacts` resolves the bearer token through `/api/repositories` before touching Artifacts. Repository names are prefixed with the authenticated tenant ID; fork names stay under that repository scope. Credentials are returned only to the authenticated caller with `Cache-Control: no-store`. They are never inserted into patch history.

```sh
npm ci
npm test
npm run dev
```

Requires Node 24+ and Wrangler 4.145.0+. Live Artifacts behavior requires an account with Artifacts access; tests use contract mocks and need no credentials. Configure `O5S_ORIGIN` as an HTTPS axum endpoint for deployment. The committed localhost value is a local development default, not an externally reachable deployment.

| Request | Operation |
|---|---|
| `GET /api/artifacts?repo=demo` | Scoped repository information |
| `POST /api/artifacts` with `{ "repo":"demo", "action":"create" }` | Create the Git projection repository |
| `POST` with `{ "repo":"demo", "action":"fork", "fork":"agent1" }` | Fork the current Artifacts repository |
| `POST` with `{ "repo":"demo", "action":"token" }` | Issue a 300-second write token |

Create the coordination repository through axum first. An Artifacts fork is a fork of the remote Git projection and does not create a native patch channel. Neither this adapter nor the native API pushes local exports automatically. That synchronization needs an explicitly configured job/capability; live push, event subscriptions and previews remain unverified.

The implementation uses the [official Workers binding](https://developers.cloudflare.com/artifacts/api/workers-binding/): `create`, `get`, `info`, `fork`, `createToken`, and disposes each temporary repository capability. No Cloudflare resources have been provisioned or deployed during development.
