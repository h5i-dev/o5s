# Deploying pico2peta-server

## Files
| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage build to distroless `nonroot`. Build from the repo root: `docker build -f deploy/Dockerfile -t pico2peta-server .` |
| `server.toml` | Sample config for a systemd install (`/etc/pico2peta/server.toml`) |
| `server.docker.toml` | The same config, baked into the image (listens on `0.0.0.0:8080`, state in `/var/lib/pico2peta`) |
| `tokens.example.toml` | Token file format (`/etc/pico2peta/tokens.toml`, mode 0600) |
| `pico2peta-server.service` | Hardened systemd unit |

## systemd install
```sh
useradd --system --home /var/lib/pico2peta --shell /usr/sbin/nologin pico2peta
install -m 0755 target/release/pico2peta-server target/release/pico2peta /usr/local/bin/
install -d -m 0750 -o root -g pico2peta /etc/pico2peta
install -m 0640 -g pico2peta deploy/server.toml /etc/pico2peta/server.toml
pico2peta-server gen-token admin   # prints the token once, plus a [[tokens]] entry
$EDITOR /etc/pico2peta/tokens.toml # paste the entry (as [[tokens]]); chmod 0640, group pico2peta
install -m 0644 deploy/pico2peta-server.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now pico2peta-server
```
The unit runs `check-config` before every start. `systemctl stop` sends SIGTERM, and then:
1. `/readyz` turns to 503 and new API requests get 503 `shutting_down`.
2. In-flight requests get `shutdown_grace_secs` to finish.
3. The repository is flushed and the audit log is synced.

`TimeoutStopSec` (60 s) must stay above the grace period.

## Docker
```sh
docker run -d --name p2p -p 8080:8080 \
  -v p2p-data:/var/lib/pico2peta \
  -v /secure/tokens.toml:/etc/pico2peta/tokens.toml:ro \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  pico2peta-server
```
`docker stop` sends SIGTERM. Give it `--stop-timeout 45` or more if you raise the grace period.

## TLS
The server speaks plain HTTP/1.1 and h2c. Terminate TLS in a reverse proxy (nginx, envoy, an ALB) and keep the server on loopback or a private network. Bearer tokens must never cross an untrusted network in clear text. Behind a proxy, set `server.trust_forwarded_for = true` so audit records show the real client address. Never enable it when clients can reach the server directly, because they could then forge their address.

For large uploads, disable request buffering in the proxy (`proxy_request_buffering off; client_max_body_size 0;` in nginx). Otherwise the proxy spools whole uploads to disk.

## S3 backend
- Create the bucket first. The server never creates buckets.
- **Add a lifecycle rule that aborts incomplete multipart uploads** (for example after 1 day). If the process is killed in the middle of an upload, the parts stay billable until that rule removes them:
  ```json
  {"Rules":[{"ID":"abort-mpu","Status":"Enabled","Filter":{"Prefix":""},
    "AbortIncompleteMultipartUpload":{"DaysAfterInitiation":1}}]}
  ```
- Don't enable bucket versioning just to protect packs. They are create-only (`If-None-Match: *`) and never overwritten.
- Minimal IAM permissions on `arn:aws:s3:::bucket/prefix/*`: `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject` (used only by GC), `s3:AbortMultipartUpload`. Add `s3:ListBucket` on the bucket with a prefix condition.
- Staging (the open pack) and refs stay on the local disk at `repo.path`. Put that path on durable storage (EBS, not instance store) and back it up. Without it, refs are lost.

## Observability
- `GET /healthz` is liveness (the process is up). `GET /readyz` is readiness: it is 503 while draining, after an audit-log failure, or when the repository is unreachable. Neither needs credentials.
- `GET /metrics` serves OpenMetrics text with no credentials, so restrict it at the proxy or firewall. Key series:
  - `p2p_http_requests_total{method,route,status}` and `p2p_http_request_duration_seconds`
  - `p2p_http_in_flight`
  - `p2p_replace_total{outcome=ok|conflict|rejected}`
  - `p2p_auth_failures_total`, `p2p_authz_denied_total`, `p2p_audit_write_failures_total`
  - `p2p_s3_{requests,retries,throttled,failures}_total`
- Logs are JSON lines on stderr (journald). Each request logs one INFO line with status and latency, and its span carries `request_id`. The same id is returned in `x-request-id` and stored in audit records.

## Audit log
The log is append-only JSON lines at `audit.path`, fsync'd before each mutating call returns. Each record's `prev` field is the BLAKE3 hash of the previous line, so you can check the chain with:
```sh
pico2peta-server verify-audit /var/log/pico2peta/audit.jsonl
```
If an audit write fails (for example, the disk is full), the server refuses all further mutations, returns 503 from `/readyz`, and increments `p2p_audit_write_failures_total`. Rotation is not built in. To rotate, stop the server, move the file away, and start it again: a new chain starts in the new file. Archive the old file together with the last `prev` hash it ended on.

## Garbage collection
With `gc.interval_secs > 0`, the server advances the GC protocol on that schedule: plan, then sweep and repack, then delete, with each phase at least `gc.grace_secs` after the previous one. Admins can also trigger it with `POST /v1/admin/gc` (`?dry_run=true` for a report only). GC is safe with concurrent writers, but it relies on the **put→replace lease**: a client must make a ref point at a newly put object within `grace_secs`. Results show up in `p2p_gc_runs_total`, `p2p_gc_deleted_bytes_total`, the logs, and the audit log (`action = "gc"`). Any `dangling_refs` in a report means the repository is corrupt, so investigate it before the next run.

## Capacity notes
- Each transfer buffers at most about 2 MiB, whatever the object size. Measured: 19 MiB peak RSS growth while streaming 512 MiB up and back down (`crates/server/tests/memory.rs`).
- `max_concurrent_requests` bounds concurrent requests, including those still streaming bodies. Beyond it, the server answers 503 with `Retry-After: 1`.
- A client that stalls for 60 s in the middle of a transfer is disconnected.
