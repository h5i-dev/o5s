# o5s

Why do we need a separate GitHub for source code, JFrog for build outputs, Docker Hub for container images,
PyPI and npm for packages, Hugging Face for LLM weights, and so on? They are all just data: sequences of 0s
and 1s. Why not treat them all the same way? All the operations we really have are push, pull, and overwrite.

o5s is one content-addressed, immutable store for all of it: source code, packages, container images and model
weights. The clients you already use (`git`, `docker`, `pip`, `npm`, `mvn`, `huggingface_hub`) talk to it
unmodified.

## Potential pushback

- "Merge in Git is necessary" → Merge is just a special case of overwrite.
- "Xet makes large weights fast" → Then use it for every domain.
- "Git packs source code better" → Similarity repack stores 0.79–0.95× of `git gc --aggressive`, for any data.
- "Registries need their own rules" → They're per-namespace rules: `mutable`, `immutable`, `head-release`.
- "One store means one bug breaks everything" → Every decision is proved in Lean on the extracted code.
- "Separate systems are fine" → Not for lineage: one store links commit → build → package → image → model.

## CLI example

The `o5s` CLI is the low-level plumbing over the core (reference: `docs/CLI.md`).

```sh
cargo build --release
alias o5s=target/release/o5s
R=/data/repo

o5s -C $R init
H=$(o5s -C $R put model.safetensors)               # stores the file, prints its BLAKE3 hash
o5s -C $R get $H > model.safetensors               # every read is verified against the hash
o5s -C $R get $H --range 0..1024 > head.bin

o5s -C $R replace models/acme/main $H --expect absent   # a name moves only by compare-and-swap; prints "ok 1"
o5s -C $R replace models/acme/main $H2 --expect 1       # move it from version 1 (exit 3 if someone else moved it)
o5s -C $R ns set releases immutable                     # published names never move

o5s -C $R prov record train --in data=$D --out model=$H # provenance
o5s -C $R prov lineage models/acme/main
o5s -C $R promote dev/app prod/app --require-provenance  # policy-gated, audited
o5s -C $R gc

# Against a server:
export O5S_REMOTE=https://o5s.example.com O5S_TOKEN=o5s_...
o5s put --for-name models/acme/main model.safetensors
```

Exit codes: 0 ok, 1 error, 2 not found, 3 conflict, 4 forbidden, 5 unauthorized.

## Supported fronts

```sh
o5s-server gen-token admin                    # prints the token once, plus its tokens.toml entry
o5s-server serve --config deploy/server.toml  # local disk or any S3-compatible bucket
export S=o5s.example.com TOKEN=o5s_...
```

| front | the real client, unmodified |
|---|---|
| **Git** | `git clone https://any:$TOKEN@$S/git/team/proj.git`, then `git push` (SHA-1 and SHA-256, shallow and partial clone) |
| **OCI** | `docker login $S -u any -p "$TOKEN"`; `docker push $S/team/app:1.0`; `oras push $S/team/app:sbom ./sbom.json` |
| **Hugging Face** | `export HF_ENDPOINT=https://$S HF_TOKEN=$TOKEN`; `hf upload team/tiny ./model`; `snapshot_download` (LFS and Xet) |
| **PyPI** | `twine upload --repository-url https://$S/pypi/team/ -u token -p "$TOKEN" dist/*`; `pip install --index-url https://token:$TOKEN@$S/pypi/team/simple/ mypkg` |
| **npm** | `.npmrc`: `registry=https://$S/npm/team/` and `//$S/npm/team/:_authToken=${TOKEN}`; `npm publish`, `npm install` |
| **Maven** | `settings.xml` server `o5s` (user `token`, password = token); `mvn deploy -DaltDeploymentRepository=o5s::https://$S/maven/releases` |
| **native** | `PUT /v1/objects`, `POST /v1/refs/...` (compare-and-swap), and provenance, attestations, promotion and search under `/v1` |

PyPI, npm and Maven can also proxy-cache their public upstreams. Every front shares one store, so the same bytes
pushed through Git, PyPI and OCI are stored once, under one provenance log, audit log and policy.
