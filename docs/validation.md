# Local validation

Validated on 2026-10-11. No Cloudflare resources were provisioned, no deployment was performed and no Git push was made.

- Rust workspace: 21 tests passed, including 10,000 independent patch deliveries, 10,000 conflicting agent proposals, a 10,000-edit chain, differential DAG materialization, exhaustive three-vertex graph/liveness projection, atomic batch refusal, tenant isolation, concurrent API requests, restart, single-writer locking and real bare-Git export.
- Cloudflare ESM adapter: three Node tests passed with mocked Artifacts capabilities. These establish local contract behavior, not live service integration.
- Current Rust kernel was regenerated through Charon/Aeneas. `h5i app check o5s` passed the axiom gate for **75 theorems in five hand-written modules**, including retained arithmetic/storage regression coverage. The substantive patch/graph scope and explicit assumptions are in [proofs](proofs.md).
- The three merge-guard mutants were previously caught by the proof suite: approval, passing checks and dependency closure. A caught mutant compiled/extracted and then failed a Lean theorem; compilation failure was not counted as a catch.
- Admin JavaScript parsed successfully. A local scripted h5i browser exercised authentication, proposal recording, building on an existing patch, independent approval/checks, batched integration, explicit conflict display, composing a resolution with both dependencies, integration of that resolution and local Git export. Real API requests and resulting state were checked. The browser's limited CSS/select rendering is not evidence of a Chromium layout test.
- The reproducible [scaling example](../o5s/platform/examples/scaling.rs) measured 10,000 patch updates and a 10,000-vertex dead chain. See [scaling](scaling.md) for the exact work counts and limitations. There was no distributed network load test.

Run `./scripts/verify.sh` for formatting, Rust tests, Node adapter tests, UI syntax, extraction and the Lean gate. Run `h5i app mutate o5s --only merge_` for the semantic guard mutants. Both require the installed pinned proof tooling. When invoking Lake directly, run from `o5s/proofs` so elan selects its `lean-toolchain`; selecting a different Lean version can invalidate shared build artifacts.
