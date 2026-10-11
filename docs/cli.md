# Git and agent CLI

Install the local CLI with `cargo install --path crates/cli --locked`. Start the server with `./scripts/dev.sh`. The CLI defaults to `http://127.0.0.1:3000`; `--url` or `O5S_URL` selects another origin.

Run `o5s login` and provide the printed development token on stdin. Saved profiles bind their token to the authenticated origin. `--profile author`, `--profile reviewer` and `--profile checks` keep roles separate. `O5S_TOKEN` supplies a token without saving it. Tokens are never CLI arguments or output fields.

## Existing Git repositories

Use ordinary `git init`, `git clone` and `git worktree add`. Inside that Git repository, `o5s connect demo` attaches collaboration metadata to its common Git directory. Native worktrees share that connection; their checked-out files remain independent. Connecting creates the server-side collaboration record if it does not exist.

```sh
git init project
cd project
o5s connect demo
printf 'first line\nsecond line\n' > message.txt
o5s propose --intent 'Add the initial message'
o5s show 1
```

The proposal captures the current files, including staged, unstaged and untracked changes. It does not stage or commit them. After a conflict-free snapshot has been fetched, subsequent proposals infer disjoint line-range changes against the fetched immutable base where possible. Whole-file replacements remain whole-file proposals.

An author cannot approve their own patch or report its checks. With separately authenticated profiles:

```sh
o5s --profile reviewer approve 1
o5s --profile checks check 1 --pass
o5s --profile reviewer merge 1
o5s fetch
```

`check` reports a result; it does not run a test command. `merge 2 3 4` admits a batch atomically when reviews, passing checks and prerequisites are present. A missing prerequisite or refused patch leaves the channel unchanged.

`fetch` writes the conflict-free snapshot to `refs/o5s/main`. It preserves HEAD, the index and working files. Inspect it with `git show refs/o5s/main:message.txt` or `git diff HEAD refs/o5s/main`; use normal Git commands when you choose to update your working tree. Conflicts refuse the ref update.

## Conflict and graph operations

`o5s status`, `o5s view` and `o5s log` expose local changes and remote proposals. Whole-file/line conflicts can be resolved with `o5s resolve PATH --content-file FILE --intent 'Explain this resolution'`, or `--delete`. The resolution is another proposal and needs independent review and checks.

Graph documents use stable node identities rather than byte offsets. Inspect their admitted nodes with `o5s document PATH`. `o5s propose-graph --operations-file edits.json --intent 'Explain the change' --dependencies 1,2` reads a JSON edits array:

```json
[
  {
    "path": "graph.txt",
    "operations": [
      {"kind": "insert", "left": "Start", "right": "End", "atoms": [[104, 105, 10]]}
    ]
  }
]
```

Later insertions, deletions, restorations and ordering operations name the exact identities returned by `document`. Concurrent ambiguous orders and delete/insert disagreements remain explicit conflicts. The server refuses whole-file/line changes to graph documents and graph changes to legacy documents until a migration operation is specified. Ordinary Git diffs do not yet generate graph operations automatically. See [document operations](patch-model.md) for the wire types and examples.

## Supported file subset

The CLI accepts regular UTF-8 files, each at most 4 MiB, with a total request limit of 4 MiB. Deletion is distinct from an empty file. Executable modes, symbolic links, binary contents and submodules are refused before submitting a proposal. These file kinds need explicit patch operations; silently discarding their meaning would corrupt a repository.

The client locks connection metadata across commands and writes fetched refs with compare-and-swap. Saved credentials use an atomic file replacement with mode `0600` on Unix. A connected repository cannot be silently rebound to a different tenant or collaboration repository.

The integration tests launch the actual CLI against a live Axum listener and real Git repositories. They cover native worktrees, concurrent line changes, graph conflicts and reviewed order resolution, ref/HEAD/index preservation, unsupported content and origin/tenant separation. Run `cargo test -p o5s-cli`.
