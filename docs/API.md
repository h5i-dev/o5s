# o5s server: HTTP API and protocol reference

The server (`o5s-server`) exposes three interfaces on one listener:

| Prefix | What | Clients |
|---|---|---|
| `/v1/...` | the native API: objects, names, namespaces, admin | `o5s --remote`, `o5s_client::RemoteRepo`, curl |
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
- A bad or missing credential gets `401`. `/v2` and `/git` add `WWW-Authenticate: Basic realm="o5s"`, so clients prompt or retry with credentials.
- A bad token is never downgraded to anonymous.

Tokens:
- Create a static token with `o5s-server gen-token <principal>`. Revoke it by deleting its entry and sending `SIGHUP`.
- OIDC tokens map a claim (default `sub`) to `<principal_prefix><claim>`.

## Authorization

Policies grant `read < write < admin` on namespace prefixes (segment-aligned; `""` means everything).

| Operation | Needs |
|---|---|
| resolve or list a name | `read` on the name (listings are filtered) |
| `replace` / `replace_many` / `update_many` / `delete_ref` | `write` on every target, plus `read` on every read-set name (a conflict reveals versions) |
| `set_rule` | `admin` on the prefix |
| `PUT /v1/objects` | `write` on some namespace |
| `GET` / `HEAD /v1/objects/{hash}` | the object must be **reachable from a name the caller may read** (see below); otherwise `404 not_found`, exactly as for a missing object |
| `oci/…`, `git/…`, `_prov/…` through `/v1` | global `admin`, because these namespaces are managed by the adapters |
| OCI pull / push | `read` / `write` on `oci/<repo>` |
| git fetch / push | `read` / `write` on `git/<repo>` |
| `POST /v1/admin/gc` | `admin` on `""` |

Every denial is audited.

## Native API (`/v1`)

All bodies are JSON unless noted. Every error has the form `{"error":{"code","message","rule?","subject?"}}`, where `code` is a stable string:

| code | HTTP | meaning |
|---|---|---|
| `not_found` | 404 | object does not exist, **or** no name the caller may read reaches it (the two are indistinguishable by design) |
| `ref_not_found` | 404 | name does not exist |
| `invalid_range` | 416 | range outside the object (`Content-Range: bytes */<len>`) |
| `invalid_name`, `parse`, `bad_request`, `target_not_in_read_set`, `hash_mismatch` | 400 | bad input |
| `missing_object` | 422 | replace points at an object that does not exist |
| `forbidden` | 403 | namespace rule (`rule` says which) |
| `permission_denied` | 403 | authorization |
| `unauthenticated` | 401 | |
| `rule_change_on_non_empty` | 409 | |
| `payload_too_large` | 413 | `server.max_upload_bytes` |
| `overloaded` | 429 or 503 | rate limit, per principal or per namespace (429), or concurrency limit (503); honour `Retry-After` |
| `quota_exceeded` | 507 | the namespace (`subject`) was at or over its `[[limits]] max_stored_bytes` at the last usage scan; `/v1` ref deletes and admins are not blocked |
| `shutting_down` | 503 | draining |
| `backend`, `corrupt`, `io`, `internal` | 5xx | server-side failure |

### Objects
**Reads by hash follow reachability** (since the h5i rebuild, 2026-11-16). Knowing a hash is not a
capability.
- `GET`/`HEAD /v1/objects/<hash>` serve an object only if it is reachable from a name the caller may read:
  through the name's target, its chunks, and its front's references (a Git tree's entries, an OCI
  manifest's layers, an LFS pointer's content, ...).
- Otherwise the answer is `404 not_found`, the same body and the same early refusal as for an object that
  does not exist, so an unreadable object cannot be told from a missing one.
- An object that no name reaches is unreadable to everyone, admins included, until a name points at it.
- Put-then-get therefore needs a name in between, e.g. a `replace` that creates one. The decision is the
  access kernel's (`crates/net-kernel`, proven in `verify/net`: `visible_object_spec`, with
  noninterference for unreadable objects). Which names reach which objects comes from the core's
  reachability index.
```sh
# put (streamed; returns {"hash","size"}). Optional end-to-end check:
curl -T big.bin -H "Authorization: Bearer $T" -H "x-o5s-expect-hash: <blake3 hex>" $S/v1/objects
# name it (a read needs a readable name that reaches the object)
curl -H "Authorization: Bearer $T" -H "Content-Type: application/json" $S/v1/replace \
  -d '{"name":"team/data/a","read_set":[{"name":"team/data/a","observed":null}],"new":"<hash>"}'
# get, whole or ranged (206 with Content-Range); HEAD for the size
curl -H "Authorization: Bearer $T" -r 0-1023 $S/v1/objects/<hash>
# make earlier puts durable without a replace
curl -X POST -H "Authorization: Bearer $T" $S/v1/flush
```
- **`PUT /v1/objects?for_name=<name>`** (stable API) stores the object the way the repository's profile for `<name>`'s namespace says, exactly like `LocalRepo::put_named`:
  - the profile's front encoding plan (e.g. `safetensors`: tensor-aligned byte grouping);
  - its chunker (`fastcdc` / `xet`);
  - `derive = "previous"`: a delta against the object `<name>` points at now.

  It needs write access on `<name>` and doesn't move the name (follow it with a replace). `RemoteRepo::put_named` sends it.
- `group_width=2|4` byte-groups chunks before zstd (bf16/f16 2, f32 4); with `for_name`, it's used only when the front doesn't choose a width. These are storage choices only: the object's identity (BLAKE3 of its bytes) is unchanged.
- `PUT /v1/objects?delta_base=<hash>` stores the new object as an XOR delta against `<hash>` (for versions of model weights). Its identity is still the content hash, and reads are unchanged.
- Objects are immutable. The ETag is the hash, and responses are `Cache-Control: private, immutable`.
- **Lease:** a newly put object that no name references is reclaimed by GC after `gc.grace_secs` (default 1 h). Point a name at it within that time, or put it again.

- `GET /v1/objects/<hash>?verify=strict|eof` (API v1.17): read verification for this request. Without it, the repository's `[read] verify` default applies (strict unless the operator configured otherwise). `eof` is refused with 403 unless the repository allows it (`[read] allow_eof = true`); a bad value gets 400. `RemoteRepo::open_verified` sends it.

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

`prefix` is segment-aligned (`team/app` lists `team/app` and `team/app/...`, not `team/apples`). A trailing `/`
names the same prefix (`team/app/` = `team/app`).
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
- `POST /v1/admin/gc?dry_run=true&repack_all=false` returns a GC report. It advances the plan → sweep → delete protocol. Repacks follow the repository's `[gc] similarity_repack` (on by default; front hints such as git's path/size grouping order the similarity repack). The server's `[gc] grouped` is a deprecated override of that setting.
- `GET /v1/whoami` returns `{"principal","authenticated"}`.
- Token management (admin on `""`; needs `auth.token_file`):
  - `POST /v1/admin/tokens {"principal":"ci"}` returns `{"token","sha256"}`. The token is shown once and only its hash is stored.
  - With `auth.token_key_file` (a file of at least 32 random bytes, the HMAC key), the issued tokens are
    h5i-app tokens, `v1.0.<user>.<expiry>.<hmac-sha256>`. An optional `"ttl_secs"` sets the lifetime
    (default one year). They are parsed by h5i-app-token's extracted, proven parser and checked against
    the key (constant time), the expiry and the token table, so revoking still works. Without a key, a
    random `o5s_…` secret is issued as before. Static `[[auth.tokens]]` entries keep working either way.
  - `GET /v1/admin/tokens` lists principals and hashes.
  - `DELETE /v1/admin/tokens/<sha256>` revokes a token immediately.

## OCI distribution (`/v2`)
```sh
docker login o5s.example.com -u any -p "$TOKEN"
docker push o5s.example.com/team/app:1.0      # OCI repo team/app -> namespace oci/team/app
oras push o5s.example.com/team/app:sbom ./sbom.json --artifact-type application/spdx+json
```
- Supported: chunked and monolithic uploads, cross-repo mount (needs read on the source repo), tags/list with pagination, the referrers API (`OCI-Subject`, `artifactType` filter), and manifest or tag DELETE.
- Blob DELETE returns 405; GC reclaims unreferenced blobs.
- Tag mutability is per-namespace configuration. For example, `PUT /v1/namespaces/oci/team/app/tags {"rule":"immutable"}` makes the tags of that repo create-once.

## Git (`/git`)
```sh
git clone https://any:$TOKEN@o5s.example.com/git/team/proj.git
git push origin main            # branch update = compare-and-swap on git/team/proj/refs/heads/main
git push --atomic origin main v1.0 :old-branch
```
- Refs map to names `git/<repo>/refs/...`, so namespace rules apply. For example, make `git/<repo>/refs/tags` immutable to forbid moving or deleting tags.
- A stale push gets `ng … fetch first`.
- Served packs are deltified against the previous version of each path.
- Shallow clones (`--depth`, `--deepen`, `--shallow-since`, `--unshallow`) and partial clones (`--filter=blob:none`, `blob:limit=<n>`, with lazy fetches of missing objects) work over protocol v2 (git's default). A want that isn't a ref tip is served only if it's reachable from this repo's refs.
- SHA-256 repositories (`git init --object-format=sha256`): configure their name prefixes in `[git] sha256_repos = ["team/s256"]`, since git learns a repository's format before its first push. Every operation works in both formats, and a push in the other format is refused.
- Derived data, refreshed in the background after every push. Both are ordinary objects behind refs, so GC keeps the current ones and reclaims stale ones; losing them costs speed, never correctness:
  - `git/_commitgraph/<repo>`: commit and tag graph, used for negotiation and history walks;
  - `git/_clonepack/<repo>`: the pack of the last full clone (heads+tags), streamed as-is to the next clone with the same wants. `[git] clone_pack_cache = false` turns it off; it costs one pack of storage per cloned repository.
- Pushes are indexed in parallel (index-pack style) and stored in batches.
- Limits are listed in `docs/NET_PROGRESS.md` under "Production gaps": `deepen-not` is unsupported.

## Hugging Face Hub (`/api`, `/<repo>/resolve`)
```sh
export HF_ENDPOINT=https://o5s.example.com HF_TOKEN=$TOKEN
hf upload team/tiny ./model     # or HfApi().upload_folder(...); hf_hub_download / snapshot_download work too
git clone https://any:$TOKEN@o5s.example.com/git/hf/models/team/tiny.git   # the same repo, as Git
```
- The HF repo `team/tiny` of type model/dataset/space is the Git repo `hf/<type>s/team/tiny`. Authorization uses its namespace `git/hf/models/team/tiny`, so one grant covers both the HF API and `git`.
- Small files are Git blobs. Files of 64 KiB or more, or with NUL bytes, go through Git LFS: the repo holds pointer blobs, and the content is a core object stored with the namespace's chunker (`[chunking]`, e.g. Xet).
- Supported: repo create, info (`siblings`, `files_metadata`), tree listing, preupload, the LFS batch API with signed one-hour upload and download URLs, NDJSON commits (add, LFS add, delete file, delete folder; `parentCommit` is checked), and `resolve` with `Range`, `ETag`, `X-Repo-Commit` and `X-Linked-*` headers. Any commit id or branch works as a revision.
- Xet uploads (the default with `hf_xet` installed) are served at `/xet` (`casUrl` from `xet-write-token`). Xorbs are checked against their Merkle hash, and shard segments against their verification hashes. Each file is rebuilt and checked against its Xet file hash and sha256, then stored like an LFS upload.
- LFS objects are scoped to a repo: a commit, `resolve` or download can only reference an object uploaded to that repo, so knowing another repo's sha256 is not enough.
- Files uploaded through Xet also download through Xet: `resolve` advertises `X-Xet-Hash`, and reconstructions use signed xorb URLs. Files uploaded over LFS download over plain HTTP.
- Branches, tags and history: `create_branch`/`delete_branch` (`startingPoint`), `create_tag`/`delete_tag` (lightweight; annotated tags pushed with git are peeled), `list_repo_refs` and `list_repo_commits` (newest first, up to 1000). Any revision may be a branch, a tag or a commit id.
- Not yet: global Xet dedup queries, pull requests, and the repo settings and delete APIs.
- Disable it with `[hf] enabled = false`. It requires `[git]`.

## PyPI (`/pypi/<index>`)
```sh
twine upload --repository-url https://o5s.example.com/pypi/team/ -u token -p "$TOKEN" dist/*
pip install --index-url "https://token:$TOKEN@o5s.example.com/pypi/team/simple/" mypkg
uv pip install --index-url "https://token:$TOKEN@o5s.example.com/pypi/team/simple/" mypkg
```
- Simple API (PEP 503 HTML and PEP 691 JSON, with sha256, `requires-python` and PEP 658/714 core metadata: `<wheel>.metadata`). Uploads use the legacy API that twine and `uv publish` speak. Authorization is on `pypi/<index>`.
- Files are create-once: an identical re-upload is a no-op, while different bytes under the same filename get 409.
- PEP 592 yank: `PUT /pypi/<index>/-/yank/<project>/<file>` (the body is the reason) and `DELETE` to unyank, with write access. pip skips yanked files unless they're pinned exactly.
- Proxy-cache: `[pypi.upstream.<index>] url = "https://pypi.org/simple"` (optional `token`, `ttl_secs`). Project pages merge local uploads with upstream files. An upstream file is fetched and sha256-checked on first download, then served from the store, including while the upstream is down.

## npm (`/npm/<registry>`)
```sh
# ~/.npmrc
registry=https://o5s.example.com/npm/team/
//o5s.example.com/npm/team/:_authToken=${TOKEN}
npm publish            # plain or scoped (@scope/name)
npm install left-pad
```
- Packuments, tarballs, and publish (the JSON body that npm sends). On publish, the tarball's sha512 and sha1 are checked against `dist.integrity` and `dist.shasum`. Versions are create-once (409 on republish with other bytes). Dist-tags come from the publish. Authorization is on `npm/<registry>`.
- Proxy-cache: `[npm.upstream.<registry>] url = "https://registry.npmjs.org"`. Upstream versions are merged in, and tarballs are fetched and integrity-checked on first install, then served from the store. When the upstream is unreachable, packuments list only cached versions, and `latest` falls back to the newest cached one.
- `npm dist-tag ls|add|rm` work (for versions in that registry; `latest` can't be removed).
- `npm deprecate` (and clearing it with `""`), `npm unpublish <pkg>@<v>` and `npm unpublish <pkg> --force` work. Packuments carry a CouchDB-style `_rev`; a stale one gets 409. Unpublish deletes version names, so a namespace rule such as `immutable` refuses it.
- Not yet: search.

## Maven (`/maven/<repo>`)
```xml
<!-- settings.xml: credentials (Basic, password = token) and Central through o5s -->
<servers><server><id>o5s</id><username>token</username><password>${env.TOKEN}</password></server></servers>
<mirrors><mirror><id>o5s</id><mirrorOf>central</mirrorOf><url>https://o5s.example.com/maven/central</url></mirror></mirrors>
```
```sh
mvn deploy -DaltDeploymentRepository=o5s::https://o5s.example.com/maven/releases
```
- This is the plain repository layout (GET, HEAD, PUT). Checksums (`.md5`, `.sha1`, `.sha256`, `.sha512`) are computed from the stored file, and a checksum PUT must match it.
- Release files are create-once: re-deploying with other bytes gets 409. `maven-metadata.xml` and `-SNAPSHOT` versions are mutable. Authorization is on `maven/<repo>`.
- Proxy-cache: `[maven.upstream.central] url = "https://repo.maven.apache.org/maven2"`. A miss is fetched, checked against the upstream's `.sha1`, and stored. Mutable upstream files are refreshed after `ttl_secs`, and the cached copy is served if the upstream is down.

## Provenance, search, attestations and promotion (all fronts)
- **Automatic provenance:** every publish records `op = "<front>.publish"` (git push per ref, OCI manifest, HF commit, PyPI upload, npm version, Maven file). The stored object is the output, and the front's ids go in `meta` (repo/ref/oid, digest/tag, index/project/version/file, ...).
- **External ids** name objects in every call below:
  - `blake3:<hex>` and `name:<core name>`;
  - `git:<repo>@<oid>` (SHA-1 or SHA-256);
  - `pypi:<index>/<project>/<file>`, `npm:<registry>/<pkg>@<version>`, `maven:<repo>/<path>`;
  - `oci:<repo>@sha256:<hex>`.

  Each requires read access on its front's namespace. Ids that name an object by hash (`blake3:<hex>`,
  and `git:<repo>@<oid>`, which goes through the global oid map) also need the object to be reachable from
  a name the caller may read; otherwise they are refused exactly like an unknown id.
- **`GET /v1/provenance?ref=<id>`** returns producers, consumers, transitive `lineage` and `impact`.
- **`POST /v1/provenance`** `{op, inputs: {role: id}, outputs: {role: id}, meta}` records a client-declared derivation, e.g. a wheel built from a git commit. It needs write access on every output.
- **`GET /v1/search?q=<text>`** (or `prefix=` / `exact=`) searches names, front metadata and provenance. Results are limited to names the caller can read.
- **`POST /v1/attestations`** takes a DSSE envelope (in-toto Statement) and links it to its subjects. **`GET /v1/attestations?ref=<id>`** lists them as `{hash, envelope, trusted_keys}` (`hash` is the stored envelope object), with the trusted keys that verify each one.
- **`POST /v1/promote {src, dst}`** points `dst` at what `src` names, in one OCC transaction, if the `[[promotion]]` rule covering `dst` holds:

  ```toml
  [[promotion]]
  dst = "prod"                       # longest matching prefix applies; uncovered names can't be promoted into
  required_keys = ["<hex ed25519 public key>"]
  predicate_type = "https://slsa.dev/provenance/v1"
  require_provenance = true
  ```

  It needs read on `src` and write on `dst`. It answers 403 with the reasons when the policy refuses, and 409 if either name moved concurrently. Every promotion is recorded in provenance and the audit log.
- **`GET /v1/admin/audit?principal=&action=&target=&outcome=&since=&limit=`** (admin) is one query over the hash-chained audit log of every front, including rotated files.

## Operations
See `deploy/README.md` for install, TLS, reload (SIGHUP), S3 credentials and lifecycle rules, metrics, audit verification (`o5s-server verify-audit`) and capacity notes.
