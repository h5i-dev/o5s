# Roadmap to production (from the human, 2026-11-15). Work through it autonomously.

**Goal:** `docs/VISION.md`. One production-grade store replaces Git hosting, Hugging Face, Artifactory and registries, and every front performs at specialist level or better.

**Standing orders**
- Work this list top to bottom **without waiting for further instructions**. When an item is done, start the next one.
- Make decisions yourself. Write genuine human-only questions under "Questions for human" in your progress doc, pick the safe default, and keep going.
- Commit and push as you go, on task branches cut from `origin/main`: gate green (check the exit status), no force-push, never touch `main`, no Claude attribution. The human opens the PRs and merges; there are no `demo`/`net` branches.
- Every measurement is pre-registered in `PLAN.md` before its code. Report results honestly against the frozen thresholds.
- Disk hygiene: target dir < 4 GB, no TMPDIR leaks, nothing under `/home` or `/tmp`, perf build-id cache on scratch. `/mnt/scratch` has about 14 GB free. If space blocks an item, record it and do the next item.

## Core session (`o5s`)
1. **Finish in-flight work:** `read_verify = strict (default) | eof` (see PROGRESS "Answered"), and the idmap fix handoff with net.
2. **Experiment 3 held-out run:** redis and git.git, once, under the bench lock (protocol in "Batch measurement" below). Write the verdict in `EXP1.md`/`PROGRESS.md`. If it fails on a repo, analyse why and improve the generic orderer or framer. Never add a Git-specific path.
3. **P5 cross-domain features (core side):**
   - **Provenance across fronts:** link a Git commit → build → wheel/npm/OCI artifact → trained model checkpoint. Fronts record provenance through one core API, with lineage and impact queries across domains.
   - **Signing and promotion:** signed attestations (in-toto/DSSE envelope format; keys pluggable, Sigstore later). `promote(src_name → dst_name)` between namespaces, e.g. dev → prod, gated by policy: required signatures, provenance present. Audit everything. Promotions are OCC transactions.
   - **Cross-domain dedup:** a test showing the same bytes pushed via Git, PyPI and OCI are stored once. Report the measured savings.
   - **Search:** an index over names, front metadata and provenance (prefix, exact, and simple full-text over metadata), kept consistent via the ref log.
   - **Retention policies** per namespace profile (keep N versions / keep published forever / deletable if regenerable via provenance), enforced by GC.
4. **Scale testing within the disk budget:** synthetic metadata-heavy runs (10^7–10^8 small objects, deterministic seed) for index, MIDX, ref store (10^6–10^7 names), GC and provenance. Find and fix the first bottleneck at each scale. Record the limits in ARCHITECTURE.md.
5. **Verification catch-up:**
   - Bring the Lean model and proofs (P1–P5) in line with the current code: backend ref log, group commit, update_many, Frame3 references and GC, fronts' owned prefixes, promotion.
   - Extend the Lean↔Rust difftest to the new operations.
   - No `sorry`. Assumptions listed in VERIFICATION docs.
6. **Security review of the core:** decoder limits, path handling, resource exhaustion (huge manifests, deep delta/reference chains, zip-bomb-like frames), concurrency races. Fix the findings, and add fuzz targets for every new format (Frame3, attestations, the search index).
7. **Release prep (v0.1):**
   - CHANGELOG, README rewrite (what it is, quick start for each front), ARCHITECTURE/API docs current, upgrade and format-compatibility policy.
   - Tag a release candidate (`v0.1.0-rc.N`) on its release branch (`release/v0.1.0-rc.N`, cut from `main`).
   - Write `docs/RELEASE.md` with the merge-to-main checklist. The human merges to `main`.
8. **Line-level combine for concurrent edits of one file (from the SWE-bench pilot, 2026-11-17).**
   - **Evidence.** In the pilot (two agents sharing a working copy through o5s, one name per file), the o5s arm
     solved 7/10 against git's 6/10. Same-file edits to different hunks became manual conflicts, though:
     django-13344 took 1068 s against 388 s with git.
   - **Prototype.** In the experiment's client (`/mnt/scratch/swe-o5s/bin/ws`, with `harness/merge3.py` and
     tests next to it), `ws pull` now combines base, ours and theirs line by line. Base is the version last
     synced, from `.ws/state.json`. It reports a conflict only when hunks overlap or touch; the combined file is
     a local change, pushed with the usual OCC `update`. The pilot run kept no store, so its conflicts can't be
     replayed exactly; the rerun below is the test.
   - **To make it a product feature:**
     - **Where it lives:** a workspace front and CLI (`o5s ws`), not the core. The store keeps bytes and
       names; combining is client-side and stays domain-agnostic. The core already provides what it needs:
       per-name versions, OCC multi-name `update`, and reads by hash.
     - **A merge kernel with theorems** (`crates/kernel`, `verify/core`), proved over a specified diff:
       - with no conflict, the result applies both sides' hunks;
       - `merge3(b, o, b) = o` and `merge3(b, b, t) = t`;
       - merging is symmetric up to which side is "ours";
       - a conflict is reported exactly when hunks overlap or touch.

       The diff itself (Myers or histogram) is either extracted and proved, or a spec'd trusted component.
     - **Merge base from the store, not from client state.** Each replace could carry the parent hash it
       replaced, as a provenance record (`prov record replace --in parent=…`), so any client, or a fresh clone,
       finds the base of a name's two versions. Today the base exists only in the client's state file.
     - **Limits and safety:**
       - cap the size and line count of files to combine, since diffing is superlinear, and fall back to a
         conflict above the cap;
       - treat binary or NUL content as a conflict;
       - define line endings (CRLF, a missing final newline) and decoding (bytes, never lossy UTF-8);
       - make the result deterministic.
     - **Conflict presentation:** keep ours untouched and stash theirs (agents did well with that), with an
       optional `diff3`-style marker mode for humans, and a machine-readable conflict list (base ranges) for
       tools.
     - **Per-front merge drivers.** A front can supply a structure-aware combine for its formats (JSON and TOML
       keys, notebooks, lockfiles) through the `Front` trait, falling back to lines. This is where domain
       expertise wins without leaking into the core.
     - **Path-level cases:** renames and moves, add/add, modify/delete, and directory structure. These build on
       the tree object (ARCHITECTURE.md), so a workspace snapshot is one tree with per-entry combines.
     - **Evaluation,** pre-registered in PLAN.md: rerun the SWE-bench pilot with the combine enabled.
       Metrics are manual conflicts per task, wall time against the git arm (django-13344 above all), and
       solved count. No silent mis-merge is allowed: every combined file must pass the task's tests or be
       re-reviewed.

## Net session (`o5s-net`)
1. **Finish in-flight work:** the idmap ranged-lookup fix (agreed with core), and the server side of `read_verify`.
2. **Front completeness, each tested with the real unmodified client:**
   - **OCI:** referrers API, and cosign/notation signatures stored as OCI artifacts (wire them to the core attestations once item 3 lands).
   - **Git:** `object-format=sha256`, shallow and partial clone (`--filter=blob:none`), reachability bitmaps or an equivalent so clone doesn't walk every object, and served-pack size closer to git's (currently 1.8×).
   - **HF:** whatever `huggingface_hub` features remain (`list_repo_refs`, `create_commit` with deletes, dataset and space repo types, as far as is meaningful).
   - **PyPI/npm/Maven:** auth tokens per client tool, yank/deprecate/unpublish semantics mapped to name rules, and upstream proxy-cache correctness (TTL, offline mode, stale-if-error).
3. **P5 cross-domain features (front side):** every front records provenance through the core API (e.g. a wheel built from Git commit X; an image layer containing wheel Y). Promotion endpoints and policy checks in the server. One audit log query across fronts.
4. **Batch benchmarks vs specialist systems (§6.5),** using the protocol below: git daemon, CNCF Distribution, devpi/pypiserver, verdaccio, a Maven repository (e.g. reposilite) if it runs locally, and HF vs the generic-front reference. Include the redis Git-front numbers that were stopped earlier. Results go in `BENCH.md` and `NET_PROGRESS.md`, against the frozen thresholds.
5. **Dogfooding:**
   - Run a o5s server on this host and use it for this project's own needs: mirror this repo through the Git front, and route pip and npm used by the tests through the PyPI and npm proxy-caches.
   - Keep it running and record every bug found.
   - Do a backup → restore → fsck drill, and an upgrade drill: an old format, then the new binary.
6. **Security review of the server:** authn/authz bypass, path traversal in every front's URL parsing, request smuggling, body-size and time limits, slowloris, upstream SSRF in the proxy-caches, token handling. Fuzz the HTTP-facing parsers. Fix everything found.
7. **Ops:** metrics and dashboards (a Grafana JSON in `deploy/`), alerting rules, Kubernetes manifests or a Helm chart, an S3 deployment guide, rate limiting and quotas per namespace.

## Batch measurement protocol (both sessions)
- **When:** only when an item above calls for it, never for development.
- **Locking:** take the bench lock (`flock /mnt/scratch/pico2peta/bench.lock`). Announce the start and expected duration to the other session with a message, and keep each window ≤ 60 minutes. The other session keeps working, but builds at `nice -n19 -j1` during the window.
- **Reporting:** report the load average for every result. If load from outside our sessions makes a result unusable, say so and rerun once later.
