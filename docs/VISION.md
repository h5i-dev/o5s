# Vision (from the human, 2026-11-15). This overrides any "pick a battlefield" framing.

**It is wrong that Git, Hugging Face, JFrog Artifactory, container registries, package indexes and so on are separate systems.** pico2peta replaces all of them with **one** store, and every kind of data is handled **the same way** in the core.

There is no "main battlefield." We do not give up on source code because a generic compressor lost to Git, and we do not specialize the product on model weights. Every domain must be served at least as well as its specialist system does it.

## Architecture: uniform core, domain-specific fronts

```
  git clients   huggingface_hub   docker/oras   pip/uv   npm   maven/gradle   generic HTTP/CLI
      │               │               │           │       │         │               │
  ┌───┴───┐      ┌────┴────┐      ┌───┴──┐    ┌───┴─┐  ┌──┴─┐  ┌────┴──┐      ┌─────┴────┐
  │ Git   │      │ HF/Xet  │      │ OCI  │    │PyPI │  │npm │  │ Maven │ ...  │ generic  │   FRONTS
  │ front │      │ front   │      │front │    │front│  │front│ │ front │      │ front    │   (domain wins live here)
  └───┬───┘      └────┬────┘      └───┬──┘    └───┬─┘  └──┬─┘  └────┬──┘      └─────┬────┘
      └───────────────┴──────────────┴───────────┴───────┴─────────┴────────────────┘
                     Front API: domain profile = chunker + encoding pipeline
                     (delta/base selection, grouping, codecs) + protocol + name rules
  ═══════════════════════════════════════════════════════════════════════════════════════
   CORE (identical for every domain): content-addressed immutable objects, packs and index,
   names with OCC replace, namespace rules, provenance, authz and audit, GC/retention,
   signing and promotion, search, S3/local backends, verified pure core (Lean)
```

**The core knows nothing about Git, safetensors or wheels.** It stores objects, moves names and records provenance, the same way for everyone.

**A front owns everything domain-specific:**
- The wire protocol, so existing clients (git, huggingface_hub, docker, pip, npm, maven) work unchanged.
- External-id mapping (SHA-1, sha256 digests, Xet hashes).
- Domain encoding: Git-style windowed delta search for source, byte grouping and tensor-aligned XOR deltas for weights, layer handling for OCI.
- Upstream proxying and caching, and domain name rules (published versions immutable, branches mutable, ...).
- Fronts plug in through a **Front API** in the core. Encoding steps are generic, reusable building blocks (chunkers, codecs, derived-object records such as `base + patch`), so one front's trick is available to all fronts.

**Cross-domain value comes for free** because everything sits in one core:
- One permission model and audit log.
- Provenance from commit → build → wheel/image → trained model.
- Dedup across domains: the same tarball inside a wheel, an image layer and a repo.
- One GC and retention policy, and one dev → prod promotion and signing flow.

## Success criteria: every front at specialist level (pre-register the thresholds)

| front | replaces | must match or beat on |
|---|---|---|
| Git | GitHub/Gitea/git daemon storage | stored size vs `git gc --aggressive` (≤ 1.0×), clone/fetch/push time and bytes on the wire |
| HF/Xet | Hugging Face Hub + Xet | dedup/storage on real model families, upload/download throughput, `huggingface_hub` compatibility |
| OCI | Docker registry / Harbor | push/pull time, storage with cross-image dedup, conformance (OCI distribution-spec tests) |
| PyPI / npm / Maven | JFrog Artifactory / Nexus | API compatibility with real clients, proxy-cache hit latency, storage |
| generic | S3 + ad-hoc versioning | throughput, range reads, cost |

Exp1/1b/1c show that a *generic* compressor in the core loses to Git on source. That is a finding for the **Git front**: it must do Git-class encoding (for example Git's own windowed delta search, or keeping Git pack representations as content-addressed objects) and reach parity. It is not a reason to drop the domain.
