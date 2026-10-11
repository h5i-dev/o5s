# Authenticated agent load tests

## Rust driver

`o5s-load` (`crates/loadgen`) drives a running server over the authenticated API with persistent connections and checks the final bytes. Its `git` and `compare` modes measure the plain-Git baseline on the same workloads; see the [comparison with Git](git-comparison.md). The Python driver below is limited by its own threads and per-request connections. With 64 requests in flight it reported about 1.3 s per command, while the server's `/metrics` showed 2.3 ms per command including lock waits.

```sh
cargo build --release -p o5s-server -p o5s-load
H5I_APP_SECRET=... target/release/o5s-load o5s --url http://127.0.0.1:3000 --agents 1000 --concurrency 64 --workload disjoint
```

The results below are for 1,000 authors with 64 requests in flight, on the host described in the Git comparison, on October 11, 2026, with release builds. *Propose* rows count proposals; the review row counts approve-plus-check pairs.

| Storage | Workload | Proposals/s (p95) | Review pairs/s (p95) |
|---|---|---:|---:|
| Local log | Adjacent lines, one file | 816 (96 ms) | 1,861 (84 ms) |
| Local log | One file each | 2,457 (47 ms) | 1,801 (86 ms) |
| Local log | One repository each | 2,650 (45 ms) | 1,849 (86 ms) |
| PostgreSQL 17 | Adjacent lines, one file | 208 (324 ms) | 176 (384 ms) |
| PostgreSQL 17 | One file each | 275 (265 ms) | 180 (370 ms) |
| PostgreSQL 17 | One repository each | 2,029 (60 ms) | 4,080 (25 ms) |

PostgreSQL ran in a local Docker container reached through Docker's port proxy, with a 32-connection pool. An uncontended approval takes 2.8 ms and a proposal 3.2 ms (`cargo run --release -p o5s-server --example pg_latency`): one simple-query roundtrip for `BEGIN`, the search path and the lock, two typed `SELECT`s, pipelined upserts, and `COMMIT`. A single hot repository is limited by those roundtrips under its lock, to about 350 transactions per second here. Unrelated repositories proceed in parallel. All runs completed with zero failed requests, zero retries and checked contents. The earlier SERIALIZABLE engine reached 95, 106 and 396 proposals/s on these workloads and retried 23% of attempts.

## Python driver

`scripts/load.py` drives the same HTTP commands as clients. It creates one identity per author and separate identities for review and check reports. Proposals run concurrently, followed by concurrent review/check reports and admission batches. Each run compares forward and reverse admission orders, checks every admitted patch's membership, and checks output bytes or retained conflict alternatives. It also checks that self-approval and unreviewed admission are refused without changing the channel view. A failed check gives a nonzero exit status.

The driver uses Python's standard library. It starts an isolated server with temporary storage and fresh test credentials when `--local-server` is given. It never reads a production signing secret. The server binary must already exist:

```sh
CARGO_TARGET_DIR=/var/tmp/o5s-build CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo build -p o5s-server
python3 scripts/load.py \
  --local-server /var/tmp/o5s-build/debug/o5s-server \
  --agents 1000 --concurrency 64 --batch 100 \
  --workload disjoint --restart --output /var/tmp/o5s-load.json
```

`--agents` is the number of distinct author identities and proposals. `--concurrency` limits simultaneous client requests. This command therefore measures 1,000 authors with at most 64 requests in flight. It does not measure 1,000 simultaneously active requests. Requests are closed loop: each worker waits for its response before sending more work. A fixed offered-rate driver, queue-depth measurements and overload tests are still needed for M9 in [the roadmap](roadmap.md).

## Workloads

| Workload | Submitted changes | Checked result |
|---|---|---|
| `disjoint` | One replacement per author, each on a separate line of the same immutable base | Exact combined bytes, no conflicts |
| `overlap` | All authors replace the first line with distinct content | All conflicting alternatives remain visible |
| `files` | One new file per author, in one repository | Every submitted file retains its bytes |
| `repositories` | One new file per author, each in its own repository | Every repository retains its submitted file |

All workloads also compare the entire forward and reverse channel views. The reverse channel admits batches in reversed patch order. Base fixture patches are admitted before measured work begins. `--history N` adds N sequential, reviewed and admitted base patches per repository before the concurrent phase. Their content is unchanged; this isolates dependency/history overhead without pretending to cover deletion/restoration semantics. `--edit-bytes` selects the replacement filler length. Actual submitted content includes the author prefix and an LF beyond that filler.

Setup, authentication probes and history construction are excluded from per-command latency and proposal throughput. The separate `identity_probe_requests` rows report authenticated repository-list reads for authors, the reviewer and the check reporter. These include the whole HTTP request and list response, so they cannot isolate token verification cost. `proposal_seconds` includes the full concurrent proposal phase. `admission_seconds` starts with that phase and ends after reports and both channels' admissions. Their throughput figures describe different amounts of work; the report names both. Request latencies include client encoding, transport and server response time. Percentiles use the sorted sample at `floor((n - 1) * percentile)`.

The report separates deliberate `expected_refusal_*` probes from ordinary commands. Their HTTP 422 responses count as failed HTTP requests in those probe rows and establish the expected policy rejection. Local Linux runs sample server RSS and CPU ticks through `/proc` every 100 ms and report the retained JSON file size. Those samples include setup and restart; brief peaks can occur between samples. File size is not the number of bytes written during the run. The binary fingerprint is captured before launch. Failed runs also write a report with `completed: false`, their settings and recorded request samples; they do not claim that correctness checks completed.

## Remote credentials

To test an existing server, supply a token file with this shape. The values below are placeholders:

```json
{
  "agents": ["AUTHOR_TOKEN_1", "AUTHOR_TOKEN_2"],
  "reviewer": "REVIEWER_TOKEN",
  "checker": "CHECK_REPORTER_TOKEN"
}
```

```sh
chmod 600 /path/to/load-tokens.json
python3 scripts/load.py --url https://your-o5s-origin.example \
  --tokens /path/to/load-tokens.json --agents 2 --concurrency 2
```

The driver verifies that the selected tokens name distinct users in one tenant. It excludes tokens, request bodies and response bodies from error logs, and rejects HTTP redirects so bearer credentials stay at the configured origin. Remote runs create repositories with a unique `load-*` prefix. Their stored records remain on the server after the test. Choose a disposable tenant and remove its fixtures through the server's supported storage procedure.

`--data PATH` retains an isolated local server's data instead of removing the temporary directory. The test signing secret remains in memory, so retained data is a replay/inspection fixture rather than a reusable authenticated deployment. `--restart` is available only for an isolated local server. It terminates the process normally, starts it again on the same storage, and compares every acknowledged channel view. This checks graceful restart, not a power failure or a process killed during commit.

## Recorded local results

The initial measurements were run on October 11, 2026, using a debug binary with debug information disabled, incremental compilation disabled, and the JSON store. The machine exposed 128 logical CPUs, an AMD EPYC 7502 CPU and 263,938,136 KiB of total memory on Linux 6.8.0-137-generic. Client and server ran on that same host over loopback. Several load jobs and development builds ran concurrently, so the rows are exploratory measurements rather than controlled comparisons.

The measured server binary's SHA-256 was `bcfaef121245afbc80e585a8485b7132baf5b35498f316d313e5dc3d37de16e7`. The reports, build settings and process-sampling results are preserved in [load-initial.json](evidence/load-initial.json). Each report lists the checks executed by that version of the driver.

| Workload | Authors / in-flight requests | Prior patches per repository | Proposals/s | Proposal p95 |
|---|---:|---:|---:|---:|
| Disjoint lines | 16 / 8 | 1 | 279.18 | 30.96 ms |
| Overlapping lines | 64 / 16 | 1 | 97.32 | 252.45 ms |
| Separate repositories | 32 / 16 | 1 | 171.26 | 107.56 ms |
| Separate files, history | 32 / 16 | 129 | 65.07 | 270.60 ms |

All four rows passed membership, byte/conflict, delivery-order and graceful-restart checks. These checks establish behavior for the submitted fixtures. They do not establish production throughput, multi-process safety or the behavior of the planned complete patch graph. The history fixture misses the roadmap's proposed 1,000 proposals/s and 250 ms p95 targets. The store still serializes writes through one process mutex and serializes the whole database after mutations; that implementation needs replacement before making a claim about thousands of active editors.

The initial four reports predate the refusal probes and process sampling. The current tool samples CPU and server RSS on local Linux runs, but does not measure database bytes written, retries, queue depth or response pagination. It does not execute CI, inject lost responses, crash during commits, exchange records between replicas or drive a million-operation history. Add those measurements and failure cases alongside the storage/protocol changes. Keep failed runs and their settings when reporting later results.

[load-release-smoke.json](evidence/load-release-smoke.json) records an eight-author overlapping-edit run against a later release binary, built with optimization level 3 and thin LTO. It passed policy refusals, conflict retention, delivery-order convergence and graceful restart, and includes per-role identity probes. Larger debug and release loads were active on the same machine. The changed code and shared host load prevent using this smoke as a speed comparison with the initial rows.

[load-1000-before-rollback.json](evidence/load-1000-before-rollback.json) records a completed release run with 1,000 authors and 64 requests in flight. It retained exact disjoint edits, passed policy refusals and survived graceful restart. Proposals reached 26.79/s with a 4,188.92 ms p95. The full proposal, review, check and two-channel admission sequence took 185.66 seconds. Sampled server RSS peaked at 1,623,692 KiB while retained JSON was 12,598,113 bytes. Proposal and review latency missed the proposed targets. This binary cloned the full database and allocated its complete JSON buffer for each write; the newer rollback path retains only changed facts, registry tails or channel membership and streams the JSON file. That path still serializes every write and rewrites the full snapshot. Its throughput needs a separate measurement.
