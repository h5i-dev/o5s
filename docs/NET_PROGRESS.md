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

### Next
- Goal 2: server (`crates/server`).
- Goal 3: client (`crates/client`).
- Goal 4: CI (`.github/workflows/`).
- Goal 5: deploy (`deploy/`).

## Production gaps
- **S3 credentials:** only static and environment credentials exist. There is no IMDSv2 instance profile, STS web identity or `~/.aws` profile support yet. These can plug into the `CredentialsProvider` trait.
- **Lost-response detection** relies on the ETag being the MD5 of the body, so it does not work with SSE-KMS or SSE-C. In that case a lost response is reported as `AlreadyExists`. That is still correct for content-addressed keys, but the count is wrong.
- **Abandoned multipart uploads** (process killed mid-upload) are not cleaned up by the client. Deployments should set a bucket lifecycle rule `AbortIncompleteMultipartUpload` (for example, 1 day). This is documented in deploy/ (TODO).
- **GET checksums:** a GET is not checksummed end to end beyond TCP and TLS. Integrity of pack records relies on the core pack CRCs.
- **Virtual-hosted style and real AWS:** neither has been tested, because no AWS credentials are available here. Only path style against MinIO has run.
- **Extra copy:** `PutSource::Bytes` is copied once into the runtime, because `Backend` is sync and borrows the bytes.
- **Large listings:** `list()` buffers all keys in memory. The trait returns a `Vec`, so a paginated or streaming list would need a trait change if a prefix grows to millions of keys.

## Requests for core
- 2026-11-15: a streaming put, a backend error variant, and committing early. All three were delivered in API v1.
- (none open)

## Small fixes made to core crates
- (none)

## Questions for human
- (none yet)
