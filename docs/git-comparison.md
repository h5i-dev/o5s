# o5s compared with plain Git

This compares how many concurrent agents get their changes integrated into one shared history, how long that takes, and what is left for them to resolve. The comparison uses the same machine, workloads, bytes and concurrency for both systems. Both sides' final contents are checked in every run.

## Method

`o5s-load compare` (`crates/loadgen`) runs both systems on one workload. `scripts/compare_git.sh OUT AGENTS CONCURRENCY` runs the full matrix, each workload on a fresh server, and writes one JSON report per run.

**Git baseline.** Each agent has its own clone of one bare repository, which holds a seed commit: the shared base file, or a README. Clones are made before timing starts. Each agent then edits, commits, and integrates the usual way: `git push`. When the push is rejected, it runs `git pull --rebase` and pushes again. A rebase conflict stops that agent with its change unintegrated, which is what happens to an unattended agent; someone must then resolve it. The transport is `file://`, with no server, authentication or network, so the setup favours Git. Timing covers each agent's edit, commit and integration loop.

**o5s.** Agents use the authenticated HTTP API of one server process. Each author proposes, an independent reviewer approves, an independent check reporter reports, and patches are admitted in batches of 100. This pipeline is stricter than Git's: nothing is admitted without review and a check report. Rows marked *CLI* make proposals with the real `o5s propose` command, run as a separate process in each agent's own Git repository. That pays process start-up and Git worktree reads, like the Git baseline.

**Workloads.** Both systems apply the same edit for agent *i*.

| Workload | Edit by agent *i* |
|---|---|
| Adjacent lines | Replace line *i* of one shared file |
| Lines 4 apart | Replace line 4*i* of one shared file, so Git's three-way merge has unchanged context between edits |
| One file each | Add a new file `agent-i.txt` to one repository |
| Same line | Every agent replaces line 0 of one shared file |
| One repository each | Add a file to that agent's own repository; there is no shared history |

"Integrated" counts changes in the shared history. For **same line**, o5s admits every proposal but keeps all of them as explicit conflict alternatives. That is not a resolved file, and a resolution patch is still needed. Git integrates one edit and stops every other agent with a rebase conflict.

## Results

All runs were on October 11, 2026, on a 128-thread AMD EPYC 7502 with 251 GiB of memory, Linux 6.8.0-137-generic and Git 2.43.0. Client and server shared the host. The o5s server used the local write-ahead log on local disk, with release builds of the working tree. Reports are in [evidence/git-comparison](evidence/git-comparison/).

### 1,000 agents, at most 64 in flight

| Workload | Git: integrated / stopped by conflict | Git: integrated/s, p95 per agent, push retries | o5s API: integrated/s (proposal p95) | o5s CLI: integrated/s (proposal p95) |
|---|---|---|---|---|
| Adjacent lines | 431 / 569 | 9.8/s, 11.4 s, 24,888 | 574/s (98 ms) | 145/s (780 ms) |
| Lines 4 apart | 1,000 / 0 | 5.0/s, 65.0 s, 55,773 | 244/s (268 ms) | 48/s (2.5 s) |
| One file each | 1,000 / 0 | 9.0/s, 20.6 s, 59,201 | 1,083/s (47 ms) | 245/s (309 ms) |
| Same line | 1 / 999 | – | 595/s* (95 ms) | 146/s* (779 ms) |
| One repository each | 1,000 / 0 | 691/s, 97 ms, 0 | 896/s (41 ms) | 278/s (136 ms) |

### 200 agents, at most 32 in flight

| Workload | Git: integrated / stopped by conflict | Git: integrated/s, push retries | o5s API: integrated/s | o5s CLI: integrated/s |
|---|---|---|---|---|
| Adjacent lines | 87 / 113 | 12.0/s, 2,366 | 896/s | 277/s |
| Lines 4 apart | 200 / 0 | 11.8/s, 5,843 | 630/s | 275/s |
| One file each | 200 / 0 | 11.7/s, 5,800 | 1,568/s | 413/s |
| Same line | 1 / 199 | – | 1,122/s* | 293/s* |
| One repository each | 200 / 0 | 540/s, 0 | 1,116/s | 396/s |

\* Admitted as retained conflict alternatives; one resolution patch is still required.

o5s rows include proposal, review, check report and batch admission. Every o5s run admitted every proposal with the expected bytes or retained alternatives, and had no failed requests.

## Interpretation

- **Shared history is the difference.** Git integrates one change per successful fast-forward. With many agents, most pushes are rejected and retried, so throughput stays around 5–12 changes per second regardless of agent count, and retries grow roughly with the square of it. o5s admits independently reviewed patches without rebasing them onto each other; a channel is a dependency-closed set.
- **Fine-grained concurrency.** Git's three-way merge treats edits on adjacent lines as a conflict, and 57% of agents were stopped. o5s line proposals on a shared immutable base merge as disjoint ranges.
- **Same line.** Neither system decides a winner. Git stops 999 agents, each with a local conflict. o5s keeps all 1,000 alternatives in one place, where a single reviewed resolution patch can settle them.
- **Without contention the two are close.** With one repository per agent, Git manages 540–690 integrations/s. o5s manages 900–1,100 via its API, and 280–400 via its CLI, whose cost is process start-up and worktree reads.
- **Line edits on large files cost o5s more.** For lines 4 apart, each proposal renders a 4,000-line preview, so o5s is slower than on adjacent lines.

## Limits of this comparison

- One host, closed-loop clients. These are not open-loop overload measurements.
- Git ran without a server, authentication or hooks, and o5s ran one server process. A hosted Git service adds its own latency; a CI gate adds more.
- The PostgreSQL backend is slower per repository: each transaction holds the repository's advisory lock for several database roundtrips. Its numbers are in [storage](storage.md) and [load testing](load-testing.md).
- Conflicts left to Git agents are counted, not resolved. Resolving them would add more rebase rounds and contention.
