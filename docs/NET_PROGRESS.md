# Net session progress (backends & server, branch `net`)

## Status (2026-11-15)

### Goal 1: S3 backend (`crates/backend-s3`). Done.
- `S3Client` (async) and `S3Backend` (sync `pico2peta_core::backend::Backend`, with its own tokio runtime).
- SigV4 signing is implemented in-crate. It is checked against the AWS documentation test vectors (GET with Range, PUT, ListObjects). Payloads are signed with their SHA-256, so the server verifies body integrity.
- Create-only writes use `PUT` with `If-None-Match: *`, and `412` maps to `AlreadyExists`. If a response is lost and the retry gets `412`, the client compares the stored ETag with the body's MD5 and reports `Created` when the stored object is ours.
- Multipart upload kicks in at `multipart_threshold` (64 MiB by default), from memory or straight from a file (`PutSource::File`).
  - Parts default to 16 MiB and grow automatically to stay under 10,000 parts. 4 parts are in flight at once, so memory is bounded at about concurrency × part size.
  - `CompleteMultipartUpload` is sent with `If-None-Match: *`. A 200 response carrying an `<Error>` body is handled. `NoSuchUpload` or `412` after a retry is resolved by comparing against the expected multipart ETag `md5(md5s)-N`.
  - On failure or `AlreadyExists` the upload is aborted. Before starting, a HEAD skips the upload if the key already exists.
- Reads:
  - Ranged GET verifies the length. Reading past the end gives `Corrupt`, and a truncated body is retried.
  - `open()` streams through a bounded channel of 4 chunks. A mid-body failure resumes with `Range: bytes=pos-` pinned by `If-Match: <etag>`.
- Listing uses ListObjectsV2 with pagination and `encoding-type=url`, strips the configured prefix, and returns keys sorted.
- Retries use capped exponential backoff with full jitter, a separate (larger) base delay for throttling (`SlowDown`/429/503), and a shared retry budget (token bucket, same scheme as the AWS SDK's "standard" mode) to prevent retry storms. Errors are classified as transient (transport, timeout, 408/429/5xx, `RequestTimeout`, `ConditionalRequestConflict`, …) or permanent (403, 404, other 4xx).
- Timeouts: connect, header/read idle, and a per-request total scaled by body size (`request_timeout + bytes / min_throughput`).
- Concurrency is bounded by a semaphore (`max_in_flight`, default 32), which also bounds the open connections. The idle pool size is configurable.
- Configurable endpoint, region, bucket, key prefix, and path-style or virtual-hosted addressing. That covers AWS, MinIO, Ceph RGW and R2. `conditional_writes=false` falls back to HEAD-then-PUT for stores without `If-None-Match`.
- Counters (`S3Stats`: requests, retries, throttled, failures, bytes) are ready for Prometheus export by the server.
- Tests:
  - 19 unit tests: SigV4 vectors, XML parsing including malformed input, backoff and budget, config, and the multipart ETag.
  - 17 MinIO integration tests in `tests/minio.rs`. They are skipped cleanly when `P2P_S3_ENDPOINT` is unset.
  - The core conformance suite runs in single-part, forced-multipart and no-conditional-writes modes.
  - Also covered: multipart from bytes and from a file, a multipart create-only race, pagination over 1100 keys, and a 20 MiB stream.
  - Failure injection goes through a fault proxy (`tests/common/mod.rs`): 5xx/429/SlowDown, TCP RST, lost PUT response, lost CompleteMultipartUpload response, truncated ranged read, truncated stream with resume, persistent 503 (gives up with `transient=true`), 403 not retried, stalled server times out, bad credentials.
- Local MinIO: `scripts/minio-test.sh start|stop|env`.
  - The binary lives at `/mnt/scratch/pico2peta/minio/minio`, built from source tag `RELEASE.2025-09-07T16-13-09Z`, because MinIO no longer publishes community binaries (dl.min.io returns 410).
  - To run the tests: `eval "$(scripts/minio-test.sh env)"; cargo test -p pico2peta-backend-s3`.

### Goal 2: server (`crates/server`, binary `pico2peta-server`). Done.
- HTTP API v1 on axum. The table is in `crates/client/src/wire.rs`.
  - `PUT /v1/objects` streams the upload, with an optional `x-p2p-expect-hash` check.
  - `GET`/`HEAD /v1/objects/{hash}` support single `Range` (`a-b`, `a-`, `-n`) and return 206 or 416 with `Content-Range: */n`, plus an ETag and an immutable cache header.
  - Also: `GET /v1/refs/{name}`, `GET /v1/refs?prefix=`, `POST /v1/replace` (200 for Ok, 409 for Conflict), `GET`/`PUT /v1/namespaces[/{prefix}]`, `POST /v1/flush` and `GET /v1/whoami`.
  - Errors are JSON `{error:{code,message,rule?,subject?}}`. `code` is the core `Error::code()` or an HTTP-only code.
- **Authn:** an `Authenticator` trait (async, pluggable for OIDC). `TokenAuthenticator` keeps static bearer tokens as SHA-256 only, read inline or from `auth.token_file`. Anonymous access is opt-in, and a bad token is never downgraded to anonymous.
- **Authz:** per-namespace grants with `read < write < admin` on segment-aligned prefixes. A principal is a name, `*` (any authenticated) or `anonymous`.
  - Each operation needs read on the name, or write on the replace target plus read on every read-set name (because conflicts reveal versions).
  - `set_rule` needs admin. Ref listings are filtered.
  - Objects are global: reading one needs read somewhere, storing one needs write somewhere.
- **Audit:** append-only JSON lines with a BLAKE3 hash chain (`seq`, `prev`).
  - A writer thread group-commits with fdatasync. Mutations wait until their record is durable before replying.
  - Denials and (optionally) reads are logged too. Records carry the request id and client address (TCP peer, or `X-Forwarded-For` with `trust_forwarded_for`).
  - A torn tail is truncated at open. After a write failure the server refuses all mutations and `/readyz` returns 503.
  - `pico2peta-server verify-audit` checks the chain.
- **Ops:**
  - Prometheus `/metrics`: HTTP count, latency histogram, in-flight, bytes, replace outcomes, auth failures, denials, audit failures, and S3 client counters.
  - `/healthz` and `/readyz`.
  - `tracing` logs in JSON or text: one INFO access line per request plus a request-id span, and the `Authorization` header is marked sensitive.
  - Graceful shutdown on SIGTERM/SIGINT: readiness turns to 503, new requests get 503, in-flight requests get `shutdown_grace_secs`, then flush and audit sync.
  - TOML config with `deny_unknown_fields`, validated at startup, plus a `check-config` subcommand.
- **Bounded memory and backpressure:**
  - Uploads go through a bounded channel of 8 frames into `put_stream` on the blocking pool. Downloads go in 256 KiB chunks through a bounded channel of 4, read by short `spawn_blocking` calls, so no blocking thread waits on a slow client. A client idle for 60 s is cut off.
  - `max_upload_bytes` is enforced both from Content-Length and mid-stream.
  - Load shedding: beyond `max_concurrent_requests` the server returns 503 with `Retry-After`. Permits are held until the response body finishes.
  - Measured: 19 MiB peak RSS growth for a 512 MiB upload plus download in one process (`tests/memory.rs`, bound set at 96 MiB).
- Tests: 9 unit tests (ranges, auth, policy, audit chain, torn tail, tamper detection, config) and 14 end-to-end tests (`tests/server.rs`, `tests/memory.rs`).
  - Covered: client roundtrip and typed errors, 24 MiB streaming, rules and conflicts, authn, anonymous access, namespace authz isolation, audit content, HTTP ranges and headers, upload limits and hash mismatch, metrics, overload 503, graceful shutdown (drain, then flush, then durability on reopen), bounded memory, and the full server on the S3/MinIO backend.

### Goal 3: client (`crates/client`). Library done; CLI wiring is with the lead.
- `RemoteRepo::connect(url, token)` (checks via whoami) and `RemoteRepo::new(url, token, ClientConfig)`. It implements `Repository + ObjectStore`.
- End-to-end BLAKE3 verification:
  - `put` sends the expected hash.
  - `put_stream` hashes while streaming and compares with the server's answer.
  - Whole-object `get`/`open` re-hash at EOF and fail with `Error::Corrupt`.
- Idempotent calls retry with jitter on transport errors and 502/503/504. `replace` retries only on connect failures, and `put_stream` never retries (its source is one-shot).
- Typed core errors are rebuilt from wire codes, including `Forbidden{rule}`, `MissingObject`, `InvalidRange{len}` and `TargetNotInReadSet`.
- The lead plans `pico2peta --remote URL`, with the token from `--token` or `P2P_TOKEN`.

### Goal 4: CI (`.github/workflows/ci.yml`). Done.
- Jobs: rustfmt check, clippy `--workspace --all-targets -D warnings`, `cargo test --workspace --locked`, rustdoc `-D warnings`, and a MinIO integration job.
- The MinIO job builds MinIO from the pinned source tag with Go (`scripts/build-minio.sh`) and caches the binary, because community binaries and images are gone. It then runs the S3 backend and server tests against it.
- All of these commands pass locally on the current workspace. The workflow has not run on GitHub yet, since nothing has been pushed.

### Goal 5: deploy (`deploy/`). Done.
- A distroless nonroot Dockerfile, sample configs (`server.toml`, `server.docker.toml`, `tokens.example.toml`), a hardened systemd unit (`systemd-analyze verify` clean), and `deploy/README.md` (install, TLS, S3 lifecycle and IAM, observability, audit, capacity).
- Smoke-tested the real binary with the sample config: gen-token, check-config, put, replace, resolve, ranged get, SIGTERM drain and flush, verify-audit.

### Next
- Goal 6: Git smart-HTTP and OCI distribution adapters. The hash-mapping design proposal is below, waiting for the lead's agreement.

### Goal 6a: OCI distribution adapter (`crates/server/src/oci.rs`, `/v2`). Done.
- Covers pull, push (monolithic and chunked with `Content-Range` checks), cross-repo mount, tags/list with pagination, and the OCI 1.1 referrers API (`OCI-Subject`, `artifactType` filter). Deletion returns 405.
- Auth: Basic (any user, the token as password) or Bearer. Unauthenticated requests get a `WWW-Authenticate: Basic` challenge.
- Mapping follows the agreed design, with the lead's amendment:
  - The sha256 → BLAKE3 digest index (`oci/_digest`) and per-repo blob membership (`oci/<repo>/_blobs`) are **segment maps** (`segmap.rs`): content-addressed sorted segments behind one ref, OCC updates, tiered compaction, ranged binary search for large segments, and a cache.
  - Tags, manifest links, media types and referrer links stay refs.
  - Map values are weak. `OciTracer` keeps blobs alive through manifests, and is chained inside the core `ProvTracer` for server GC.
  - Uploads spool to `<repo>.oci-uploads/` and hash incrementally (bounded memory). Sessions are per principal, with a TTL and a session cap.
- **Passes the OCI distribution-spec v1.1.1 conformance suite: 64/64** across pull, push and content discovery (content-management is skipped because deletes are unsupported). It ran three times on fresh repositories. CI job: `scripts/oci-conformance.sh`.
- `/v1` writes under `oci/`, `git/` and `_prov/` need global admin, which protects the digest↔content invariants.
- `POST /v1/replace_many` (atomic, API v1.2), with `RemoteRepo` overriding `replace_many`.

### Goal 6b: Git smart HTTP adapter (`crates/server/src/git.rs`, `/git/<repo>.git`). Done (v1 scope).
- Protocol v0/v1 upload-pack (clone and fetch, with have/ACK/NAK negotiation and side-band-64k) and receive-pack (push, with report-status, side-band-64k and `--atomic` via `replace_many`). v2 clients fall back to v0 automatically.
- The pack reader is written in-crate:
  - Spools the push to disk and verifies the SHA-1 trailer.
  - Resolves OFS and REF deltas, including thin packs against existing objects, with a 256 MiB bounded base cache.
  - Stores canonical `<type> <size>\0<body>` objects and checks connectivity (every referenced oid is in the pack or the repo).
  - Appends one oid-map segment per push.
- Ref updates are CAS: `old` must match the current ref, otherwise `ng … fetch first`. Namespace rules apply, so for example `git/<repo>/refs/tags` can be `ImmutableAfterPublish` (`ng … ref is immutable`). Deletes return `ng`.
- Wants must be tips of this repo's refs, and common haves are restricted to commits reachable from this repo. Because the oid map is global, this keeps repos from reading each other's objects.
- `GitTracer` (header range-read, then commit, tree and tag parsing through an oid-map snapshot) is chained in server GC as `ProvTracer { AdapterTracer { git, oci } }`.
- Verified with the real git 2.50 CLI. Full head and tag sets of the ripgrep and jq mirrors push, clone back with **identical refs**, and pass `git fsck --full` (debug build: ripgrep 8 s push / 17 s clone, jq 9 s / 21 s; server RSS ≤ 170 MB).
- Automated tests (`tests/git.rs`, real git CLI): push, clone, incremental fetch, stale-push rejection, force-with-lease, reader and stranger authz, immutable tags, atomic all-or-nothing, delete refusal, and GC tracer edges, including BLAKE3 = hash of canonical git bytes.

## Design: Git and OCI hash mapping (goal 6; AGREED with core lead 2026-11-15; amendment: per-blob mappings in segment maps, not refs)

**Principles**
1. Internal identity stays BLAKE3-256 of the exact stored bytes. Adapters never invent a second identity scheme inside the core.
2. Each external id maps to the BLAKE3 of the byte-exact payload, so serving the external protocol is a lookup plus a verbatim read. Nothing is regenerated on the read path.
3. The mapping is verified on ingest. The external hash is computed while streaming, and a mismatch is rejected before anything becomes visible under a name.
4. Every external→internal mapping is a pure function of the bytes. Mapping tables can therefore be global and merged by union, without conflicts, and all of them are rebuildable from the objects.

**OCI distribution (`/v2/...`)**
- Payload: the exact blob or manifest bytes (compressed layers stay compressed). Pulls are byte-identical, so `sha256:` digests hold by construction.
- Mapping: refs, using core names and rules.
  - `oci/_digest/sha256/<hex>` → BLAKE3 is a global digest index under `ImmutableAfterPublish`, so a digest can never be repointed.
  - `oci/<repo>/_blobs/sha256/<hex>` → BLAKE3 is the per-repo link. Registry semantics say a blob is visible in a repo only if it is linked there. Links are create-once.
  - `oci/<repo>/_manifests/sha256/<hex>` → BLAKE3 holds manifests by digest.
  - `oci/<repo>/tags/<tag>` → BLAKE3 of the manifest. The tag rule is per-namespace configuration: `Mutable` for dev, `ImmutableAfterPublish` for releases. This is the brief's "per-kind config".
- GC: links and tags are refs, so `OpaqueObjects` already keeps everything referenced alive. Deleting a tag or manifest doesn't free layers until their links are removed, the same as Docker distribution.
- Uploads: chunked `PATCH` sessions spool to a local file under `<repo>/staging/oci-uploads/<uuid>` (bounded memory, resumable). The final `PUT ?digest=` hashes the spooled file, calls `put_stream`, then creates the refs.
- Authz: the namespace is `oci/<repo>`, with pull = read and push = write, through the same Policy engine.
- Deferred: dedup inside compressed layers (store the uncompressed tar plus recompression parameters, à la DupHunter or zstd:chunked). That needs byte-exact recompression guarantees and should be an experiment first.

**Git smart HTTP (`/git/<repo>.git/info/refs`, `git-upload-pack`, `git-receive-pack`)**
- Payload: the canonical loose-object bytes `<type> <size>\0<body>`, the same payload Experiment 1 measures. EXP1 compression results therefore transfer directly to the git adapter. The BLAKE3 of this is the internal id. SHA-1 and SHA-256 repositories use the same scheme; the oid length comes from the repo's object format.
- Mapping (oid → BLAKE3, type, size): a single global oid map, valid because the mapping is a pure function.
  - It is stored as content-addressed sorted segments: 20/32-byte oid, 32-byte BLAKE3, 1-byte type and varint size, about 60 B per object.
  - A segment-list manifest object, pointed to by the ref `git/_oidmap`, ties them together. Pushes add a segment, and a compactor merges segments LSM-style.
  - Updating the map is an OCC `replace` with `read_set = [(git/_oidmap, v)]`. A conflict retries the merge, which always succeeds because it is a union.
- Refs: `git/<repo>/refs/heads/<b>` → BLAKE3 of the commit. A receive-pack command `old new ref` *is* a compare-and-swap: resolve, check that the current BLAKE3 maps to `old`, then `replace(name, [(name, Version(v))], new)`. A version conflict becomes `ng <ref> fetch first`. `refs/tags/*` can be made immutable by namespace rule.
- Push: index the received pack (resolve deltas) into canonical objects, put each one, append an oid-map segment, then move refs. Objects become durable before refs move, because core `replace` flushes first.
- Fetch: upload-pack negotiates against the oid map, walks commits and trees from wants to haves, and streams a pack. v1 sends full objects with no deltas. Delta generation is a later optimization, gated on measurement as the brief prescribes.
- Implementation: use gitoxide crates (`gix-pack`, `gix-object`, `gix-packetline`) for pack parsing and encoding instead of hand-rolling them.
- GC: git objects are not individually named, so GC needs a **`GitTracer`** (commit → tree and parents, tree → entries, tag → target). It translates oids through the oid map, and also keeps the oid-map segments alive (`git/_oidmap` manifest → segments).

**Requests and questions for core**
1. Agree on the payload formats above: canonical git loose-object bytes, and verbatim OCI bytes.
2. Tracer composition. GC takes one `&dyn Tracer`, and the server would pass a composite (Git + oid-map manifests + opaque). A tracer must be conservative, never treating a non-git blob as git in a way that drops liveness. That's fine, because over-approximation only keeps extra objects. Is `Tracer::children(hash, store)` enough, or should core add a cheap type hint so tracers don't have to read every live object?
3. Multi-target `replace`, so that `git push --atomic` and OCI manifest+tag can move several names in one commit. Until then, refs move one at a time. Git's non-atomic push semantics already allow that.
4. Ref store scale. The OCI digest index and links create one ref per blob per repo, possibly millions. Is the journaled RefStore fine at 10^6–10^7 names? If not, the digest index could move into content-addressed segments like the git oid map.

Plan: OCI first (smaller and well specified). It will be validated with the official OCI distribution-spec conformance suite (Go), built locally. Git smart HTTP comes second.

## Production gaps
- **S3 credentials:** only static and environment credentials exist. There is no IMDSv2 instance profile, STS web identity or `~/.aws` profile support yet. These can plug into the `CredentialsProvider` trait.
- **Lost-response detection** relies on the ETag being the MD5 of the body, so it does not work with SSE-KMS or SSE-C. In that case a lost response is reported as `AlreadyExists`. That is still correct for content-addressed keys, but the count is wrong.
- **Abandoned multipart uploads** (process killed mid-upload) are not cleaned up by the client. Deployments should set a bucket lifecycle rule `AbortIncompleteMultipartUpload` (for example, 1 day). This is documented in deploy/ (TODO).
- **GET checksums:** a GET is not checksummed end to end beyond TCP and TLS. Integrity of pack records relies on the core pack CRCs.
- **Virtual-hosted style and real AWS:** neither has been tested, because no AWS credentials are available here. Only path style against MinIO has run.
- **Extra copy:** `PutSource::Bytes` is copied once into the runtime, because `Backend` is sync and borrows the bytes.
- **Server TLS:** none built in; terminate TLS at a reverse proxy (documented). Native rustls listener is a possible addition.
- **OIDC/JWT authenticator:** not implemented; the `Authenticator` trait is async and ready for it.
- **Token management:** only via config/token file plus restart. There is no admin API to issue or revoke tokens, and no hot reload (SIGHUP).
- **Audit log rotation and shipping:** manual (stop, move, start). There is no remote sink (syslog, S3).
- **Unauthenticated `/metrics`:** restrict it at the network level. There is no separate metrics listener yet.
- **Rate limiting:** only a global concurrency cap. There is no per-principal rate limit or quota.
- **Durability of server puts:** an unreferenced `put` becomes durable at the next replace or flush (core semantics, since the open pack is not fsynced per put). If the server crashes before that, the client's later `replace` gets `MissingObject` and must re-put. Clients needing durability without a replace call `POST /v1/flush`.
- **Git adapter limits:**
  - SHA-1 repos only (no `object-format=sha256`). No protocol v2 server side (clients fall back to v0), no shallow or partial clones, and no ref deletion.
  - Served packs carry full objects, with no delta compression on the wire, so clones transfer more bytes than from git.
  - Upload-pack walks the full reachable set per request (no bitmaps or commit-graph), which is O(history) per fetch.
  - Each push object is held in memory (cap 1 GiB per object).
  - Ref names outside the core grammar (`[A-Za-z0-9._+-]` segments) are rejected with `ng`.
- **OCI adapter limits:** no deletes (so registry GC can only reclaim blobs that were never referenced by a manifest), sha256 digests only, and upload sessions do not survive restarts.
- **Large listings:** `list()` buffers all keys in memory. The trait returns a `Vec`, so a paginated or streaming list would need a trait change if a prefix grows to millions of keys.

## Requests for core
- 2026-11-15: a streaming put, a backend error variant, and committing early. All three were delivered in API v1.
- `Error::Unauthorized` (v1.1) and `replace_many` (v1.2) were delivered, and both are adopted.
- Open question: does `ObjectStore::contains` (or `size`) report `false` for an object served **only** from a condemned pack? The OCI adapter accepts a manifest when `contains(blob)` is true, without re-putting the blob. If a condemned-only copy counts as present, a manifest pushed between GC's delete-phase mark and the pack deletion could reference a blob that then disappears. If the answer is yes, I need a `touch`/`ensure_live(hash)` primitive (rewrite if condemned-only), or `contains` must ignore condemned copies.
- (none open)

## Small fixes made to core crates
- (none)

## Questions for human
- (none yet)
