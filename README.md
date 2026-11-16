# pico2peta

**One store for immutable, content-addressed data** (source code, packages, container images, model weights)
that replaces Git hosting, Hugging Face, Artifactory and registries with a single, verified core
(`docs/VISION.md`).

- **One core, every domain.** Objects are BLAKE3-256 content addresses, stored in append-only packs. Small objects
  are bundled; large ones are cut by content-defined chunking into deduplicated chunks under a manifest tree. Every
  read is verified, in strict mode by default (`docs/ARCHITECTURE.md`).
- **Names** move only by optimistic compare-and-swap over the read set a writer depended on (`replace`, multi-name
  `update`). Per-namespace rules (`mutable`, `immutable`, `head-release`) make published names immovable.
- **Fronts** speak each domain's protocol on top of the same core: Git (smart HTTP, SHA-1 and SHA-256, shallow and
  partial clone), OCI distribution, Hugging Face Hub and Xet, PyPI, npm and Maven (with upstream proxy-caches).
  Domain-specific storage choices (chunker, derived versions, byte grouping) are **namespace profiles**, not core
  code.
- **Similarity repack** (default, every domain): GC lays out repacked objects by content similarity. On the Exp1
  corpus it stores 0.79–0.95× `git gc --aggressive` (geomean 0.84), including the held-out redis and git.git
  (`docs/EXP1.md`, Experiment 3).
- **Provenance, signing, promotion, search, retention:**
  - provenance records link a commit → build → wheel → image → model, with lineage and impact queries across
    domains;
  - in-toto/DSSE attestations;
  - policy-gated `promote` (dev → prod) as one audited OCC transaction;
  - search over names and metadata;
  - per-profile retention.
- **Storage:** local disk or any S3-compatible store; many writers per repository; crash consistency tested at
  every write and fsync point, and with real SIGKILLs.
- **Verification:** a Lean 4 model with machine-checked proofs, and a Rust↔Lean differential test (`lean/README.md`).
  The proofs cover content immutability, no lost updates, immutable names never moving, authorization, GC safety,
  front-owned prefixes, and promotion.

## Quick start (CLI: low-level plumbing over the core; reference in `docs/CLI.md`)

```sh
cargo build --release
alias p2p=target/release/pico2peta
R=/data/repo

# A repository whose models/ namespace derives each new version from the previous one
# (stored as a delta, planned by the safetensors front):
p2p -C $R init --profile weights=front:safetensors,derive:previous --namespace models/=weights
H=$(p2p -C $R put --for-name models/acme/main model.safetensors)   # stored per the profile; prints the hash
p2p -C $R replace models/acme/main $H --expect absent              # names move only by OCC
p2p -C $R get $H --range 0..1024 > head.bin                          # verified reads

p2p -C $R ns set releases immutable                                 # published names never move
p2p -C $R update dev/app=$H prod/app=$H --expect dev/app=absent --expect prod/app=absent

p2p -C $R prov record train --in data=$D --out model=$H              # provenance
p2p -C $R prov lineage models/acme/main                              # a HASH or a NAME
p2p -C $R attest keygen ci.key                                      # prints "<keyid> <pubkey>"
p2p -C $R attest create dev/app --key ci.key
p2p -C $R promote dev/app prod/app --require-key $PUB --require-provenance
p2p -C $R search tensor library                                     # names + metadata
p2p -C $R gc                                                        # from cron; default grace 1h

# Against a server (domain clients such as git, pip, npm, docker and huggingface_hub talk to its fronts):
export P2P_REMOTE=https://p2p.example.com P2P_TOKEN=p2p_...
p2p put --for-name models/acme/main model.safetensors
```

CLI exit codes: 0 ok, 1 error, 2 not found/absent, 3 OCC conflict, 4 forbidden (namespace rule or promotion
policy), 5 unauthorized.

## Quick start for each front (server; details in `docs/API.md`, deployment in `deploy/README.md`)

```sh
pico2peta-server gen-token admin      # prints the token once, plus a [[tokens]] entry for tokens.toml
pico2peta-server serve --config deploy/server.toml
export S=p2p.example.com TOKEN=p2p_...
```

| front | the real client, unmodified |
|---|---|
| **Git** | `git clone https://any:$TOKEN@$S/git/team/proj.git`, then `git push` (each ref update is an OCC compare-and-swap) |
| **OCI** | `docker login $S -u any -p "$TOKEN"`; `docker push $S/team/app:1.0`; `oras push $S/team/app:sbom ./sbom.json` |
| **Hugging Face** | `export HF_ENDPOINT=https://$S HF_TOKEN=$TOKEN`; `hf upload team/tiny ./model`; `hf_hub_download` / `snapshot_download` (LFS and Xet) |
| **PyPI** | `twine upload --repository-url https://$S/pypi/team/ -u token -p "$TOKEN" dist/*`; `pip install --index-url https://token:$TOKEN@$S/pypi/team/simple/ mypkg` |
| **npm** | `.npmrc`: `registry=https://$S/npm/team/` and `//$S/npm/team/:_authToken=${TOKEN}`; `npm publish`, `npm install` |
| **Maven** | `settings.xml` server `p2p` (user `token`, password = token); `mvn deploy -DaltDeploymentRepository=p2p::https://$S/maven/releases` |
| **native** | `PUT /v1/objects`, `POST /v1/refs/...` (OCC), provenance/attestations/promotion/search under `/v1` |

PyPI, npm and Maven can proxy-cache their public upstreams (`[pypi.upstream.<index>]` etc.). One provenance log,
audit log and policy cover every front.

## Layout

| crate | role |
|---|---|
| `pico2peta-core` | `Hash`, `Error`, `Encoding`, `ReadVerify`, the `ObjectStore`/`Repository`/`Backend` traits, LocalDisk, conformance suite, fault injection |
| `pico2peta-pack` | pack store: packs, indexes, chunking, codecs, frames (incl. reference frames), derived objects, recovery |
| `pico2peta-refs` | ref store: journal with group commit, or a create-only log on a backend; OCC; namespace rules |
| `pico2peta-repo` | `LocalRepo`: profiles, the Front API, idmap, GC with similarity repack, fsck |
| `pico2peta-order` | domain-agnostic similarity features, ordering and framing |
| `pico2peta-chunk` | content-defined chunkers (FastCDC, byte-exact Xet) |
| `pico2peta-prov` | provenance, search, retention |
| `pico2peta-attest` | in-toto/DSSE attestations, Ed25519, policy-gated promotion |
| `pico2peta-weights` | the safetensors front (tensor-aligned deltas, byte grouping) |
| `pico2peta-cli` | the `pico2peta` binary |
| `pico2peta-server`, `-client`, `-backend-s3` | HTTP server with every front, remote client, S3 backend |
| `bench/*`, `fuzz/`, `lean/` | experiments and benchmarks, fuzz targets, the Lean model |

- **Design:** `docs/VISION.md`, `docs/ARCHITECTURE.md`, `docs/PLAN.md`.
- **Status:** `docs/PROGRESS.md`, `docs/NET_PROGRESS.md`, `docs/ROADMAP.md`.
- **Measurements:** `docs/EXP1.md`, `docs/BENCH.md`.
- **Reference:** HTTP API `docs/API.md`, CLI `docs/CLI.md`.
