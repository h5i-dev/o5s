# Releasing pico2peta

v0.1 is the first release. It is cut from `demo` as release candidates (`v0.1.0-rc.N`). The human merges a
candidate to `main`; agents never push `main`.

## Compatibility policy (from v0.1)

**Versions.**
- **0.x:** the on-disk format is the stable contract; the CLI and Rust API may still change between minor
  versions.
- **1.0:** the stable HTTP API (`/v1`, docs/API.md) and the CLI exit codes join that contract.
- Every breaking change is listed under "Changed (breaking)" in CHANGELOG.md.

**On-disk data is never silently rewritten.**
- **Readers keep reading every format any released version wrote.** Today that includes `P2PPACK1`,
  `P2PIDX01`/`02` and the v1 `Frame`, next to the current formats.
- **New formats are additive:**
  - a new record kind, codec, magic or config key;
  - old writers never produce them;
  - old readers refuse them with a clear error (`Corrupt`/`Parse`), never misread them.
  - Example: codec 2 (`GroupedZstd`) is refused by readers that predate it.
- **Writers may switch to a newer format** only when every reader of the *previous* release can either read
  it or refuses it cleanly. Data written by the old format stays valid; GC repacks may rewrite it over time.
- **`config.toml` `format`** (currently `1`) is bumped only for a change old readers can't refuse
  record-by-record. Readers refuse unknown versions at open. New keys get defaults, so old configs stay valid.
- **Derived local caches** may be deleted at any time and are rebuilt: `idx-cache/*.idx3`, `midx-*.midx`.

**Upgrades.**
- Within 0.1.x, a new binary opens existing repositories in place; there is no migration step.
- Rolling upgrades of a multi-writer repository are safe if no new-format records are written until every
  writer runs the new version. Release notes say when a release starts writing a new format.
- Downgrade is supported only to a version that can read every format present. CHANGELOG's "On-disk formats"
  section lists what each release writes.

## Formats written by v0.1

| object | magic / id | notes |
|---|---|---|
| pack | `P2PPACK2` | header nonce; `P2PPACK1` read |
| pack index | `P2PIDX03` | ~44 B/entry; `P2PIDX01`/`02` read |
| record | `P2PR` header | kinds: Blob 0, Manifest 1, Frame 2 (read only), Frame2 3, Delta 4, Frame3 5. Codecs: Raw 0, Zstd 1, GroupedZstd 2 |
| multi-pack index (local cache) | `P2PMIDX1` | derived |
| ref journal / snapshot | `P2PREFJ1` / `P2PREFS1` | multi-host log entries `P2PLOGE1` |
| provenance record / log / checkpoint | `P2PPROV1` / `P2PPLOG2` / `P2PPIDX1` | `P2PPLOG1` read |
| attestation | provenance record, op `attest`, DSSE envelope as params | in-toto Statement v1 |
| front id maps (segment map) | `P2PSEG01` / `P2PMAP01` | |
| front records | `P2PPYPI1`, `P2PPYYK1`, `P2PNPMV1`, `P2PNPMT1`, `P2PNPMD1`, `P2PMVN01`, `P2PXORB1`, `P2PXFIL1` | owned by each front |

## Release-candidate checklist (agents, on `demo`)

1. `scripts/check.sh` exits 0 on a clean checkout of the commit to tag. Check the exit code; don't read it
   off a pipe.
2. Merge the latest `net` and re-run the gate. Both sessions confirm that nothing is in flight.
3. CHANGELOG.md:
   - move `[Unreleased]` to `[0.1.0-rc.N] - <date>`;
   - breaking changes listed;
   - the "On-disk formats" section matches the table above.
4. Docs current:
   - README quick start for each front;
   - docs/API.md (API version, endpoints);
   - docs/CLI.md regenerated (`scripts/gen-cli-doc.sh`);
   - ARCHITECTURE.md limits;
   - SECURITY.md;
   - BENCH.md numbers with their dates and loads.
5. Fuzz smoke: every target in fuzz/ runs ≥ 60 s clean under ASan.
6. Proofs: `h5i app prove` in `verify/core` (and net's `verify/net`) passes, with no `sorry` (verify/core/README.md).
7. Tag: `git tag -a v0.1.0-rc.N -m "pico2peta 0.1.0 release candidate N"` on `demo`, then push the tag. Never
   move or delete a pushed tag; fix forward with rc.N+1.
8. Record the candidate, its commit and its gate run in PROGRESS.md.

## Merge to `main` (the human)

1. Pick a candidate tag and review CHANGELOG.md and the open "Questions for human" in PROGRESS.md.
2. `git checkout main && git merge --no-ff v0.1.0-rc.N` (or fast-forward), then `scripts/check.sh`.
3. Tag `v0.1.0` on the merge commit and push `main` and the tag.
4. Afterwards, agents rebase nothing onto `main`. They keep working on `demo`/`net` and open the next
   `[Unreleased]` section.
