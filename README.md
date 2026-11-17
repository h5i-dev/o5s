# o5s

`o5s` is the universal repository for any kind of data.

**Motivation**

- Why do we need a separate GitHub for source code, JFrog for build outputs, Docker Hub for container images,
PyPI and npm for packages, Hugging Face for LLM weights, and so on?
- Why do we keep reinventing the same infrastructure for versioning, access control, and release management?
- Why not treat them all the same way? They are all just data, just sequences of 0s and 1s.

**Our Answer**

- `o5s` treats every kind of data with the same three operations, `put`, `get`, and `replace`.
- Shared infrastructure handles versioning, access control, provenance, garbage collection, and per-namespace policies.
- It also ships adapters for popular tools such as Git, OCI, Hugging Face, PyPI, npm and Maven, so it fits into existing workflows as they are.

**Potential pushback**

- *"Why should I use o5s?"* → Less infrastructure to manage and no need to change the tools you already use.
- *"Isn't this just S3 with adapters?"* → S3 stores data. o5s manages how data is versioned, referenced, and released.
- *"Merge in Git is necessary"* → Merge is just a special case of `replace`.
- *"Git packs source code better"* → Then use it for every domain.
- *"Hugging Face's Xet makes large weights fast"* → Then use it for every domain.
- *"Registries need their own rules"* → They're just per-namespace rules (`mutable`, `immutable`, `head-release`).
- *"One store means one bug breaks everything"* → Every decision is formally verified in Lean 4.
- *"How do I migrate everything?"* → Start with one registry and keep your existing tools.

## CLI example

The `o5s` CLI is the low-level plumbing over the core (see `docs/CLI.md`).

```sh
cargo build --release
alias o5s=target/release/o5s
R=/data/repo

# Store and read objects
o5s -C $R init
H=$(o5s -C $R put model.safetensors)                    # stores the file, prints its BLAKE3 hash
o5s -C $R get $H > model.safetensors                    # every read is verified against the hash
o5s -C $R get $H --range 0..1024 > head.bin

# Move names
o5s -C $R replace models/acme/main $H --expect absent   # a name moves only by compare-and-swap; prints "ok 1"
o5s -C $R replace models/acme/main $H2 --expect 1       # move it from version 1 (exit 3 if someone else moved it)
o5s -C $R ns set releases immutable                     # published names never move

# Provenance, promotion, GC
o5s -C $R prov record train --in data=$D --out model=$H
o5s -C $R prov lineage models/acme/main
o5s -C $R promote dev/app prod/app --require-provenance  # policy-gated, audited
o5s -C $R gc

# Run a server
o5s-server gen-token admin                    # prints the token once, plus its tokens.toml entry
o5s-server serve --config deploy/server.toml  # local disk or any S3-compatible bucket
export O5S_REMOTE=https://o5s.example.com O5S_TOKEN=o5s_...
o5s put --for-name models/acme/main model.safetensors
```
Each front speaks its domain's protocol, so the real client works unmodified
(`S=o5s.example.com`, `TOKEN=o5s_...`).

```sh
# Git (SHA-1 and SHA-256, shallow and partial clone)
git clone https://any:$TOKEN@$S/git/team/proj.git
git push

# OCI
docker login $S -u any -p "$TOKEN"
docker push $S/team/app:1.0
oras push $S/team/app:sbom ./sbom.json

# Hugging Face (LFS and Xet)
export HF_ENDPOINT=https://$S HF_TOKEN=$TOKEN
hf upload team/tiny ./model
hf download team/tiny

# PyPI
twine upload --repository-url https://$S/pypi/team/ -u token -p "$TOKEN" dist/*
pip install --index-url https://token:$TOKEN@$S/pypi/team/simple/ mypkg

# npm
npm config set registry https://$S/npm/team/
npm config set //$S/npm/team/:_authToken $TOKEN
npm publish
npm install mypkg

# Maven (~/.m2/settings.xml: <server><id>o5s</id><username>token</username><password>${env.TOKEN}</password></server>)
mvn deploy -DaltDeploymentRepository=o5s::https://$S/maven/releases

# Native API
curl -T big.bin -H "Authorization: Bearer $TOKEN" https://$S/v1/objects   # {"hash","size"}
curl -H "Authorization: Bearer $TOKEN" -r 0-1023 https://$S/v1/objects/<hash>
```

## Benchmarks

Each adapter is measured against the specialist server for its domain, driven by the real, unmodified client
(v0.1.0-rc.1, one 4-vCPU AMD EPYC host with 15 GB RAM and NVMe, medians of 3 runs).
Numbers are o5s divided by the specialist, so lower is better.

| Adapter | Specialist | Push / upload | Pull / download | Stored bytes |
|---|---|---|---|---|
| Git (jq, ripgrep, redis) | `git daemon` | 0.80–1.02× | clone 0.73–0.97×, fetch 0.71–0.83× | clone wire 0.58–1.10× |
| OCI | Distribution (`registry`) | 0.60× | 1.74× | 1.00× |
| Hugging Face (Xet) | Xet dedup | server time 0.37× of a plain upload | server CPU 0.99× of a plain download | 0.91× |
| PyPI | devpi | | cold 1.10×, hot 1.18× | 0.47× |
| npm | Verdaccio | | cold 1.06×, hot 1.06× | 0.11× |
| Maven | Reposilite | | cold 0.99×, hot 1.03× | 0.93× |

## Formally verified in Lean 4

Proved on the Rust code itself via [h5i](https://github.com/h5i-dev/h5i).

- **Verified reads.** A read returns bytes only if they hash to the requested address.
- **Immutable objects.** A `put` never overwrites a stored object.
- **No lost updates.** A `replace` commits only if nothing it read has changed since.
- **Published names never move.** Writes and deletes under an immutable namespace are rejected.
- **GC never loses live data.** A pack is deleted only if no live object depends on it.
- **Authorization.** Only permitted principals can read or move a name.
