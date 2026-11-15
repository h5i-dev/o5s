# pico2peta server: HTTP API and protocol reference

The server (`pico2peta-server`) exposes three interfaces on one listener:

| Prefix | What | Clients |
|---|---|---|
| `/v1/...` | the native API: objects, names, namespaces, admin | `pico2peta --remote`, `pico2peta_client::RemoteRepo`, curl |
| `/v2/...` | OCI Distribution v1.1, passing the conformance suite in all four workflows | docker, podman, oras, crane, helm |
| `/git/<repo>.git/...` | Git smart HTTP (protocol v2 for fetch, v0/v1 for fetch and push) | `git clone` / `fetch` / `push` |

Ops endpoints `/healthz`, `/readyz` and `/metrics` need no credentials. Move `/metrics` to a private address with `server.metrics_listen`. The wire types live in `crates/client/src/wire.rs`.

## Authentication

| Form | Accepted on | Notes |
|---|---|---|
| `Authorization: Bearer <token>` | all | static tokens (SHA-256 in config), or OIDC JWTs if `[auth.oidc]` is set |
| `Authorization: Basic base64(<any user>:<token>)` | all | what `docker login` and git credential helpers send |
| none | all | only if `auth.allow_anonymous = true`; the principal is `anonymous` |

Responses:
- A bad or missing credential gets `401`. `/v2` and `/git` add `WWW-Authenticate: Basic realm="pico2peta"`, so clients prompt or retry with credentials.
- A bad token is never downgraded to anonymous.

Tokens:
- Create a static token with `pico2peta-server gen-token <principal>`. Revoke it by deleting its entry and sending `SIGHUP`.
- OIDC tokens map a claim (default `sub`) to `<principal_prefix><claim>`.

## Authorization

Policies grant `read < write < admin` on namespace prefixes (segment-aligned; `""` means everything).

| Operation | Needs |
|---|---|
| resolve or list a name | `read` on the name (listings are filtered) |
| `replace` / `replace_many` / `update_many` / `delete_ref` | `write` on every target, plus `read` on every read-set name (a conflict reveals versions) |
| `set_rule` | `admin` on the prefix |
| `PUT /v1/objects` | `write` on some namespace |
| `GET /v1/objects/{hash}` | `read` on some namespace (objects are global and content-addressed) |
| `oci/…`, `git/…`, `_prov/…` through `/v1` | global `admin`, because these namespaces are managed by the adapters |
| OCI pull / push | `read` / `write` on `oci/<repo>` |
| git fetch / push | `read` / `write` on `git/<repo>` |
| `POST /v1/admin/gc` | `admin` on `""` |

Every denial is audited.

## Native API (`/v1`)

All bodies are JSON unless noted. Every error has the form `{"error":{"code","message","rule?","subject?"}}`, where `code` is a stable string:

| code | HTTP | meaning |
|---|---|---|
| `not_found` | 404 | object does not exist |
| `ref_not_found` | 404 | name does not exist |
| `invalid_range` | 416 | range outside the object (`Content-Range: bytes */<len>`) |
| `invalid_name`, `parse`, `bad_request`, `target_not_in_read_set`, `hash_mismatch` | 400 | bad input |
| `missing_object` | 422 | replace points at an object that does not exist |
| `forbidden` | 403 | namespace rule (`rule` says which) |
| `permission_denied` | 403 | authorization |
| `unauthenticated` | 401 | |
| `rule_change_on_non_empty` | 409 | |
| `payload_too_large` | 413 | `server.max_upload_bytes` |
| `overloaded` | 429 or 503 | rate limit (429) or concurrency limit (503); honour `Retry-After` |
| `shutting_down` | 503 | draining |
| `backend`, `corrupt`, `io`, `internal` | 5xx | server-side failure |

### Objects
```sh
# put (streamed; returns {"hash","size"}). Optional end-to-end check:
curl -T big.bin -H "Authorization: Bearer $T" -H "x-p2p-expect-hash: <blake3 hex>" $S/v1/objects
# get, whole or ranged (206 with Content-Range); HEAD for the size
curl -H "Authorization: Bearer $T" -r 0-1023 $S/v1/objects/<hash>
# make earlier puts durable without a replace
curl -X POST -H "Authorization: Bearer $T" $S/v1/flush
```
- `PUT /v1/objects?group_width=2|4` stores chunks byte-grouped before zstd (bf16/f16 2, f32 4). `for_name=<name>` picks that namespace's chunker. Both are storage hints only: the identity is unchanged and they combine.
- `PUT /v1/objects?delta_base=<hash>` stores the new object as an XOR delta against `<hash>` (for versions of model weights). Its identity is still the content hash, and reads are unchanged.
- Objects are immutable. The ETag is the hash, and responses are `Cache-Control: private, immutable`.
- **Lease:** a newly put object that no name references is reclaimed by GC after `gc.grace_secs` (default 1 h). Point a name at it within that time, or put it again.

### Names (OCC)
```sh
curl -H "Authorization: Bearer $T" $S/v1/refs/team/app/heads/main      # {"name","hash","version"}
curl -H "Authorization: Bearer $T" "$S/v1/refs?prefix=team/app"         # {"refs":[...]}
# compare-and-swap: commits iff every read-set entry still matches (null = absent)
curl -X POST -H "Authorization: Bearer $T" $S/v1/replace -d '{
  "name":"team/app/heads/main",
  "read_set":[{"name":"team/app/heads/main","observed":3}],
  "new":"<hash>"}'
# -> 200 {"outcome":"ok","version":4}  or  409 {"outcome":"conflict","stale":[{"name":...,"observed":5}]}
```
- `POST /v1/replace_many` takes `{"updates":[{"name","new"}],"read_set":[...]}`. All updates commit atomically, or none do.
- `POST /v1/update_many` is the same, but `"new": null` deletes the name.
- `DELETE /v1/refs/<name>` takes `{"read_set":[...]}`. It is OCC; create-once names cannot be deleted. Re-creating a name continues its version sequence.

### Namespaces
```sh
curl -H "Authorization: Bearer $T" $S/v1/namespaces                     # {"rules":[{"prefix","rule"}]}
curl -X PUT -H "Authorization: Bearer $T" $S/v1/namespaces/pkgs -d '{"rule":"immutable"}'
```
The rules are `mutable`, `immutable` (create-once) and `head-release`, where `<ns>/heads/*` is mutable, `<ns>/releases/*` is create-once, and anything else is refused. A rule can only be set while no names exist under the prefix.

### Admin
- `POST /v1/admin/gc?dry_run=true&repack_all=false` returns a GC report. It advances the plan → sweep → delete protocol. With `repack_all=true` and `[gc] grouped = true` (opt-in), git content is regrouped in the Experiment 1 layout.
- `GET /v1/whoami` returns `{"principal","authenticated"}`.
- Token management (admin on `""`; needs `auth.token_file`):
  - `POST /v1/admin/tokens {"principal":"ci"}` returns `{"token","sha256"}`. The token is shown once and only its hash is stored.
  - `GET /v1/admin/tokens` lists principals and hashes.
  - `DELETE /v1/admin/tokens/<sha256>` revokes a token immediately.

## OCI distribution (`/v2`)
```sh
docker login p2p.example.com -u any -p "$TOKEN"
docker push p2p.example.com/team/app:1.0      # OCI repo team/app -> namespace oci/team/app
oras push p2p.example.com/team/app:sbom ./sbom.json --artifact-type application/spdx+json
```
- Supported: chunked and monolithic uploads, cross-repo mount (needs read on the source repo), tags/list with pagination, the referrers API (`OCI-Subject`, `artifactType` filter), and manifest or tag DELETE.
- Blob DELETE returns 405; GC reclaims unreferenced blobs.
- Tag mutability is per-namespace configuration. For example, `PUT /v1/namespaces/oci/team/app/tags {"rule":"immutable"}` makes the tags of that repo create-once.

## Git (`/git`)
```sh
git clone https://any:$TOKEN@p2p.example.com/git/team/proj.git
git push origin main            # branch update = compare-and-swap on git/team/proj/refs/heads/main
git push --atomic origin main v1.0 :old-branch
```
- Refs map to names `git/<repo>/refs/...`, so namespace rules apply. For example, make `git/<repo>/refs/tags` immutable to forbid moving or deleting tags.
- A stale push gets `ng … fetch first`.
- Served packs are deltified against the previous version of each path.
- Limits are listed in `docs/NET_PROGRESS.md` under "Production gaps": SHA-1 repos only, no shallow or partial clones.

## Hugging Face Hub (`/api`, `/<repo>/resolve`)
```sh
export HF_ENDPOINT=https://p2p.example.com HF_TOKEN=$TOKEN
hf upload team/tiny ./model     # or HfApi().upload_folder(...); hf_hub_download / snapshot_download work too
git clone https://any:$TOKEN@p2p.example.com/git/hf/models/team/tiny.git   # the same repo, as Git
```
- The HF repo `team/tiny` of type model/dataset/space is the Git repo `hf/<type>s/team/tiny`. Authorization uses its namespace `git/hf/models/team/tiny`, so one grant covers both the HF API and `git`.
- Small files are Git blobs. Files of 64 KiB or more, or with NUL bytes, go through Git LFS: the repo holds pointer blobs, and the content is a core object stored with the namespace's chunker (`[chunking]`, e.g. Xet).
- Supported: repo create, info (`siblings`, `files_metadata`), tree listing, preupload, the LFS batch API with signed one-hour upload and download URLs, NDJSON commits (add, LFS add, delete file, delete folder; `parentCommit` is checked), and `resolve` with `Range`, `ETag`, `X-Repo-Commit` and `X-Linked-*` headers. Any commit id or branch works as a revision.
- Xet uploads (the default with `hf_xet` installed) are served at `/xet` (`casUrl` from `xet-write-token`). Xorbs are checked against their Merkle hash, and shard segments against their verification hashes. Each file is rebuilt and checked against its Xet file hash and sha256, then stored like an LFS upload.
- LFS objects are scoped to a repo: a commit, `resolve` or download can only reference an object uploaded to that repo, so knowing another repo's sha256 is not enough.
- Files uploaded through Xet also download through Xet: `resolve` advertises `X-Xet-Hash`, and reconstructions use signed xorb URLs. Files uploaded over LFS download over plain HTTP.
- Not yet: global Xet dedup queries, pull requests, tags/branches via the HF API (use `git push`), and the repo settings and delete APIs.
- Disable it with `[hf] enabled = false`. It requires `[git]`.

## PyPI (`/pypi/<index>`)
```sh
twine upload --repository-url https://p2p.example.com/pypi/team/ -u token -p "$TOKEN" dist/*
pip install --index-url "https://token:$TOKEN@p2p.example.com/pypi/team/simple/" mypkg
uv pip install --index-url "https://token:$TOKEN@p2p.example.com/pypi/team/simple/" mypkg
```
- Simple API (PEP 503 HTML and PEP 691 JSON, with sha256, `requires-python` and PEP 658/714 core metadata: `<wheel>.metadata`). Uploads use the legacy API that twine and `uv publish` speak. Authorization is on `pypi/<index>`.
- Files are create-once: an identical re-upload is a no-op, while different bytes under the same filename get 409.
- Proxy-cache: `[pypi.upstream.<index>] url = "https://pypi.org/simple"` (optional `token`, `ttl_secs`). Project pages merge local uploads with upstream files. An upstream file is fetched and sha256-checked on first download, then served from the store, including while the upstream is down.

## npm (`/npm/<registry>`)
```sh
# ~/.npmrc
registry=https://p2p.example.com/npm/team/
//p2p.example.com/npm/team/:_authToken=${TOKEN}
npm publish            # plain or scoped (@scope/name)
npm install left-pad
```
- Packuments, tarballs, and publish (the JSON body that npm sends). On publish, the tarball's sha512 and sha1 are checked against `dist.integrity` and `dist.shasum`. Versions are create-once (409 on republish with other bytes). Dist-tags come from the publish. Authorization is on `npm/<registry>`.
- Proxy-cache: `[npm.upstream.<registry>] url = "https://registry.npmjs.org"`. Upstream versions are merged in, and tarballs are fetched and integrity-checked on first install, then served from the store. When the upstream is unreachable, packuments list only cached versions, and `latest` falls back to the newest cached one.
- `npm dist-tag ls|add|rm` work (for versions in that registry; `latest` can't be removed).
- Not yet: unpublish, deprecate, and search.

## Maven (`/maven/<repo>`)
```xml
<!-- settings.xml: credentials (Basic, password = token) and Central through pico2peta -->
<servers><server><id>p2p</id><username>token</username><password>${env.TOKEN}</password></server></servers>
<mirrors><mirror><id>p2p</id><mirrorOf>central</mirrorOf><url>https://p2p.example.com/maven/central</url></mirror></mirrors>
```
```sh
mvn deploy -DaltDeploymentRepository=p2p::https://p2p.example.com/maven/releases
```
- This is the plain repository layout (GET, HEAD, PUT). Checksums (`.md5`, `.sha1`, `.sha256`, `.sha512`) are computed from the stored file, and a checksum PUT must match it.
- Release files are create-once: re-deploying with other bytes gets 409. `maven-metadata.xml` and `-SNAPSHOT` versions are mutable. Authorization is on `maven/<repo>`.
- Proxy-cache: `[maven.upstream.central] url = "https://repo.maven.apache.org/maven2"`. A miss is fetched, checked against the upstream's `.sha1`, and stored. Mutable upstream files are refreshed after `ttl_secs`, and the cached copy is served if the upstream is down.

## Operations
See `deploy/README.md` for install, TLS, reload (SIGHUP), S3 credentials and lifecycle rules, metrics, audit verification (`pico2peta-server verify-audit`) and capacity notes.
