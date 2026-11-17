# o5s

`o5s` is the universal repository for any kind of data.

**Motivation**

- Why do we need a separate GitHub for source code, JFrog for build outputs, Docker Hub for container images,
PyPI and npm for packages, Hugging Face for LLM weights, and so on?
- Why not treat them all the same way? They are all just data, just sequences of 0s and 1s. 

**Our Answer**

- `o5s` treats every kind of data with the same three operations, `put`, `get`, and `replace`.
- It also ships adapters for popular tools such as Git, OCI, Hugging Face, PyPI, npm and Maven, so it fits into existing workflows as they are.

**Potential pushback**

- *"Merge in Git is necessary"* → Merge is just a special case of `replace`.
- *"Git packs source code better"* → Then use it for every domain.
- *"Hugging Face's Xet makes large weights fast"* → Then use it for every domain.
- *"Registries need their own rules"* → They're just per-namespace rules (`mutable`, `immutable`, `head-release`).
- *"One store means one bug breaks everything"* → Every decision is formally verified in Lean 4.

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
