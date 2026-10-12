<p align="center">
  <a href="https://github.com/h5i-dev/o5s/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/o5s?color=blue"></a>
</p>

<h1 align="center">Version Control for Many Concurrent Agents</h1>

**o5s** is a patch-based collaboration server for teams of AI agents and people who change the same code at the same time. Every change is an immutable patch with a content hash and explicit dependencies. Changes are integrated without rebasing, and its core is formally verified in Lean 4 against the Rust code that runs.

- **No rebase loop:** Agents propose patches against what they saw. Patches that don't conflict are admitted in any order, and the result is the same. Edits to different lines of one file merge automatically.
- **Conflicts are data:** Concurrent edits to the same lines are kept side by side as alternatives instead of stopping an agent. A resolution is one more patch, reviewed like any other.
- **Review is enforced:** A patch enters a channel only with an approval and a passing check report from someone other than its author, and only after everything it depends on.
- **Verified core:** Every registry decision is made by Rust code extracted to Lean, with proofs of the invariant, history, the admission gate and tenant isolation.
- **Git at the edges:** Agents work in ordinary Git checkouts. Admitted snapshots export to bare Git repositories, or to Cloudflare Artifacts.

```bash
./scripts/dev.sh                              # Start the server and print three tokens
cd my-project && o5s connect demo             # Attach an ordinary Git repository
o5s propose --intent 'Add the parser'         # Propose your working-tree changes
o5s --profile reviewer approve 1              # An independent reviewer approves
o5s --profile checks check 1 --pass           # An independent check report
o5s --profile reviewer merge 1                # Admit it to the main channel
o5s fetch                                     # Fetch the snapshot to refs/o5s/main
```

---

## 1. Quick Start

Requires Rust stable, Git and Python 3. No database or Cloudflare account is needed.

```bash
./scripts/dev.sh                 # Local write-ahead log in .o5s/
./scripts/dev.sh --postgres      # Or PostgreSQL 17 in Docker
```

Open **http://127.0.0.1:3000** and paste one of the printed tokens: author, reviewer or check reporter. The web UI shows repositories, patches, channels, conflicts and the activity log.

To see a full merge cycle, run the demo while the server is up:

```bash
python3 scripts/demo.py
```

It creates two conflicting changes and admits them in opposite orders on two channels, which show the same conflict. It then resolves the conflict with a reviewed patch, checks that both channels converge, and exports a local bare Git repository you can clone.

Install the agent CLI with `cargo install --path crates/cli --locked`, then save each printed token under its role. Each command reads its token from stdin:

```bash
o5s login                        # author
o5s --profile reviewer login     # reviewer
o5s --profile checks login       # check reporter
```

See the [CLI manual](docs/cli.md) for line and graph edits, conflict resolution and history.

---

## 2. How It Works

| Concept | Meaning |
|---|---|
| **Patch** | An immutable change: author, intent, the ids of its dependencies, and its edits. Its id is the SHA-256 of that content, so the same change always has the same id. |
| **Channel** | A named set of admitted patches that always includes every dependency of its members. `main` is created with the repository; `fork` copies a channel. |
| **Admission** | A patch joins a channel when it has an independent approval and a passing check report, and its dependencies are already members. A batch is admitted all or nothing. |
| **View** | The files a channel's patches produce. Concurrent edits of the same text stay visible as conflict alternatives until a resolution patch is admitted. |
| **Documents** | Text can be edited by line ranges on an earlier version, or as a graph of stable text atoms that survive concurrent insertions and deletions. Binary and executable files are supported; symlinks and submodules are refused. |

Agents keep using Git for their working copies. `o5s propose` reads staged, unstaged and untracked changes without touching the index; `o5s fetch` writes the admitted snapshot to `refs/o5s/main` without moving `HEAD`.

---

## 3. Compared with Git

The same edits by the same number of agents, on one host, with final contents checked on both sides. Git agents `push`, and on rejection `pull --rebase` and push again. o5s agents propose, are independently reviewed and checked, and are admitted in batches. The table shows 1,000 agents with at most 64 in flight.

| Workload | Git: integrated | Git: integrations/s | o5s: integrated | o5s: integrations/s |
|---|---|---|---|---|
| Each agent edits a different line of one file | 431 of 1,000 (569 stopped by rebase conflicts) | 9.8 | 1,000 | 574 |
| Each agent adds its own file | 1,000 | 9.0 | 1,000 | 1,083 |
| Each agent has its own repository | 1,000 | 691 | 1,000 | 896 |

The o5s figures use its HTTP API with the local storage backend. Through its CLI, which starts a process per agent, they are about 4× lower. Method, all workloads and limits are in the [comparison with Git](docs/git-comparison.md).

---

## 4. What Is Proved

The registry kernel ([`crates/registry`](crates/registry)) makes every committed decision: repository directory, patch registration, reviews and check reports, forks and admission. It is written in the Rust subset that [Aeneas](https://github.com/AeneasVerif/aeneas) translates to Lean 4. The proofs in [`crates/registry/proofs`](crates/registry/proofs) are about that translation, and `h5i app check` rejects any `sorry` or added axiom.

| Guarantee | Theorem |
|---|---|
| The extracted `transition` computes a pure model of every command | `Refine.transition_refines` |
| Every reachable state keeps the invariant: dependency-closed channels, independent reviews and checks, unique and fresh ids | `Invariant.inv_preserved`, `reachable_inv` |
| Registered patches never change; channel membership only grows | `patches_immutable`, `members_append_only` |
| Only approved patches with a passing check report are admitted | `Guarantees.merge_gate` |
| A repository is visible only to its own organization | `isolation`, `scopes_separate` |
| PostgreSQL's partial, per-command loads compute what the whole repository specifies | `frame`, `Narrow.narrow_load` |
| The store holds what committing computes | `Storage.stored` |

The document kernels in [`proofs/`](proofs) prove admission, graph traversal, interval conflicts, checked rendering, frontiers and deletion tags. Not yet proved: the document graph adapter, conflict semantics, convergence and line merging. The [verification plan](docs/verification-plan.md) covers how to prove them. Hashing, authentication, edit preparation and Git export are tested native code.

---

## 5. Storage and Deployment

| Backend | How it stores | Use |
|---|---|---|
| Local log (default) | A checksummed write-ahead log of committed kernel writes in `O5S_DATA`; `o5s-server verify`, `backup` and `import-json` | One server, development, tests |
| PostgreSQL | One transaction per command under a per-repository lock, loading only the rows the command reads; idempotency keys and retries | Several server replicas sharing one database |
| Cloudflare Workers | The same kernel and PostgreSQL service through Hyperdrive, with Git export to Cloudflare Artifacts | [`crates/worker`](crates/worker/README.md) |

Set `O5S_DATABASE_URL` to use PostgreSQL; `O5S_DATA`, `O5S_BIND` and `H5I_APP_SECRET` set the data directory, listen address and token signing key. The server serves `/healthz`, `/readyz`, `/metrics` and request IDs. [Storage and transactions](docs/storage.md) covers backups, secret rotation and the Git export guard. [Load testing](docs/load-testing.md) has throughput figures for both backends.

---

## 6. Verify

```bash
./scripts/verify.sh              # Formatting, all tests, UI and Worker checks, both Lean projects
h5i app doctor .                 # Check the pinned Charon, Aeneas and Lean toolchain
h5i app mutate . --only merge_   # Inject bugs that the proofs must reject
```

Set `O5S_TEST_DATABASE_URL` (for example from `scripts/postgres.sh`) to include the PostgreSQL tests.

---

## 7. Documentation

- [CLI manual](docs/cli.md): connecting Git repositories, proposing, reviewing, resolving and fetching
- [Architecture](docs/architecture.md) and [patch model](docs/patch-model.md): channels, conflicts and document operations
- [Storage and transactions](docs/storage.md), [load testing](docs/load-testing.md) and [scaling](docs/scaling.md)
- [Proof scope](docs/proofs.md) and [verification plan](docs/verification-plan.md)
- [Status](docs/status.md) and [roadmap](docs/roadmap.md)
- [Cloudflare Worker](crates/worker/README.md), with Git projections and agent workspaces in Artifacts

---

## 8. License

o5s is licensed under the Apache License 2.0. See [LICENSE](LICENSE). It draws on ideas from [Pijul](https://pijul.org/) but contains no Pijul code.
