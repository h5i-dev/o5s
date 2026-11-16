# Kubernetes manifests

Two shapes. Both run the distroless image (`deploy/Dockerfile`) as uid 65532 with a read-only
root filesystem, probe `/readyz` and `/healthz`, and drain on SIGTERM.

| file | shape | when |
|---|---|---|
| `local.yaml` | StatefulSet, **1 replica**, packs, refs and audit on one PersistentVolume | a single server; simplest |
| `s3.yaml` | StatefulSet, **N replicas**, packs and refs on S3 (`[repo] refs = "backend"`), small per-pod volume for staging and its own audit log | high availability, large stores |

```sh
kubectl create namespace o5s
# Tokens: entries from `o5s-server gen-token <principal>` (sha256 only; keep the tokens elsewhere).
kubectl -n o5s create secret generic o5s-tokens --from-file=tokens.toml=../tokens.example.toml
kubectl -n o5s apply -f local.yaml        # or s3.yaml (edit bucket, region, role ARN first)
```

Notes:
- **Replicas share nothing but S3.** In `s3.yaml`, refs are a create-only log on S3 (multi-host OCC,
  verified with several servers). Each pod keeps its own staging pack and its own hash-chained
  audit log on its volume (ship them to central storage with your log pipeline).
- **IRSA:** the ServiceAccount annotation `eks.amazonaws.com/role-arn` gives the pods web-identity
  credentials. The IAM policy is in `../README.md` ("S3 backend").
- **Ingress:** large uploads need generous proxy body limits and timeouts (e.g. nginx
  `proxy-body-size: 0`, `proxy-request-buffering: off`, read timeout ≥ 3600 s). Set
  `trust_forwarded_for = true` only behind a proxy that overwrites `X-Forwarded-For`.
- **Metrics:** `/metrics` on the service port (or `server.metrics_listen`). The dashboard is
  `../grafana-dashboard.json`, the alerts `../prometheus-alerts.yml`.
- **GC:** in `s3.yaml`, replicas run no background GC (`interval_secs = 0`). A CronJob triggers
  `POST /v1/admin/gc` hourly, with an admin token from the `o5s-admin` secret. The GC protocol is
  safe with concurrent writers, and one runner is enough.
