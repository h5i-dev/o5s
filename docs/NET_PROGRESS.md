# Net session progress (backends & server, branch `net`)

## LFS upload 1.35× → ≤ 1.2× of `/v1`: the Xet chunker off the put path's feeding thread (2026-11-17, branch net-xet-parallel-chunker)

- **Cause:** for `hf/` content (Xet profile, kept for LFS↔Xet chunk dedup) the store's put path ran the serial Xet chunker inline on the thread that also reads the stream (about 1.4 GB/s). That made it the put's serial bottleneck. The first parallel try, 32 MiB windows cut on several threads, was slower (0.49 s) because nothing was handed out while a window filled, and it was reverted.
- **Fix:** `PipelinedXetChunks` in `crates/pack/src/store.rs`. The serial `XetChunker` runs on its own thread, fed 1 MiB blocks through a bounded channel (8 deep). The put thread keeps reading and hands out each chunk as soon as its length comes back. The cuts are the serial chunker's by construction: it sees the same bytes in order, and streaming cuts don't depend on how the input is split.
  - The existing exactness test `put_stream_xet_parallel_windows_cut_exactly_like_serial` passes (jagged reads; sizes around 32 MiB boundaries; a constant run with forced max-size cuts). The unused `LengthChunks` is removed.
- **Dev check** (quiet windows, start load < 1, bench lock; LFS upload server busy against `/v1` PUT of the same 270 MB on one server, median of 4):
  - 0.343 vs 0.312 s, **1.10×** (22:10);
  - 0.320 vs 0.273 s, **1.17×** (23:28).
  - It was 1.35× on rc.1. Server CPU is 0.92 vs 0.70 s (SHA-256 and the Xet chunker still cost CPU, now off the critical path).
  - Not yet combined with the TCP_NODELAY fix (branch net-oci-pull-nodelay); the next batch measures both.

## Final batch for v0.1.0-rc.1: net §6.5 windows A and B (2026-11-17, on release/v0.1.0-rc.1 ba21b99)

- **Runs.** A (git, oci, hf, pypi, npm) ran 10:59–11:17 and B (maven) 11:18–11:44. Release build from a clean worktree at ba21b99.
  - The process log, every 10 s, shows no foreign process above 15 % CPU; peak load 3.9 came from our own processes.
  - Raw data: `bench/results/fronts-rc1-ba21b99.jsonl`. Tiers: `…-report.md`. §6.8 comparison against BASELINE: `…-vs-baseline.md`. Window logs: `bench/results/rc1-logs/`.
- **§6.8 verdict: PASS.**
  - Functional checks: 75/75.
  - Metrics: none worse than its allowance.
  - Tiers: 28 compared, none worse.
  - The back-to-back re-push regression of the superseded 20f55ad pass is fixed (core: reach without a background traversal). jq `push_s` 0.480 vs 0.476 s, redis 8.63 vs 9.04, ripgrep 0.554 vs 0.563.
  - The 20f55ad pass stays on record on branch `net-batch-rc1`.
- **H4 HF, judged per the PLAN amendments (no pre-h5i baseline for these rows):**
  - Xet upload, server busy: 0.37× (STRONG PASS).
  - LFS upload: 1.35×. It misses the 1.2× target and is recorded as a FAIL per the human's ruling (the Xet chunker is kept for LFS↔Xet dedup; the parallel chunker is post-RC).
  - Downloads, on server CPU: LFS 1.01× (PASS), Xet 0.99× (STRONG PASS).
  - Family storage: 0.91× (STRONG PASS).
- **Front verdicts (worst tier):** git, pypi, npm and maven PASS.
  - OCI's FAIL is pull vs Distribution, as in the baseline: not a regression, and core's verified-read work.
  - HF's FAIL is the ruled LFS upload.
  - Maven: cold p50 0.99×, hot p50 1.03×, stored 0.93×.
- `bench/fronts/h3_compare.py` is fixed for the rename (an undefined constant; tier keys now map pico2peta → o5s).

## Status (2026-11-16, after the o5s rename)

- **Project renamed pico2peta → o5s** (demo 7387883, merged into net 61b45f2). The gate is green, and `verify/net` re-extracts unchanged with the official h5i 0.5.5; `h5i app prove` is ok and both obligations are proven. Local paths (`/mnt/scratch/pico2peta`) and the frozen pre-h5i baseline files keep the old name; the report scripts map it.
- **H2 (h5i rebuild), net side: done.**
  - Kernels extracted and proven: access/authz guard, object-read reachability, git push, publish rules, wire parsers, LFS pointer.
  - h5i obligations: panic-free and read-only.
  - Mutants: 18/18 caught.
  - Server on h5i-app-http; issued tokens via h5i-app-token.
  - Released crates.io h5i 0.5.5 only.
- **H3:** run partly. Its findings were fixed: first push (reach closure), clone-pack order, GET (core d00980b). Per the human's re-plan it is folded into one final batch.
- **H4 (Xet/LFS), dev results:** see the H4 section and the PLAN §6.8 HF amendments. Xet up 0.35× and Xet down 1.05× CPU meet the target. LFS up is 1.34×, recorded as FAIL by the human's ruling (Xet chunker kept for LFS↔Xet dedup; parallel chunker post-RC). LFS down is 1.04× CPU.
- **Final batch:** the core's windows on demo 9914209 partly done (suite to rerun after an outside render loaded the host). Net windows A/B cancelled for now; measurements stopped until the human says otherwise.

## Status (2026-11-15)

**Summary (verified 2026-11-15):** all six goals in AGENT_TASK_NET.md are done and verified. The checks: clippy `-D warnings`; all 18 net test binaries, including MinIO; OCI conformance 73/0; real-git round-trips; and the cargo-fuzz targets. CI also has Lean and fuzz-smoke jobs. Production hardening continued beyond the goals (see "Hardening after goal 6"). Remaining limits are listed under "Production gaps". The parts:
- S3 backend: MinIO conformance plus fault injection.
- Server: authn (tokens and OIDC), authz, audit, metrics, TLS, reload, rate limits, GC.
- Client.
- CI: fmt, clippy, test, docs, MinIO, OCI conformance.
- Deploy.
- OCI adapter: conformance 73/0.
- Git adapter: real-git round-trips.

User docs are in `docs/API.md` and `deploy/README.md`.

### Goal 1: S3 backend (`crates/backend-s3`). Done.
- `S3Client` (async) and `S3Backend` (sync `o5s_core::backend::Backend`, with its own tokio runtime).
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
  - 17 MinIO integration tests in `tests/minio.rs`. They are skipped cleanly when `O5S_S3_ENDPOINT` is unset.
  - The core conformance suite runs in single-part, forced-multipart and no-conditional-writes modes.
  - Also covered: multipart from bytes and from a file, a multipart create-only race, pagination over 1100 keys, and a 20 MiB stream.
  - Failure injection goes through a fault proxy (`tests/common/mod.rs`): 5xx/429/SlowDown, TCP RST, lost PUT response, lost CompleteMultipartUpload response, truncated ranged read, truncated stream with resume, persistent 503 (gives up with `transient=true`), 403 not retried, stalled server times out, bad credentials.
- Local MinIO: `scripts/minio-test.sh start|stop|env`.
  - The binary lives at `/mnt/scratch/pico2peta/minio/minio`, built from source tag `RELEASE.2025-09-07T16-13-09Z`, because MinIO no longer publishes community binaries (dl.min.io returns 410).
  - To run the tests: `eval "$(scripts/minio-test.sh env)"; cargo test -p o5s-backend-s3`.

### Goal 2: server (`crates/server`, binary `o5s-server`). Done.
- HTTP API v1 on axum. The table is in `crates/client/src/wire.rs`.
  - `PUT /v1/objects` streams the upload, with an optional `x-o5s-expect-hash` check.
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
  - `o5s-server verify-audit` checks the chain.
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
- The lead plans `o5s --remote URL`, with the token from `--token` or `O5S_TOKEN`.

### Goal 4: CI (`.github/workflows/ci.yml`). Done.
- Jobs: rustfmt check, clippy `--workspace --all-targets -D warnings`, `cargo test --workspace --locked`, rustdoc `-D warnings`, and a MinIO integration job.
- The MinIO job builds MinIO from the pinned source tag with Go (`scripts/build-minio.sh`) and caches the binary, because community binaries and images are gone. It then runs the S3 backend and server tests against it.
- All of these commands pass locally on the current workspace. The workflow has not run on GitHub yet, since nothing has been pushed.

### Goal 5: deploy (`deploy/`). Done.
- A distroless nonroot Dockerfile, sample configs (`server.toml`, `server.docker.toml`, `tokens.example.toml`), a hardened systemd unit (`systemd-analyze verify` clean), and `deploy/README.md` (install, TLS, S3 lifecycle and IAM, observability, audit, capacity).
- Smoke-tested the real binary with the sample config: gen-token, check-config, put, replace, resolve, ranged get, SIGTERM drain and flush, verify-audit.

### Hardening after goal 6
- Native TLS (rustls, concurrent handshakes, ALPN h2) with SIGHUP hot reload of the cert, tokens and policies (`tests/tls.rs`).
- Fuzz targets plus never-panic proptests for every net decoder, and allocation caps on untrusted size headers (zlib-bomb-safe pack scanning). All 7 cargo-fuzz targets ran clean under ASan: git_push 77k runs (180 s), git_parsers 1.1M, oci_parsers 735k, segmap 2.2M, s3_xml 181k, http_range 1.8M, audit_log 13k.
- `ensure_live` (API v1.3) is used wherever existing objects get referenced without being re-put.

### Grouped repack of git content (core `Grouper`, net `GitGrouper`)
- `GitGrouper` follows the EXP1 V2 layout:
  - Groups are `blob:<ext>` (or `blob:<basename>`), `tree`, `commit` and `tag`.
  - Order is path, then the time of the oldest commit that contains the object. Every git repo's refs are walked, oldest commits first.
- **Opt-in** through `[gc] grouped = true`. The default is false because Experiment 1 failed its pre-registered verdict (redis 1.336× git-aggressive, above the 1.30 per-repo maximum), and so did Experiment 1b (patch-from deltas: ripgrep 1.16×, jq 1.09×, redis 1.35×). See docs/EXP1.md. Per the pre-registration, the recommendation for source code stays Git-compatible storage through the git adapter, with grouped repack as an opt-in. and `repack_all`, or per call with `POST /v1/admin/gc?repack_all=true`.
- **Measured on ripgrep** (heads + tags, 14,190 objects), with `cargo test --release -p o5s-server --test git grouped_repack -- --ignored`:

  | | bytes | vs git `--aggressive` |
  |---|---|---|
  | after git push import (one zstd record per object) | 37.06 MB | 9.7× |
  | after grouped `repack_all`: .pack | 3.93 MB | 1.154× git .pack (3.41 MB) |
  | after grouped `repack_all`: .pack + .idx | 4.82 MB | **1.264×** git .pack + .idx (3.81 MB) |

  - The repack takes 52 s on a release build; `git gc --aggressive` takes 7 s.
  - **After the core's frame v2 and idx v3 (demo b3d66e8)**, the same unchanged test gives **1.078× total and 1.021× payload**, with a 34.8 s repack on a loaded host (the lead's run). The rest is mostly index: 44 vs 28 B/object, of which 12 B is BLAKE3 vs SHA-1.
  - Full mirror including refs/pull/* (`O5S_GIT_ALL_REFS=1`, pre-v2 formats): 1.248× total, 1.135× payload.
  - After the repack the repo clones back with identical refs and passes `git fsck --full`.
- This does **not** reproduce EXP1's predicted ~0.85× on ripgrep. Both parts contribute:
  - Payload: 1.15× vs git pack.
  - Index: 62 B/object real vs the 48 B modeled in PLAN §4.4, and git's .idx is about 28 B/object.
- I've reported this to the core lead to reconcile with the Exp1 harness (`bench/exp1`). Possible causes: frame boundaries, grouping keys that differ from the harness, and the cost of the index format.

- S3 default credential chain (env, web identity, IMDSv2) with background refresh.
- Separate metrics listener, and per-principal rate limiting (429 with `Retry-After`, `o5s_rate_limited_total`).
- Audit log rotation with a continuous hash chain across files.
- Token admin API (create, list, revoke) over `auth.token_file`.
- **Multi-host serving:** `[repo] refs = "backend"` creates the repository with the core's backend ref log. Several servers with separate local directories can then share one S3 bucket and prefix. MinIO test (`two_servers_share_a_repo_via_s3`): two servers, four writers contending on one name, 60 commits and 64 OCC conflicts, final version exactly 60 on both servers, and an object written through one server referenced through the other. A config/repo engine mismatch is refused at startup.
- OIDC/JWT authentication (`tests/oidc.rs` against a mock issuer: valid token, wrong audience, expired, bad signature, tampered token, key rotation, unknown kid, static tokens alongside).
- Git protocol v2 for fetch (git's default). `tests/git.rs` now runs on v2 by default and checks v0 and v2 for identical results.
- Git wire deltas: upload-pack deltifies blobs and trees against the previously sent version of the same path. A ripgrep clone (release) transfers 33.8 MB → **6.1 MB**, takes 3.65 s → 2.91 s, refs are identical and fsck is clean.

### Server load benchmark (`crates/server/tests/load.rs`)
Run with `cargo test --release -p o5s-server --test load -- --ignored --nocapture`. Client and server share one 4-core host over loopback. Quiet host (load average 0.9 at start, 2026-11-15); the earlier run under Exp1 load is in parentheses.

| workload | throughput | p50 | p99 |
|---|---|---|---|
| put 4 KiB objects, 16 threads | 9,544 ops/s (4,849) | 1.1 ms | 2.5 ms |
| replace, each thread its own name, 16 threads | 4,792 ops/s (3,793) | 3.2 ms | 8.0 ms |
| contended replace on 1 name, 16 threads, OCC retry loop | 1,202 commits/s (646), 4,872 conflicts | 8.8 ms | 54 ms |
| streaming 4 × 256 MiB up / down (BLAKE3-verified on download) | 181 MiB/s / 1,269 MiB/s (154 / 775) | | |

The contended run checks that no update is lost: the final version equals the number of commits.

### Goal 6a: OCI distribution adapter (`crates/server/src/oci.rs`, `/v2`). Done.
- Covers pull, push (monolithic and chunked with `Content-Range` checks), cross-repo mount, tags/list with pagination, and the OCI 1.1 referrers API (`OCI-Subject`, `artifactType` filter). Manifest and tag deletes go through OCC `delete_ref`; blob deletes return 405.
- Auth: Basic (any user, the token as password) or Bearer. Unauthenticated requests get a `WWW-Authenticate: Basic` challenge.
- Mapping follows the agreed design, with the lead's amendment:
  - The sha256 → BLAKE3 digest index (`oci/_digest`) and per-repo blob membership (`oci/<repo>/_blobs`) are **segment maps** (`segmap.rs`): content-addressed sorted segments behind one ref, OCC updates, tiered compaction, ranged binary search for large segments, and a cache.
  - Tags, manifest links, media types and referrer links stay refs.
  - Map values are weak. `OciTracer` keeps blobs alive through manifests, and is chained inside the core `ProvTracer` for server GC.
  - Uploads spool to `<repo>.oci-uploads/` and hash incrementally (bounded memory). Sessions are per principal, with a TTL and a session cap.
- **Passes the OCI distribution-spec v1.1.1 conformance suite in all four workflows (pull, push, content discovery, content management): 73 passed, 0 failed.** The 6 skips follow blob deletion, which returns 405 as the spec allows; GC reclaims those blobs. Manifest and tag deletion use `delete_ref` (API v1.5). CI job: `scripts/oci-conformance.sh`.
- `/v1` writes under `oci/`, `git/` and `_prov/` need global admin, which protects the digest↔content invariants.
- `POST /v1/replace_many` (atomic, API v1.2), with `RemoteRepo` overriding `replace_many`.

### Goal 6b: Git smart HTTP adapter (`crates/server/src/git.rs`, `/git/<repo>.git`). Done (v1 scope).
- Protocol v0/v1 upload-pack (clone and fetch, with have/ACK/NAK negotiation and side-band-64k) and receive-pack (push, with report-status, side-band-64k and `--atomic` via `replace_many`). v2 clients fall back to v0 automatically.
- The pack reader is written in-crate:
  - Spools the push to disk and verifies the SHA-1 trailer.
  - Resolves OFS and REF deltas, including thin packs against existing objects, with a 256 MiB bounded base cache.
  - Stores canonical `<type> <size>\0<body>` objects and checks connectivity (every referenced oid is in the pack or the repo).
  - Appends one oid-map segment per push.
- Ref updates are CAS: `old` must match the current ref, otherwise `ng … fetch first`. Namespace rules apply, so for example `git/<repo>/refs/tags` can be `ImmutableAfterPublish` (`ng … ref is immutable`). Branch and tag deletion uses `delete_ref` (advertises `delete-refs`), and immutable refs refuse it. `--atomic` pushes with deletes use `update_many` (API v1.6).
- Wants must be tips of this repo's refs, and common haves are restricted to commits reachable from this repo. Because the oid map is global, this keeps repos from reading each other's objects.
- `GitTracer` (header range-read, then commit, tree and tag parsing through an oid-map snapshot) is chained in server GC as `ProvTracer { AdapterTracer { git, oci } }`.
- Verified with the real git 2.50 CLI. Full head and tag sets of the ripgrep and jq mirrors push, clone back with **identical refs**, and pass `git fsck --full` (debug build: ripgrep 8 s push / 17 s clone, jq 9 s / 21 s; server RSS ≤ 170 MB).
- Automated tests (`tests/git.rs`, real git CLI): push, clone, incremental fetch, stale-push rejection, force-with-lease, reader and stranger authz, immutable tags, atomic all-or-nothing, delete refusal, and GC tracer edges, including BLAKE3 = hash of canonical git bytes.

### Next (candidates, in priority order)
- Git `object-format=sha256` repositories.
- Mapping OIDC groups to policies.
- Per-namespace storage quotas.
- (Later) A Hugging Face / Xet adapter. Before designing it, coordinate with the core on adopting Xet's exact chunker (docs/RESEARCH_SPEEDUP.md §4) for chunk-boundary compatibility with HF-hosted models.
- Running the git adapter against git.git/redis-sized repos. Not done yet because of the scratch disk budget, which Exp1 needs.

## Phase 2 (2026-11-15): speed-up backlog shared with the core (AGENT_TASK_NET.md "Phase 2")
- **A. Experiment 1c: DONE, verdict FAIL** (docs/EXP1.md, "Experiment 1c"). V\* = V7, with R 1.41–2.27× (the per-repo limit is 1.30) and build 2.1–3.7× Git. The cause is the pre-registered depth rule forcing long-distance deltas; resemblance picks beat the same-path predecessor when it is eligible. The git.git Git 2.56 baselines were not run (stopped by the human). Harness: `bench/exp1c`, `scripts/exp1c.sh`, `scripts/exp1c-report.py`.
- **B. Xet chunker: DONE.** `crates/chunk`.
  - Byte-exact Xet chunking that **matches all 796 published reference boundaries** (`xet-team/xet-spec-reference-files`), plus FastCDC v2020 identical to the pack store's chunking, behind a streaming `Chunker` trait.
  - The core wired it in as a per-namespace option (API v1.11), and the server uses it for named uploads.
  - Speed (`chunk-bench`, 512 MiB of random data per thread, AMD EPYC 7R32 with 4 vCPUs, three runs under bench.lock; other sessions' builds are not locked, so the range is wide):

    | chunker | 1 thread (best / range) | 4 threads (best / range) | mean chunk |
    |---|---|---|---|
    | Xet (8/64/128 KiB) | 1.41 GB/s (0.96–1.41) | 3.22 GB/s (1.91–3.22) | 64.2 KB |
    | FastCDC v2020 (16/64/256 KiB) | 1.96 GB/s (1.33–1.96) | 4.50 GB/s (2.62–4.50) | 80.0 KB |

  - Xet runs at a stable 0.72–0.80× FastCDC's speed: it is a scalar byte-at-a-time Gear loop, whereas FastCDC processes 2 bytes per step. Both are far above the store's put pipeline throughput (about 0.17 GB/s), so the chunker choice is about dedup compatibility, not speed.
- **Per-namespace chunking in the server (core API v1.11):** `PUT /v1/objects?for_name=<name>` (write access on that name) and OCI blob uploads (namespace `oci/<repo>`) go through `LocalRepo::put_stream_for`, and `RemoteRepo::put_stream_for` exists. Test `put_for_name_uses_namespace_chunker`: the pack store's chunk record counts match `crates/chunk`'s Xet count for `alice/models/` and its FastCDC count elsewhere.
- **C. Byte grouping: DONE.** `crates/weights/src/bytegroup.rs` (`group` / `ungroup`, with proptest round-trip and no-panic tests). Measured on real checkpoints: bf16 SmolLM2-135M → Instruct delta and raw, and fp32 GPT-2 raw. Per-block grouping (what the codec does) gains −8.8% / −12.5% on deltas and −0.6% to −10% on raw weights depending on the zstd level; see "Requests for core".

## Phase 3 (2026-11-15): fronts (VISION.md)
- **HF Hub front: LFS path DONE** (`crates/server/src/hf.rs`, `tests/hf.rs`, `tests/hf/client.py`).
  - Tested with the real, unmodified `huggingface_hub` 2.4.0 and `HF_HUB_DISABLE_XET=1`:
    - create_repo, upload_file (small and LFS), and upload_folder;
    - list_repo_files and model_info(files_metadata);
    - hf_hub_download, snapshot_download, and download of an old revision after delete_file;
    - LFS dedup on re-upload.
  - The HF repo is a real Git repo, `hf/<type>s/<ns>/<name>`:
    - `git clone` of it passes `git fsck --strict`;
    - a `git push` to it is visible via `resolve`, with a matching `X-Repo-Commit`;
    - authz is on `git/hf/...`.
  - GC: `GitTracer` now follows LFS pointer blobs into the `git/_lfs` index. Tested with zero-grace GC: committed LFS content survives and an uncommitted orphan is collected (the negative control).
  - Signed LFS capability URLs (HMAC-SHA256 with a per-process key, one-hour expiry). A forged token gets 403. Because the key is per-process, URLs handed out before a restart stop working, and the client retries via the batch API.
  - CI installs huggingface_hub 2.4.0 and hf_xet 1.8.0 for the test job.
- **HF Xet upload protocol: DONE** (`crates/server/src/xet.rs`). Tested with the real default client (`huggingface_hub` 2.4.0 with `hf_xet` 1.8.0, no env overrides): round-trip, then zero-grace GC, then re-download.
  - Upload flow:
    - `xet-write-token` issues an HMAC capability.
    - Xorbs: chunks are decompressed (none, LZ4 frame, BG4+LZ4), then the chunk hashes and the Merkle xorb hash are recomputed byte-exact with xet-core. The decompressed bytes are stored as one core object, with a `O5SXORB1` record in `git/_xet`.
    - Shards: segments are checked against their verification range hashes, then each file is rebuilt into a core object and checked against its Xet file hash and sha256 (which xet stores word-swapped). It is then registered like an LFS upload.
  - GC follows record → content.
  - Negative tests: tampered xorb gives 400, forged token 401, read token 403.
  - Not done:
    - Xet downloads: no `X-Xet-Hash`, so clients use plain HTTP.
    - Global dedup: `/v1/chunks` gives 404, so clients only dedup against their local cache.
    - Storage: each xorb's decompressed bytes are kept in addition to the rebuilt files. With the Xet chunker on `hf/` (a profile), the pack store dedups them chunk for chunk; without it, they cost about 2×.
    - Xorb records are weak: a client whose local Xet cache references a xorb that GC has collected gets a 400 "upload it first" on the shard.
- **Security fix: LFS ACL.** `git/_lfs` is global (sha256 → content), so anyone who knew a sha256 could commit, and then read, another repo's private file. Now `git/_lfsacl` (sha256(repo namespace, sha)) gates commits, `resolve`, batch "have" answers and download URLs. Test `lfs_objects_are_scoped_to_their_repo`.
- **Port onto the core Front API: DONE** (`crates/server/src/fronts.rs`).
  - Fronts:
    - `git` claims `git/`: Git repos, the HF repos, and the `git/_oidmap`, `git/_lfs*` and `git/_xet` maps.
    - `oci` claims `oci/`.
    - `safetensors` is registered too, so repos configured by the CLI open in the server.
  - Each front's `gc_tracer` takes a snapshot per GC run (`GitTracer`, `OciTracer`). `GitFront::group` is `GitGrouper`, which since API v1.15 (merged) is a hint provider: the core's similarity orderer decides the layout, and Git's kind, path and time keys only break ties (docs/ORDERING.md). New fronts supply no hints.
  - GitState, OciState and HfState write through owned front handles. The fronts hold `Weak` references to them (no Arc cycle through the registry).
  - Server GC is `gc_with_fronts`, with provenance as the extra tracer.
  - Claims are now enforced by the core and not just by the `/v1` check: the plain repository refuses `git/` and `oci/` names. Global admins (`/v1` replace, delete, multi-update) and namespace admins (`set_rule`, e.g. immutable git tags) act through the owning front's handle. A multi-update spanning two fronts is refused.
  - Tests: `tests/fronts.rs`. The HF GC tests now use the real server GC path.
  - `segmap` is now a re-export of `o5s_repo::idmap` (same on-disk format); the server keeps only an `AsyncMap` extension trait for its async handlers.
- **PyPI front: DONE** (`crates/server/src/pypi.rs`, `tests/pypi.rs`).
  - Tested with real twine 6.1, pip 26 and uv 0.13:
    - upload; reader 403; identical re-upload OK; replacement 409;
    - pip install latest, pinned, and with a non-normalized name; anonymous gets a 401 Basic challenge;
    - uv install.
  - Proxy-cache, tested with a second server proxying the first: local uploads and upstream files are merged on one index. The cached file installs after the upstream is stopped and a zero-grace GC is run; a never-fetched file fails (502).
  - Storage: `pypi/<i>/<p>/<file>` → `O5SPYPI1` record → content. The `pypi` front claims `pypi/` and its GC follows record → content.
  - PEP 658/714 core metadata: DONE.
    - On wheel upload, `*.dist-info/METADATA` is read with range reads of the stored wheel (EOCD, central directory, one entry; no full load) and stored create-once as `<file>.metadata`.
    - Pages advertise `core-metadata` and `data-dist-info-metadata` (JSON and HTML).
    - Proxies pass the upstream's metadata through and cache it on first use (sha256-checked, with the upstream token on the upstream host).
    - Tested: sha256 equality, and a proxy pass-through used by pip, including offline.
  - PEP 592 yank/unyank: DONE (`/pypi/<i>/-/yank/<p>/<f>`, a mutable note over the create-once record). Tested with pip: an unpinned install skips the yanked file and an exact pin installs it.
  - Proxy semantics when the upstream fails ("stale-if-error"): pages list only locally cached files, all of which install. I tried serving the stale upstream listing and reverted it: it advertises never-cached versions, and pip and npm pick those (the newest) and fail, when a cached older one would have worked. Maven serves the stale `maven-metadata.xml`, which is right for a mutable index file.
  - Per-tool auth with one o5s token: pip, uv and twine use Basic (password = token, after a 401 Basic challenge); npm uses `_authToken` (Bearer); Maven uses settings.xml `<server>` (Basic); git uses Basic in the URL; docker, oras and crane use Basic. Tokens come from `o5s-server gen-token` / `/v1/admin/tokens`. `npm login` / `npm adduser` are not supported.
  - Maven: no yank or unpublish (the Maven protocol has none); releases stay create-once by design.
  - Not done: the `/pypi/<i>/simple/` root listing of upstream projects (only local projects are listed).
- Fix: early-rejected uploads (LFS capability, PyPI authz) now send `Connection: close`. Before, a client's pooled connection could be reset mid-body, which made one HF test flaky.
- Disk hygiene (2026-11-15, asked by the human): `tests/hf/client.py` leaked a 3–9 MB `tmp*` dir per run into `$TMPDIR`. It now removes it at exit, and the HF and PyPI tests give their subprocesses a TMPDIR inside the test's own directory. A whole-workspace test run now leaves nothing in `$TMPDIR`. Pruned the cargo target dir from 8 GB to under 4 GB.
- **npm front: DONE** (`crates/server/src/npm.rs`, `tests/npm.rs`).
  - Tested with the real npm 10.9 and node 22:
    - publish, plain and scoped (`@team/util`); reader 403; republishing a version 409;
    - install latest, pinned and scoped.
  - Proxy-cache, tested with a second server proxying the first: local and upstream packages are merged. After the upstream stops and a zero-grace GC runs, cached versions still install, `latest` falls back to the newest cached version, and a never-fetched version fails.
  - Storage: `npm/<r>/<seg>/<v>` → `O5SNPMV1` record (manifest + digests) → tarball; dist-tags are `npm/<r>/<seg>/_tags/<t>`. The `npm` front claims `npm/`.
  - Shared plumbing with PyPI lives in `pkg.rs`: a streaming put with sha1/sha256/sha512, create-once, and the HTTP client.
  - Dist-tag API (`npm dist-tag ls|add|rm`): DONE, tested with the real npm (install by tag; readers and unknown versions refused).
  - Deprecate and unpublish: DONE, tested with the real npm.
    - `npm deprecate` and undeprecate store per-version notes (`_deprecated/<v>`, mutable).
    - `npm unpublish pkg@v` (a PUT without the version) and `--force` (DELETE) remove version names through the npm front.
    - Versions follow the namespace's rules: under `immutable`, unpublish is refused. Packuments carry `_rev`; a stale one gets 409.
  - Not done: search.
- **Maven front: DONE** (`crates/server/src/maven.rs`, `tests/maven.rs`).
  - Tested with the real Maven 3.9.9; its plugins come from Maven Central through a o5s proxy:
    - `deploy:deploy-file` of a release; two snapshots (mutable); release overwrite 409; reader 403;
    - `dependency:get` of the release and of the latest snapshot.
  - Proxy chain test: server B proxies server A's Central proxy and A's releases. A fresh client resolves through B; then A is stopped and B runs a zero-grace GC, and a fresh client still resolves the plugins and the artifact entirely from B.
  - Storage: `maven/<repo>/<path>` → `O5SMVN01` record → content. The `maven` front claims `maven/`.
  - The test needs network access to Maven Central and skips without it. The Maven distribution is at `/mnt/scratch/pico2peta/tools/maven` (sha512-verified 3.9.9).
- The 401 for unauthenticated `/pypi/` and `/maven/` requests now carries a Basic challenge, as `/git/` already did: pip and Maven send credentials only after one.
- **All Phase 3 (3) fronts are DONE:** HF (LFS + Xet upload), PyPI, npm, Maven, and OCI (conformance 73/0), each tested with its real client.
- **Phase 3 (4) harness: DONE** (`bench/fronts/`, `scripts/fronts-bench.sh`). It follows PLAN §6.5 with the real clients and local specialists:
  - `git-daemon` 2.50.1 (extracted from the signed AL2023 rpm into tools/bin, since the system git has no daemon);
  - CNCF Distribution 3.0.0 with crane 0.20.6 and oras 1.2.3;
  - devpi-server 6.14.0, verdaccio 6.1.6, reposilite 3.5.19 and Maven 3.9.9.

  Every binary was checksum- or signature-verified, and all of them live under `/mnt/scratch/pico2peta/tools`.
  - **ROADMAP Net 4, final batch (2026-11-16), after the performance work.** Results: `/mnt/scratch/pico2peta/bench-fronts-results-final.jsonl`. Reports: `bench-fronts-report-final-A.md` (git, OCI) and `bench-fronts-report-final.md` (all fronts). §6.5 tiers, medians of 3; host load 1.9–6.2 (other sessions on the host).
    - **Git: PASS** (was FAIL), 90/90 functional checks.
      - clone v2: jq 1.01×, redis 0.74×, ripgrep 0.80×;
      - clone v0: 0.74–0.95×;
      - fetch10: 0.74–0.91×;
      - push: jq 0.87×, redis 0.84×, ripgrep 1.05×;
      - clone bytes: redis 0.58×, jq 1.15×, ripgrep 1.12×.
      - Untiered `first_push_s` (fresh server per run): redis 1.22×, jq 1.32×, ripgrep 1.38×. The small repos pay fixed per-push costs.
    - **OCI: FAIL on pull only**, 24/24 functional checks: push 0.62×, storage 1.00×, pull 1.99× (0.377 s vs 0.189 s).
      - Root cause is core-side. Strict verified reads hash every byte twice (per chunk before delivery, plus the whole object at EOF), and distribution verifies nothing. The core's own §6.4 run measures verified read at 0.184× warm `cat`, also FAIL.
      - The front adds nothing measurable (same `stream_reader` as `/v1`). The core plans multi-core whole-object BLAKE3 per stream after the RC.
    - **HF: FAIL** on transfer times, 6/6 functional checks; storage is STRONG PASS (family 0.91× of Xet dedup).
      - Times vs curl `/v1`: LFS upload 2.08×, LFS download 2.35×, Xet upload 7.66×, Xet download 1.56×.
      - Server-side they are ~1.0× (downloads) and 1.27× (LFS PUT, mostly the Xet-chunker profile); see "HF transfers vs /v1". The end-to-end gap is the HF clients' own hashing, chunking and disk writes, which this protocol counts against o5s.
    - **PyPI: PASS** (cold 1.02×, hot 1.19×, storage 0.47×). **npm: PASS** (1.05×, 1.06×, storage 0.11×). **Maven: PASS** (cold 0.99×, hot 1.01×, storage 0.93×; 3/3 functional checks).
    - **This batch is the pre-h5i baseline** (H5I_PLAN H0). Committed: raw rows `bench/results/fronts-baseline-pre-h5i.jsonl`, the report `bench/results/fronts-baseline-pre-h5i-report.md`, and per-metric run-to-run ranges `bench/results/fronts-baseline-pre-h5i-ranges.md` (for the no-regression rule).
  - Earlier batch (superseded) — **windows 1–2 (2026-11-15), window 3 (Maven) pending.** Report: `/mnt/scratch/pico2peta/bench-fronts-report-w2.md` (§6.5 tiers, medians of 3, with load averages of 2.8–5.0 on 4 cores, which is why the core's builds ran at nice/-j1). Git rows are window 2's rerun; window 1's are kept as superseded. These are the binaries from before the idmap merge-join and the batched git walk.
    - Functional: 132/132 checks pass (git 90, OCI 24, HF 6, PyPI 6, npm 6).
    - PASS or better: PyPI (hot 1.23×, cold 1.06×, storage 0.47×), npm (1.05×, 1.04×, storage 0.11×), git push redis 1.02× / jq 1.16×, git fetch jq 1.09× / ripgrep 0.99×, git clone bytes 0.58–1.10×, OCI push 0.61× and storage 1.00×, HF family storage 0.91× of Xet dedup.
    - FAIL: git clone 1.7–2.8× (redis 22.4 s vs 12.1 s), git push ripgrep 1.98×, git fetch redis 2.75× (0.63 s vs 0.23 s), OCI pull 2.07×, HF transfers 2.2–8.1× the /v1 reference.
    - The failures are throughput- and latency-bound (store read/put path, per-object lookups); the merge-join `get_many` and batched walks below target the git ones. They will be re-measured.
  - Earlier batch status (superseded): paused at the human's request (implementation first; no batch until the human asks).
    - Git front: ripgrep and jq ran completely. **redis: not run (stopped by the human; rerun in the final batch).** Its partial raw rows are in `/mnt/scratch/pico2peta/bench-fronts-results.jsonl` and are not a result.
    - OCI, HF, PyPI, npm, Maven: not run (rerun in the final batch).
    - Two findings from the partial redis rows, recorded as leads, not results:
      - repeat pushes slow down (see "Requests for core");
      - o5s's redis clone took ~370 s, against ~13 s for git daemon. Mostly the same large-segment lookup cost as the re-push (see "Requests for core"); the front's single-threaded encoding is now parallel.
  - Smoke runs (one run each, not the measurement): the package proxies are at about parity, while throughput-bound transfers (Git push and clone, OCI, HF) are 1.7–2.6× the specialist. That matches the store's put/get pipeline throughput and is relayed to the core.
  - Deviations from the §6.5 wording:
    - PyPI timing uses pip (uv is covered functionally in `tests/pypi.rs`).
    - Maven's "per file" is per artifact (`dependency:get -Dtransitive=false` fetches the jar, its pom and parent poms), and its one-fetch check is "hot requests cause no upstream fetch".
- Segmap → idmap swap: DONE (see the Front API port above). The Git front no longer re-puts pushed objects whose oid is already mapped (it checks `ensure_live` first).
- **Fix: connection reuse after early rejections (all routes).** A handler that answers before reading the request body (an auth or authz rejection, a bad digest...) leaves unread bytes, so hyper closes the HTTP/1 connection. A client reusing it saw EOF or a reset. This made OCI conformance flaky (4 of 15 runs failed on the merged tree, with `Put ...: EOF` cascading into later specs), and earlier the HF test.
  - The middleware `close_if_body_unread` (api.rs) tracks whether the body was read to the end and, if not, answers HTTP/1.x with `Connection: close`. The per-handler workarounds are removed.
  - Results: OCI conformance 15/15 runs at 73/0; test `early_rejection_closes_the_connection`.
- **Git front clone path: about 2× faster.**
  - Changes in `write_pack`:
    - it works in windows of up to 256 objects / 64 MiB, and loading, delta encoding and zlib run in parallel across cores;
    - base choice and pack offsets stay sequential, so the pack layout is deterministic;
    - zlib level 3 → 1;
    - the delta index uses an identity hasher instead of SipHash.
  - Development check (not a benchmark result): 3 jq clones took 8.4 s before and 5.0 s after; git daemon takes ~0.84 s per clone.
  - Remaining profile (perf, cpu-clock:u):
    - ~40% is the core read path: zstd decompress ~24%, `ZSTD_createDCtx` per read ~3%, BLAKE3 verify ~8%, zero-filled output buffers ~6%. Reported under "Requests for core".
    - 19% is `encode_delta`.
    - The core's `Index::get` is 0.4%, so lookups are not the cost, as the core lead measured.
  - Not done: a served-pack cache (it would double Git storage, so I'm not doing it without a decision).
- **HF Xet downloads: DONE.** Files uploaded through Xet are now downloaded through Xet by the default client.
  - Protocol:
    - `resolve` sends `X-Xet-Hash` and `X-Xet-Refresh-Route`.
    - `xet-read-token` answers in JSON (for hf_xet) and in `X-Xet-*` headers (for huggingface_hub).
    - `v1/reconstructions/<file>` (with `Range`) returns terms over "virtual xorbs". `v2` answers 404, and the client falls back to v1.
    - Xorb bytes come from signed, credential-free URLs `/xet/xorb/<capability>`, like presigned S3 URLs.
  - Storage:
    - At shard time each file gets a `O5SXFIL1` record: content hash, Xet file hash, sha256 and the file's Xet chunk list. It is indexed in `git/_xetfile` (by file hash) and `git/_xetsha` (by sha256).
    - A virtual xorb is a run of ≤ 8192 chunks / 64 MiB of that list, named by its real Merkle hash. It is served as uncompressed xorb chunks read from the file's content object, so nothing is stored twice and the uploaded xorbs can be collected.
    - GC: an LFS pointer keeps its file record alive, and the record keeps the content.
  - Authorization: the token's repo must be allowed to read the file (the LFS ACL).
  - Tests (`huggingface_hub_round_trip_over_xet`):
    - the real client downloads through `/xet`, which the metrics show;
    - after a zero-grace GC, a hand-run download (resolve → token → reconstruction → ranges) rebuilds the exact sha256;
    - v2 gives 404 and a forged xorb URL gives 403.
  - Files uploaded over LFS still download over plain HTTP; they have no Xet chunk list.
- Root disk rule (the human, 2026-11-15): nothing under /home or /tmp beyond sources. perf's build-id cache is set to `/mnt/scratch/pico2peta/perf-buildid`.
- HF dataset and space repos: tested with the real client (create, upload, list, download under `/datasets/` and `/spaces/`); authorization is on `git/hf/datasets/...` and `git/hf/spaces/...`.
- **HF refs API: DONE.** Branches (create/delete, `startingPoint`), tags (create/delete), `list_repo_refs`, `list_repo_commits` and tag revisions everywhere. All of it is git refs of the repo. Tested with the real client, in LFS and Xet modes.
- ROADMAP Net 2, Git: shallow and partial clone DONE, over protocol v2.
  - Shallow: `deepen N`, `deepen-relative` (`--deepen`), `deepen-since`, and `--unshallow`, with a `shallow-info` section.
  - Partial: `filter blob:none` and `blob:limit=<n>[kmg]`.
  - Non-tip wants (lazy fetches) are allowed only if reachable from this repo's refs, since object ids are global across repos.
  - Real-git test `shallow_and_partial_clone`, including refusing another repo's object.
- **ROADMAP Net 6 (server security review), findings and fixes so far (2026-11-15):**
  - **Fixed: slowloris.** `axum::serve` sets no hyper timer, so its 30 s header-read timeout never applied. A client trickling headers, or an idle keep-alive connection, was held forever (measured: still open after 75 s). The server now accepts through hyper-util's auto builder with a Tokio timer: `HEADER_READ_TIMEOUT` = 30 s for HTTP/1 headers and idle keep-alive, plus HTTP/2 keep-alive pings, and graceful shutdown via hyper-util. Measured: both kinds of connection are closed at 30.0 s. Test `slow_headers_and_idle_connections_are_closed`.
  - **Fixed: SSRF in the proxy-caches** (see above).
  - **Fixed: Xet decompression bomb.** Chunks were capped at 128 KiB each and xorbs at 64k chunks, but an 80 MB body could LZ4-expand to ~8 GB. The total is now capped at 64 MiB (xet-core's `MAX_XORB_BYTES`). Test `xorb_expansion_is_bounded`.
  - **Fixed: unbounded upstream listings.** PyPI pages, npm packuments and PEP 658 metadata are read with size caps (64 MiB listings, 16 MiB metadata).
  - Reviewed, no change needed:
    - capability URLs: HMAC-SHA256, per-process key, expiry, op+repo scope, constant-time compare; repo segments can't contain the `|` separator;
    - path handling: every front maps paths to validated core names; OCI follows the distribution-spec grammar, so no collision with internal `_` names;
    - authz on every write path (dist-tags, yank, unpublish, promote, provenance outputs, attest), and search filtered by readable names;
    - request smuggling: hyper parses, nothing is proxied;
    - token storage: sha256 only, looked up by hash;
    - `/metrics`: unauthenticated on the main listener by design (`metrics_listen` separates it), with route labels as templates, so no repo names leak;
    - `GET /v1/objects/<hash>` lets any reader fetch by BLAKE3 (content address as capability). Hashes of non-readable names aren't exposed by search or provenance.
  - In-memory request bodies (HF commit NDJSON, npm publish JSON) are bounded by min(their cap of 256/512 MiB, `[server] max_upload_bytes`) since 2026-11-16, so an operator can lower them (test `commit_body_is_bounded_by_max_upload_bytes`). Streaming them stays open, pending the human's call on scope.
- ROADMAP Net 7 (ops), part 1: DONE.
  - `deploy/grafana-dashboard.json`: 12 panels over every exported metric.
  - `deploy/prometheus-alerts.yml`: 9 rules, validated YAML.
  - `deploy/k8s/`: `local.yaml` (1-replica StatefulSet + PV) and `s3.yaml` (N replicas on S3 with backend refs, IRSA ServiceAccount, GC CronJob). Both embedded configs pass `o5s-server check-config`.
  - Per-namespace rate limits and storage quotas: DONE (`[[limits]]`, `crates/server/src/limits.rs`; deploy/README "Capacity notes").
    - Rate: one token bucket per rule, shared by all callers, applied after authorization in every front and in the `/v1` name operations.
    - Quota: a background scan every `server.limits_scan_secs` sums the logical bytes reachable from the namespace's names, through the fronts' GC tracers. Writes at or over the quota get 507 `quota_exceeded`. Reads, `/v1` ref deletes, OCI manifest deletes and global admins are exempt.
    - Metric `o5s_namespace_limited_total{outcome}`. Tests: unit `longest_prefix_rate_and_quota`, end-to-end `namespace_rate_and_quota`.
    - Limits: usage is as fresh as the last scan, and shared content is charged to every namespace that references it.
  - S3 deployment guide: DONE (deploy/README "S3 deployment guide": bucket and encryption, VPC endpoint, identity, local vs backend refs and the conditional-create requirement, storage classes vs GC, throughput and throttling, cost model, monitoring, backup/DR and failover with fsck, a pre-launch check).
  - Remaining: a Helm chart (not planned: plain manifests cover it).
- **ROADMAP Net 3 (P5, front side): DONE** (`crates/server/src/prov_api.rs`, `tests/p5.rs`).
  - Every front records its publishes as provenance through the core API, with the front's ids in `meta`, which makes them searchable.
  - External-id resolver: `git:`, `pypi:`, `npm:`, `maven:`, `oci:`, `name:` and `blake3:`, with authz per front namespace.
  - `GET`/`POST /v1/provenance` (lineage and impact across fronts), `GET /v1/search`, `POST`/`GET /v1/attestations` (DSSE), `POST /v1/promote` gated by `[[promotion]]` rules (Ed25519 keys, predicate type, require_provenance), and `GET /v1/admin/audit` (one query across fronts).
  - The test drives a real git push → `git.publish` record → a declared build of a wheel → lineage and impact → search → promotion refused without a signature → signed attestation → promotion allowed → audit query.
  - Not done: OCI cosign/notation signatures as core attestations. Their payloads are a different format (simple signing / JWS), so they stay OCI artifacts.
- ROADMAP Net 2, Git, "bitmaps or equivalent" and "served-pack size": measured, no change needed.
  - Development check on redis heads+tags (not a benchmark result): a clone sends 130.8 MB, against 226.9 MB from git daemon (the earlier batch rows), so ~0.58× git's bytes. The "1.8×" premise doesn't hold for the current code. `fsck` is clean.
  - Profile of that clone (23.7 s vs git daemon ~13 s):
    - object walk (`walk_paths_with`) 0.6%: a reachability cache would buy nothing measurable;
    - the store read path ~45%: zstd ~31%, BLAKE3 verify ~10%, crc ~2% (the core landed per-thread DCtx reuse);
    - the front's delta encoder ~23%.
  - Remaining clone-time levers: a faster `encode_delta` (rolling block hash), and `?verify=eof`-style reads for clone serving where the repo allows it.
- Security fix (ROADMAP item 6, done early): SSRF in the proxy-caches. Upstream listings (and redirects) could point the server at any URL, including the EC2 metadata service. Fetches and every redirect hop are now limited to http(s) on the upstream's host plus `file_hosts` (`pypi.org` implies `files.pythonhosted.org`). Unit test `upstream_fetches_stay_on_allowed_hosts`.
- ROADMAP Net 2, Git: `object-format=sha256` DONE.
  - `Oid` is now 20 or 32 bytes; `Fmt` is per repository, from `[git] sha256_repos` name prefixes.
  - Formats are applied to advertisements, v2 capabilities, push and fetch id parsing, `REF_DELTA` bases, pack checksums and tree parsing (by the tree's own id length).
  - The GC tracer and repack hints parse both formats, since they don't know a stored object's repository.
  - Real-git test `sha256_repositories`: push, clone v2 and v0, `fsck --strict`, pull, shallow, and both format mismatches refused. HF repos stay SHA-1.
- ROADMAP Net 2, OCI: cosign (tag scheme and OCI 1.1 referrers) and notation sign/list/verify work against the OCI front (`tests/oci_sign.rs`, real binaries). Wiring them to core attestations waits for core P5.
- idmap large-segment page cache: DONE (see "Requests for core").
- **read_verify, server side: DONE** (as agreed with the core: one policy source, the repo's `[read]` config, and no server setting).
  - `GET /v1/objects/<h>?verify=strict|eof` calls `open_verified`. `eof` gets 403 unless the repo allows it, and a bad value gets 400. With no parameter, the plain `open` follows `[read] verify`, so strict is the default by construction.
  - `RemoteRepo::open_verified` sends the parameter. Test `read_verify_parameter_follows_repo_policy`.
- **H3 (no-regression rerun), 2026-11-16: waiting for the bench windows.** H3 runs on demo 9f3ee03, which has both sessions' H2. The core's windows run first (suite, §6.4, Exp3), then net's §6.5 batch (windows A and B), compared against `bench/results/fronts-baseline-pre-h5i-ranges.md`. A 1-run harness smoke of hf/oci/npm/pypi on the H2 build passed all 14 functional checks; it is not counted as H3 numbers.
- **H4 (net: Xet/LFS), 2026-11-16, in progress. Dev checks only** (`bench/fronts/hf_split.py`: the client's wall and CPU time, and the server's busy time from its response log, per operation; run 2 is the bench's median case, a re-upload of the same file).

  | op (270 MB model) | client wall | client CPU | server busy | `/v1` | 1.2× target |
  |---|---|---|---|---|---|
  | Xet upload | 1.74 s | 1.87 s | 0.37 s (xorbs 0.35, shard 0.01) | 0.257 s | 0.31 s |
  | Xet download | 0.43 s | 0.40 s | ~0.02 s* | 0.294 s | 0.35 s |
  | LFS upload | 0.54 s | 0.37 s | 0.33 s | 0.257 s | 0.31 s |
  | LFS download | 0.76 s | 0.75 s | ~0.01 s* | 0.294 s | 0.35 s |

  \* until the response starts; the streamed body is not in the access log.
  - **The targets, as defined, are below the client's own floor.** The client (`huggingface_hub` and `hf_xet`) spends that CPU in its own process, whatever the server does:
    - Xet upload: 1.87 s of chunking, hashing and LZ4. On 4 cores that is ≥ 0.47 s, i.e. ≥ 1.8× `/v1` with a zero-time server; measured, the client-only part is ~1.2 s.
    - Downloads are client-bound (wall ≈ client CPU): reconstruction for Xet, Python streaming and hashing for LFS.
    - Only Xet download is near (floor ~1.37×).
    - Reported to the human with these numbers; the server side keeps being reduced toward `/v1` parity.
  - **Human decision (2026-11-16):** judge server-side busy time against `/v1` (≤ 1.2×); report client wall and CPU as informational (PLAN §6.8 HF amendment, drafted).
    - The server now logs `response body done` with the time since the request came in, so streamed bodies count. `hf_split.py` and `hf_bench.py` record `*_server_s` (busy) and `*_server_cpu_s` (server process CPU) per operation, `/v1` included.
    - Server busy, run 2: Xet upload 0.10 s; Xet download 0.30 s; LFS upload 0.38 s; LFS download 0.67 s.
    - **Open question to the human:** a download's busy interval lasts until the client has read the body, so it measures the reader. `huggingface_hub`'s LFS download is CPU-bound at 0.69 s, while curl on the same URL takes 0.258 s (`/v1` 0.251 s, 1.03×). Options: keep the busy interval; judge downloads on server CPU; or replay the requests with a fast reader.
  - **H4 dev result on b485c2c (demo c155ab3 + net; hf_bench, 3 runs; server busy vs `/v1`):**
    - Xet upload 0.35× and Xet download 1.11× meet ≤ 1.2×.
    - LFS upload 1.34× (0.331 vs 0.246 s; serial Xet chunker on the put path; core's parallel-window version was slower, 0.49 s, and is reverted).
    - LFS download 2.28× busy but 1.04× server CPU: its busy interval is `huggingface_hub`'s read speed.
    - Client wall/CPU (informational): Xet up 1.35 s / 1.64 s, Xet down 0.47 / 0.51, LFS up 0.55 / 0.37, LFS down 0.70 / 0.69; `/v1` 0.256 up / 0.294 down.
    - **Human rulings (2026-11-16, PLAN amendment):**
      - LFS upload keeps the Xet chunker and is recorded as a FAIL at its measured ratio (LFS↔Xet chunk dedup is worth more; FastCDC patch not applied).
      - Downloads are judged on server CPU vs `/v1`; busy time is informational.
      - Post-RC: double-buffered parallel Xet chunker in the put path.
    - Fixed meanwhile: `/v1` GET +25 % from d6bfd5f (core's d00980b; A/B back to 0.25 s). Parallel Xet chunking `xet_lengths_parallel`/`xet_cuts_parallel[_seg]` is in `crates/chunk` (exact, proptested) for the after-RC rework.
  - **Open before the final batch (2026-11-16, dev A/B on one host):**
    - **`/v1` GET +25 %:** a733d8e 0.248–0.258 s and 9aba438 0.248–0.253 s, against 0.310–0.321 s after core's d6bfd5f (parallel whole-object BLAKE3 for verified reads). PUT is unchanged. Sent to the core with the binaries and script.
    - **LFS upload, server busy:** 0.372 s against `/v1` PUT 0.242 s (1.54×). Without the `hf/` Xet-chunker profile it is ~0.27 s. The Xet chunker runs at 1.42 GB/s (FastCDC 1.98), on the store's serial put thread with the whole-object BLAKE3; a 4-way unrolled gear loop gave no gain and was reverted. Proposed to the core: speculative parallel Xet chunking in `put_stream` (byte-identical cuts, proptested), or the whole-object hash off the chunker's thread.
  - **Done:**
    - A Xet xorb POST for an already-stored xorb drains the body unbuffered (0.35 → 0.08–0.13 s on a re-upload).
    - The H3-found background-walk interference is gone (core's eb357b9 scheduling): LFS upload 0.80–0.92 → 0.54–0.62 s.
    - A Xet shard for an already-stored file only registers it (shard POST 0.72 → 0.007 s on a re-upload).
- **H4 plan (net: Xet/LFS front), starts only after H3 passes.** Baseline (pre-h5i), against `/v1`'s 0.257 s upload and 0.294 s download of the 270 MB model: Xet upload 1.967 s (7.66×), Xet download 0.457 s (1.56×), LFS upload 0.533 s (2.08×), LFS download 0.689 s (2.35×). The timer is `huggingface_hub.upload_file`/`hf_hub_download`, so it includes the client's own work (hf_xet's CDC chunking, hashing and LZ4 compression on upload), which `/v1` does not do.
  1. First, split the time: per-request server time (xorb POST, shard POST, LFS PUT/GET; a tracing span plus the access log) against the client's wall clock, and `perf record --no-buildid-cache` of the server during an upload. That shows how much of the 7.66× is reachable from the server at all. If the client's floor exceeds 1.2× of `/v1`, report it to the human with the numbers instead of claiming the target.
  2. Server suspects on upload, per xorb: LZ4/BG4 decode of every chunk; the keyed chunk hashes; `put_chunked` hashing and compressing the chunks again; then at shard time `put_concat` rereading the bytes for the whole-object hash, plus SHA-256 for the LFS key. Candidates: verify chunk hashes during decode (one pass); store already-compressed chunks without recompressing where the codec allows; do the shard-time whole-object and SHA-256 hashing incrementally at xorb time or in parallel (with core's multi-core BLAKE3 for the whole-object hash).
  3. Download: compare `xet_xorb` window streaming against `/v1` GET, server-side with curl first (as for the earlier HF work), then with the client.
  4. Every change measured against the baseline, with the proofs kept green.
- **H2 (h5i rebuild), net side: DONE** (origin/net e42e19d, merged into demo 9f3ee03). `crates/net-kernel` is extracted by Charon/Aeneas into `verify/net/proofs/generated/` and proven there (`h5i app check`: no sorry or axioms; 167 theorems gated; both of h5i's obligations, `panic-free` and `read-only`, proven).
  - **Parsers, proven equal to their specs on every input (so total):** `decode_hex` (strict, either-case or lowercase-only), `pkt_len`, `parse_dec_u64`, and the Xet chunk header (`u24`, the checks, the header). The server's hex ids, pkt-lines and xorb headers now run them. `parse_lfs_pointer` (HF's LFS pointers) is proven to compute `ptrS` (`LfsSpec.lean`): ≤ 512 bytes, the version line, and then the last valid `oid sha256:<64 hex>` and the last valid `size <strict u64>` lines (`\r` dropped, unknown lines ignored). `ptr_accepted` adds that an accepted pointer begins with the version line.
  - **Access kernel** (`access.rs` + `authz_kernel!`): every authorization decision runs behind `h5i_app_std::authz::guard_viewed`.
    - `h5i_authz` proves `guarded_transition.authorized`: writes are permitted, disclosed rows are stored and visible, and read-only commands are noninterfering with what the view hides. It also proves `read_only_sound`.
    - The policy itself is specified and proven: `is_under_spec`, `allows_spec`, `reads_some_spec`, `cells_of_spec`, and the two rules: `visible_object_spec` (an object row is visible iff the snapshot lists cells reaching it and the actor may read one) and `permits_spec` (a ref write iff the actor holds write on the name). `Authorizer::allows` calls the kernel's `allows`.
    - h5i's `[authz]` pipeline requires a PostgreSQL Storage theorem, so `Authz.lean` instantiates `h5i_authz` by hand. Storage is o5s's own store (trusted infrastructure).
  - **Object-read authorization (the human's gap): DONE.** `GET`/`HEAD /v1/objects/<h>` only serve an object reachable from a name the caller may read.
    - Mechanism: the shell asks core's `reach::ReachIndex::cells_reaching_many` which of the caller's readable cells reach the hash, then the kernel decides `ReadObject`.
    - Unauthorized and nonexistent get the same 404 body, decided before any store access, so the same timing class.
    - Unreferenced objects are unreadable to everyone. Ten tests that read back unnamed objects now name them first (`keep`), and the HF bench's `/v1` reference names its object, untimed, before the timed GET.
    - Test `object_reads_follow_reachability`: cross-team denied and indistinguishable from missing (body and HEAD); a shared blob readable by both teams; unreadable at once after its last name in a team is deleted; unreferenced objects unreadable, admin included.
  - **Security fix found in this work:** external ids (`/v1/provenance`, `/v1/attestations`, `/v1/search` refs) resolved `blake3:<hex>` with "read somewhere", and `git:<repo>@<oid>` through the *global* oid map, after only a namespace check on `<repo>`. So a reader of one repository could resolve, and read the provenance and attestations of, another team's objects. Every resolved id now also needs the object readable by reachability, and is refused like a missing id otherwise. Tests added in `p5`.
  - **Git push acceptance kernel** (`gitpush.rs`), wired into receive-pack. Proven: `decide_push_spec` (each command's decision, with `--atomic`), `accepted_update`, `accepted_delete`, `atomic_all_or_nothing`.
  - **Publish kernels** (`publish.rs`), wired into their shells. Proven: `decide_create_once_spec` + `release_files_never_change` (PyPI files, npm versions, Maven release files via `pkg::create_once`), `decide_manifest_spec` + `manifest_accepted` (OCI manifest PUT: a digest reference must be the body's, a tag valid, every referenced blob and child manifest present), `decide_commit_spec` + `commit_accepted` (HF commit: a given parent is the head, every LFS file uploaded with its size), `decide_rev_spec` (npm writes naming a `_rev`).
  - **Issued bearer tokens via h5i-app-token.** With `auth.token_key_file` (≥ 32 bytes), `POST /v1/admin/tokens` issues `v1.0.<user>.<expiry>.<hmac-sha256>` tokens (optional `ttl_secs`, default one year).
    - They are parsed by h5i's extracted, proven `h5i_app_token::parse` and checked with libcrux HMAC (constant-time compare), the expiry, and the token table, so revocation still works. The user id must match the listed principal.
    - Static sha256 tokens keep working. Test `issued_tokens_are_h5i_app_tokens` covers tampered signatures and users, and revocation.
  - **Mutants recorded** (`verify/net/h5i-app.toml` `[[mutant]]`, runs in `verify/net/.h5i/app/runs/`): 18/18 caught, 0 survived. Each names its catching theorem:
    - policy: grant level ignored, prefix not segment-aligned, object visible without a readable cell, write needs only read;
    - git push: stale check skipped, branch at a non-commit, atomic not all-or-nothing;
    - parsers: uppercase in lowercase-only hex, decimal leading zero, unchecked Xet stored lengths, LFS oid length unchecked, first LFS size wins;
    - publish: release file overwritten, manifest digest unchecked, missing blob allowed, stale HF parent, missing LFS, any npm rev.
    - `net-kernel` gets h5i-app paths from workspace dependencies, so the mutation sandbox copy builds.
  - **h5i obligations proven** (`Obligations.lean`, `[obligations]`): `panic_free` (the access kernel's `transition` returns on every principal, snapshot and command; with `ref_of_spec`) and `read_only_sound` (`ReadObject`/`ReadRef` write nothing).
  - **Server on h5i-app-http** (`crates/server/src/h5iapp.rs`). The access kernel runs as `H5iApp<App, NetStore, NetExecutor>` (`App = h5i_app_core::Authz<Access>`, whose `transition` is the proven `guard_viewed`), held in `AppState::kernel`.
    - `GET`/`HEAD /v1/objects/<h>` go through `H5iApp::run` with an `Actor`.
    - `NetExecutor` (an `h5i_app_core::Executor`) builds each command's snapshot from core's reach index (the caller's readable cells reaching the object, one batched lookup) and runs the kernel. Batched object checks (`objects_readable`) use the same snapshots and h5i's `Kernel::transition`.
    - Writes stay with core's name-layer kernel, so the executor refuses `WriteRef`/`ReadRef` as unsupported.
    - Deviations, by design:
      - (1) Principals come from the server's own authentication middleware via `Actor::assume_authenticated`, h5i-app-http's documented hatch, because ours is async (OIDC/JWKS) and h5i-app-http's `Authenticator` is synchronous. `NetAuth`, the synchronous authenticator `H5iApp` requires, accepts static and issued bearer tokens.
      - (2) `H5iApp` names a PostgreSQL `Store`. `NetStore` fills that slot (reply/refusal rendering only), and its `Store` methods are never called.
    - The rest of the routes, streaming and fronts are unchanged (axum, as the plan says).
    - Tests: `h5iapp::tests` (executor refusals; `NetAuth` resolves tokens to grants), and all of `server`/`p5` pass through the new path.
    - Static `[[auth.tokens]]` (sha256) stay supported alongside issued tokens. Disabling them would change every deployment's config and the bench setup right before H3, so that is left for the release decision. static tokens behind a legacy flag, with the bench configs moved to issued tokens.
- **Security fix (fuzz finding from the core's smoke run, oci_parsers): strict hex decoding everywhere** (`crates/server/src/hexs.rs`).
  - `oci::hex_key` sliced a 64-*byte* `&str` at byte offsets, so a digest containing a multi-byte character panicked (500, and possibly a poisoned lock). It is reachable from a URL (`/v2/<repo>/blobs/sha256:<64 bytes>`).
  - `u8::from_str_radix` also accepts a leading `+`, so non-canonical digest spellings mapped to the same key.
  - One decoder now accepts only ASCII hex digits, and only lowercase for canonical ids (OCI digests, git oids). Every site goes through it: OCI digests, git oids, pkt-line lengths, LFS sha256 keys, HF percent-decoding, token hashes and promotion keys.
  - Unit test `hexs::tests::strict` covers the multi-byte case, `+`, uppercase and wrong lengths. The remaining byte-offset `&str` slices in the fronts were checked and are at ASCII boundaries.
- **HF transfers vs /v1 (human's priority 2): front overhead removed; the rest is core work or client-side.** SmolLM2-135M (270 MB); dev checks under the bench lock in a quiet window (load 1.5–2.7).
  - Front fixes:
    - LFS uploads compute SHA-256 on their own task, fed the same refcounted buffers, instead of inline in the store's read loop;
    - Xet shard rebuilds also hash SHA-256 on their own thread;
    - xorb chunk hashes are verified on worker threads while the content is stored;
    - virtual-xorb downloads are streamed in 4 MiB windows, instead of materialising ~64 MiB per range, and without a per-chunk allocation.
  - **Server-side** (curl, to `/dev/null`, so no client or disk effects; medians of 6–7):

    | path | o5s | /v1 reference | ratio |
    |---|---|---|---|
    | LFS download (resolve) | 0.277 s | 0.273 s | **1.01×** |
    | Xet download (reconstruction + 5 xorb ranges, 8 parallel) | 0.276 s | 0.278 s | **0.99×** (was 0.340 s, 1.23×) |
    | LFS PUT | 0.327 s | 0.257 s | 1.27× |

    Of the LFS PUT's 1.27×, the hf namespace's Xet chunker profile accounts for 1.19× (`/v1?for_name=hf/...` takes 0.305 s). The front adds ~7%.
  - **End-to-end with the real clients** (medians of 5, same window): LFS upload 0.58 s, LFS download 0.69 s, Xet upload 1.05 s, Xet download 0.51 s, vs curl `/v1` put 0.26 s and get 0.31 s. These ratios include work the HF clients do that curl doesn't:
    - huggingface_hub hashes the file (SHA-256) before an LFS upload;
    - hf_xet chunks and hashes locally (~0.8 s per 270 MB);
    - downloads write through Python into the HF cache, and dirty-page writeback alone moves a plain curl GET from 0.31 s to 0.93 s once other downloads have written to disk.
  - **Wired in core API v1.20** (edd8c8b):
    - xorb uploads use `put_chunked` with the client's chunk boundaries: no server CDC, no concat copy. A 64 MiB xorb POST takes ~0.31–0.47 s, was 0.55–0.72 s.
    - Shard registration uses `put_concat` over the file's xorb ranges, which reuses their chunks, with SHA-256 from `observe` on its own thread. A shard-only (dedup) upload takes 0.58–0.69 s, was 0.94–1.17 s. Its profile has no chunker, memset or CRC left: BLAKE3 32% (source-chunk verification on read, plus the new object's hash) and SHA-256 14%.
    - Suggested to the core: for this caller, an option to skip source-chunk verification in `put_concat`, since the SHA-256 checked against the shard's claim verifies the bytes end to end.
    - Note: `put_chunked` bypasses the namespace's encoding profile for Xet-uploaded content. Chunk boundaries are Xet's anyway (that is the Xet storage model).
  - `stream_reader` (shared by `/v1`, OCI, HF and the package fronts) now runs one blocking task per stream with a reused buffer and 64 KiB pieces. It used to allocate a zeroed 256 KiB buffer per chunk (mmap, page faults) and hop the thread pool per chunk. OCI pull median went from 0.395 to ≈0.36 s in an A/B.
  - Remaining server-side items, both with the core:
    - XetChunker speed (16.5% of a shard-only upload; the core is speeding up the gear loop, byte-exact);
    - `put_concat` (core API v1.20, agreed). Xet shard registration rebuilds the file object: read-back + SHA-256 + re-chunk + hash + dedup put, ~1.1 s for 270 MB even when every chunk exists. With `put_concat` it becomes one BLAKE3+SHA-256 pass reusing the xorbs' chunk records. Net wires it in when it lands.
- **OCI pull (human's priority 3): front overhead is negligible; the bottleneck is the core's verified read path, sent to the core with evidence.**
  - Pull of the 4-image set: 0.35–0.40 s vs 0.18–0.19 s for distribution (~2.05×).
  - Server profile over 8 pull rounds: BLAKE3 36.8%, frame CRC32 11.6%, memset 6.1% (zeroed read buffers in `stream_reader`), kernel ~12%; no zstd (gzip layers are stored raw).
  - The front is the same `stream_reader` path as `/v1` GET. Questions to the core: is the CRC redundant where BLAKE3 verifies the same bytes, and can strict reads avoid hashing twice?
- **Git first push into an empty store (human's request): DONE, target met** (≤1.2×). `index_pack` was rebuilt the way git's index-pack works:
  - pass 1 is one sequential buffered read; it used to seek back and refill a fresh 64 KiB buffer per entry. The trailer checksum is verified on another thread at the same time;
  - pass 1 keeps the inflated data (256 MiB budget: deltas and small whole objects), so pass 2 doesn't inflate twice;
  - pass 2 resolves delta trees on all cores. Each worker takes a whole object (round 1) or a thin pack's base from the repository (round 2), and walks its OFS and REF children depth-first, holding only the chain of bases;
  - storing overlaps resolution: the request thread deduplicates and batches (4096 objects / 16 MiB), and two storer threads each do one `lookup_many` and one core `put_many` (v1.19) per batch.
  - Dev check, redis heads+tags (108,836 objects), first push into a fresh server, under the bench lock; host load 3–5 from other sessions:

    | | wall time |
    |---|---|
    | before (serial parse, one put per object) | 30.5–33.0 s |
    | after | 16.2–17.2 s |
    | git daemon, same window | 14.9–15.0 s |

    That is 1.08–1.15× git daemon, from ~2.7×. Phases now: pass 1 2.2–2.6 s; resolve+store 10–12 s at ~33 CPU-seconds total. The rest is CPU-bound in zstd (the core's level-3 default; level 1 is the human's decision, see the core's BENCH "put/get pass").
- **Git clone/fetch vs git daemon (human's priority 1): DONE, target met** (≤1.2× on redis, cold and warm). It took three changes, each chosen by measurement:
  1. **Clone pack cache.** A clone with no haves, no shallow and no filter produces a pack that is a function of (object format, sorted wants). The front keeps the last one per repository as an object (`O5SCPK01` + key + pack) behind `git/_clonepack/<repo>`, and streams it when the key matches. The ref keeps it alive for GC, and a stale one is simply replaced. It is filled when a clone misses, and refreshed in the background after every push (heads+tags wants), so the first clone after a push is a hit too. Config `[git] clone_pack_cache` (default on). Cost: one pack of storage per cloned repository.
     - Of options (a)–(c), (c) alone gets clones below git daemon. (a)/(b) would cost extra per-object storage for little gain once (c) and the commit graph are in.
  2. **Commit graph** (`crates/server/src/gitgraph.rs`). Each commit's tree, parents and time, and each tag's target, plus the core hash. Kept in memory (bounded), and persisted per repository behind `git/_commitgraph/<repo>`, rewritten in the background after pushes and loaded once per process.
     - Negotiation, the "client has" walk, shallow cuts and the commit waves read no commit objects.
     - `list_refs` no longer reads and SHA-1s every ref's object on every request (556 refs on redis).
  3. **Edge-only tree exclusion, without blob reads** (it used to load every blob of 16 common trees), and the fix to `get_many`'s dense-scan path (see "Small fixes made to core crates"). That scan used to read whole oid-map segments past the page cache on every wave; it was the top of the warm fetch profile.
  - Dev check (redis heads+tags, under the bench lock; cold = fresh process + `drop_caches`). Seconds, o5s vs git daemon:

    | | before | after | git daemon |
    |---|---|---|---|
    | clone, first after a push | 25.3 | 8.8 | 11.1 |
    | clone, cold | 25.3 | 8.7 | 11.1 |
    | clone, warm | 21.0 | 8.5 | 11.2 |
    | fetch10, cold | 1.42 | 0.32–0.35 | 0.29–0.31 |
    | fetch10, warm | 0.94 | 0.20 | 0.21–0.23 |

    Clone is 0.77–0.79×; fetch is 0.93× warm and ~1.13× cold. The clone pack for redis is built in ~15.6 s in the background after the push. Test `clone_pack_cache` covers v0 and v2 clones from the cache, re-encoding after a push, GC, fetch, and fsck --strict on every clone.
- **idmap batching (human's request): DONE.** `get_many` is now a merge-join (see "Small fixes made to core crates").
  - Git push: one `get_many` for every pack object plus every outside object the pack references. Canonical bytes are kept up to 256 MiB; beyond that, new objects are re-resolved. REF_DELTA bases are found by an oid map instead of a linear scan.
  - Clone and fetch: the walk looks objects up in batched BFS waves (commit/tag generations, then trees level by level, then all blobs) and no longer reads blob bodies (they used to be read twice). History is still *emitted* depth-first from the tips, first parent first, like git.
    - A first version emitted breadth-first generations. Across redis's many tips that interleaves versions of each path from different eras, which ruins "previous version at this path" delta bases: clone went 22.5 s → 30.5 s, with `encode_delta` and deflate time up, in an A/B on one repo. Keeping the DFS emit order fixed it.
  - OCI and git GC tracers, the HF LFS batch API, HF commits (LFS checks) and Xet shard registration use `get_many` too.
  - **Dev check** (not a §6.5 result; `bench/fronts/idmap_check.py`, redis heads+tags, under the bench lock, load 2.2–4.6 on 4 cores). Cold = a fresh server process after `drop_caches`; warm = the same operation again. Seconds:

    | | before | after | git daemon |
    |---|---|---|---|
    | re-push, cold | 13.78 | 12.72 | 11.25–12.60 |
    | re-push, warm | 12.51 | 12.02 | 10.90–11.87 |
    | clone v2, cold | 25.25 | 22.51 | 11.08–12.02 |
    | clone v2, warm | 21.03 | 21.78 | 11.05–12.21 |
    | first push into an empty store | 30.50 | 33.03 | 10.93–12.06 |

    A same-repo A/B of four clones per binary gives 22.2–22.8 s before and 21.7–22.2 s after.
    - Re-push is now within ~1.03–1.06× of git daemon. Lookups are no longer the cost: what remains is pack resolution, which git's index-pack does too.
    - Clone is still ~1.8×. That time is compute: the store read path (zstd), delta encoding and deflate. Lookups are gone from the profile; `SegMap::range_many` is ~2%.
  - **Finding (bench design):** the §6.5 `push_s` pushes runs 2–3 into the *same* server, so they are re-pushes and the median hides the first push. Into an empty store, a first push is ~30–33 s vs ~11–12 s for git daemon (~2.7×), dominated by the put path. The final batch should report first-push separately (a fresh server per run). The next push lever is the put path, not lookups.
- Core lead asks (CLI v0.1, `--remote`): DONE.
  - Server GC options now start from the repository's `LocalRepo::gc_options()`, so `[gc] similarity_repack` (default true) decides grouped repacks, with front hints. The server's `[gc] grouped` is now an optional, deprecated override; the deploy configs no longer set it.
  - Client: `RemoteRepo::attest`, `attestations` (with each envelope's hash: the GET response gains `hash`), `promote` (policy refusal → `Ok(Refused)`, missing access or no rule → `Err`), `provenance`, `lineage`, `impact`, and `search` (`&str` text, or `Search::Prefix`/`Exact`). Test `remote_repo_attest_promote_lineage_search`.

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
- **S3 credentials:** the default chain covers env keys, then web identity (EKS IRSA / OIDC via STS `AssumeRoleWithWebIdentity`), then the EC2 instance profile (IMDSv2). Temporary credentials refresh in the background 5 min before expiry. These are tested against mock IMDS and STS; this host has no instance role, so a live AWS run is still missing. Not supported: `~/.aws/config` profiles, SSO, and `credential_process`.
- **Non-conditional S3 stores:** with `conditional_writes = false`, create-only writes are HEAD-then-PUT and not atomic. `S3Backend::atomic_create()` then returns false (API v1.9), the core's backend ref engine refuses it, and the server rejects `repo.refs = "backend"` with that setting. Packs, which are content-named, stay safe.
- **Lost-response detection** relies on the ETag being the MD5 of the body, so it does not work with SSE-KMS or SSE-C. In that case a lost response is reported as `AlreadyExists`. That is still correct for content-addressed keys, but the count is wrong.
- **Abandoned multipart uploads** (process killed mid-upload) are not cleaned up by the client. Deployments should set a bucket lifecycle rule `AbortIncompleteMultipartUpload` (for example, 1 day); deploy/README.md documents it.
- **GET checksums:** a GET is not checksummed end to end beyond TCP and TLS. Integrity of pack records relies on the core pack CRCs.
- **Virtual-hosted style and real AWS:** neither has been tested, because no AWS credentials are available here. Only path style against MinIO has run.
- **Extra copy:** `PutSource::Bytes` is copied once into the runtime, because `Backend` is sync and borrows the bytes.
- **OIDC/JWT:** `[auth.oidc]` validates bearer JWTs (RS/PS/ES algorithms; `iss`, `aud`, `exp` and `nbf` checks; JWKS discovery, caching and rotation), mapping a claim to a principal. Static tokens still work alongside it. Gaps: changes to the OIDC config need a restart (not SIGHUP), groups and roles claims are not mapped to policies, and there is no token introspection for opaque tokens.
- **Token management:** an admin API (`/v1/admin/tokens`) creates, lists and revokes tokens in `auth.token_file`. It rewrites the file atomically, takes effect immediately and is audited. Tokens inline in the main config are read-only through the API. Policies still live in config, with SIGHUP reload.
- **Audit log shipping:** size-based rotation is built in (`audit.rotate_bytes`). The chain continues across files, and `verify-audit` checks all files, so archiving the oldest ones is allowed but a gap is detected. Shipping to a remote sink (syslog, S3) is left to external tooling, for example tailing the JSONL files.
- **Metrics and rate limits:** `/metrics` can move to a separate listener (`server.metrics_listen`), but is still unauthenticated there, so bind it to a private interface. Per-principal token-bucket rate limiting exists (`server.rate_limit`; anonymous callers keyed by IP). There are no byte or storage quotas per principal or namespace.
- **Durability of server puts:** an unreferenced `put` becomes durable at the next replace or flush (core semantics, since the open pack is not fsynced per put). If the server crashes before that, the client's later `replace` gets `MissingObject` and must re-put. Clients needing durability without a replace call `POST /v1/flush`.
- **Git adapter limits:**
  - SHA-1 repos only (no `object-format=sha256`). No shallow or partial clones. Protocol v2 covers fetch only (`ls-refs` with peel, symrefs, unborn and ref-prefix; `fetch` with ready and packfile); pushes use v0/v1, as with git's own server.
  - Served packs use OFS deltas against the previously sent version at the same path (greedy block matching, depth ≤ 50). They are not as tight as git's sliding-window search: a ripgrep clone transfers 6.1 MB vs git's 3.4 MB aggressive pack. Thin packs against the client's haves are not used.
  - Upload-pack has no bitmaps or commit-graph. Incremental fetches exclude what the client has with git's no-bitmap edge strategy (the commit closure of common commits plus the trees of up to 16 boundary commits), so they cost O(commits) rather than O(objects). A clone still walks every object it sends.
  - Each push object is held in memory (cap 1 GiB per object).
  - Ref names outside the core grammar (`[A-Za-z0-9._+-]` segments) are rejected with `ng`.
- **OCI adapter limits:** blob DELETE returns 405 (blobs are reclaimed by GC once no manifest references them), sha256 digests only, and upload sessions do not survive restarts.
- **Large listings:** `list()` buffers all keys in memory. The trait returns a `Vec`, so a paginated or streaming list would need a trait change if a prefix grows to millions of keys.

## Requests for core
- **Read path cost per object (2026-11-15, from a profile of the git front serving jq clones).** Every `get` of a small object pays:
  - a fresh zstd decompression context (`ZSTD_createDCtx`, ~3% of server CPU);
  - zero-filled output buffers (`memset`, ~6%);
  - BLAKE3 verification (~8%);
  - the decompression itself (~24%).

  Ask: reuse a decompression context per thread, and decode into uninitialized or reused buffers. Verification is by design; noting its share only. Front-side work is reported separately (the delta encoder, now parallel).
- **Bug (perf), root cause found: oid-map lookups on large segments** (2026-11-15). This slows git pushes into, and clones from, redis-sized repos.
  - Evidence:
    - the §6.5 Git rows: redis re-push 196/183 s vs 31.6 s first push; redis clone ~372 s vs git daemon 13 s;
    - dev check after the net-side change that skips re-putting known objects: still 30.7 s then 263 s.
  - Profile of the second push (perf, cpu-clock:u): `ZSTD_decompressSequences` 40%, BLAKE3 30%, crc32 14%, `Index::get` 0.2%.
  - Cause: `SegMap::segment_lookup` (`crates/repo/src/idmap.rs:256`, which came from net's segmap) binary-searches segments over `CACHE_WHOLE` (32k entries) with ranged reads, about 18 `get(seg, 72-byte range)` per lookup. Each one decompresses and BLAKE3-verifies the whole stored chunk around the range. The Git front does one lookup per object on push and on clone, so a ~300k-entry map costs ~5M chunk decodes. jq and ripgrep segments are under 32k entries and are cached whole, so they're fast.
  - My earlier hypotheses were wrong: `dedup_hit` → `refresh()` relisting, and `Index::get` (the core lead measured both paths fast).
  - **Fixed by net in `crates/repo/src/idmap.rs`, with the core lead's go-ahead.** Large segments are read through a byte-bounded page cache: 64 KiB aligned pages, 64 MiB shared, FIFO eviction, and entries straddling pages handled. `SegMap::get_many` was added for batches. Test `large_segment_pages_find_every_entry`.
  - Development check, not a benchmark result (redis heads+tags): re-push 263 s → 11.9 s, and clone 372 s → 23.5 s (git daemon ~13 s), with `fsck` clean.
- **Byte-grouping pack codec (Phase 2 C): delivered by the core as record codec 2 (`GroupedZstd`, API v1.12).** Measured with `crates/weights/examples/bytegroup_measure.rs` under the bench lock, on real checkpoints. Two granularities are reported. *Per-block* means each 64 KiB block is grouped on its own, which is what the record codec does. *Tensor-grouped* means each whole tensor is grouped first, which is the possible object-level follow-up. My first report gave only the tensor-grouped numbers under a "64 KiB blocks" label; the core lead caught that.

  | data (zstd output, 64 KiB blocks) | per-block, L3 | per-block, L19 | tensor-grouped, L3 | tensor-grouped, L19 |
  |---|---|---|---|---|
  | bf16 delta: SmolLM2-135M → Instruct (269 MB, 43% zero bytes) | −8.8% | −12.5% | −10.5% | −14.0% |
  | bf16 raw: SmolLM2-135M-Instruct | −1.9% | −10.0% | −9.4% | −14.0% |
  | fp32 raw: GPT-2 (548 MB) | −0.6% | −7.8% | −8.0% | −9.8% |

  - At zstd level 3, per-block grouping only pays off on XOR deltas. At level 19 it recovers most of the tensor-grouped gain on raw weights as well. If the codec runs at a low level, raising the level for grouped records, or object-level grouping, is where the remaining gain lies.
  - Scope of the numbers: they are whole-file (every tensor), but *blocks are tensor-aligned*: each tensor is cut into 64 KiB blocks from its own start. The store's content-defined chunks are not tensor-aligned. End to end, the core measures base −6.0% and delta −10.6% at its default `grouped_zstd_level = 6`. So my per-block column is an upper bound for chunk-level grouping. The core then tested my hypothesis that tensor alignment is the lever (o5s-bench group-align, SmolLM2 base, whole file). At L6: whole-file CDC −6.0%, CDC inside each tensor −5.9%, tensor-aligned fixed 64 KiB blocks −4.9%. At L19: −5.8% / −6.0% / −9.9%. The −10% therefore needs **both** fixed tensor-aligned blocks **and** L19 (about 6 MB/s/core). Tensor-aligned CDC alone gives nothing, so **my hypothesis is refuted** and the core's L6 default stands.
  - The transform runs at 0.9–1.2 GB/s on one core, round trip verified.
  - There is no fp32 fine-tune pair, so the fp32 delta case is unmeasured.
- **RESOLVED (2026-11-15):** a ref-log double-Ok regression (core 8e845fd) was caught by `two_servers_share_a_repo_via_s3`. 60 acknowledged commits had produced only 50–54 versions, because identical CAS records from two writers were both treated as "ours".
  - Fixed in core e7a75c4: each log entry now carries a per-attempt nonce, and only our own nonce counts as ours.
  - Re-verified on MinIO: 5 of 5 runs give 60 commits with final version 60 (75–83 conflicts), and `ref_log_commit_survives_lost_response` still passes.
- 2026-11-15: a streaming put, a backend error variant, and committing early. All three were delivered in API v1.
- `Error::Unauthorized` (v1.1), `replace_many` (v1.2), `ensure_live` (v1.3), the `Grouper` trait and `delete_ref` (v1.5) were delivered, and all are adopted. `DELETE /v1/refs/<name>` exists and `RemoteRepo::delete_ref` overrides the default.
- `put_delta` (v1.7) was delivered and adopted: `PUT /v1/objects?delta_base=<hash>` and `RemoteRepo::put_delta`. A 4 MiB object with sparse edits costs well under 1 MiB over the network API (`tests/server.rs`).
- `update_many` (v1.6) was delivered and adopted: `POST /v1/update_many`, `RemoteRepo::update_many`, and atomic git pushes with deletes.
- Resolved by `ensure_live` (v1.3). The original question was whether `ObjectStore::contains` (or `size`) reports `false` for an object served **only** from a condemned pack? The OCI adapter accepts a manifest when `contains(blob)` is true, without re-putting the blob. If a condemned-only copy counts as present, a manifest pushed between GC's delete-phase mark and the pack deletion could reference a blob that then disappears. If the answer is yes, I need a `touch`/`ensure_live(hash)` primitive (rewrite if condemned-only), or `contains` must ignore condemned copies.
- (none open)

## Small fixes made to core crates
- `crates/repo/src/idmap.rs` (2026-11-15, agreed with the core lead): a page cache for large-segment lookups, plus `SegMap::get_many`.
- `crates/repo/src/idmap.rs` (2026-11-15, asked by the human; core lead told): `SegMap::get_many` internals only. There is no API change and no format change. It is now a sorted merge-join:
  - keys are sorted and deduplicated, then each segment, newest first, is matched against the keys still missing;
  - small segments are merged against the decoded-segment cache;
  - large segments are split recursively around probe entries (through the page cache) until a sub-range holds about one key per 64 KiB page, or fits one 4 MiB window; that range is then read sequentially in 4 MiB-aligned ranged reads (bypassing the page cache) and merged;
  - results come back in input order.
  - Follow-up (same file, lead told): the dense-scan path reads through the page cache. Runs of missing 64 KiB pages are fetched with one ranged read of ≤4 MiB each. `PAGE_CACHE_BYTES` went from 64 to 128 MiB.
  - Proptest `get_many_matches_per_key_lookups` (1–4 segments of up to 3 × `CACHE_WHOLE` entries, overrides by newer segments, misses, duplicates, dense and sparse batches) checks it against per-key `get_in`, plus a deterministic edge test. `proptest` became a dev-dependency of `o5s-repo`.
- `crates/cli/src/main.rs`: wrapped a bare URL in a doc comment in `<…>` so that `cargo doc -D warnings` (CI docs job) passes.

## Questions for human
- (none yet)
