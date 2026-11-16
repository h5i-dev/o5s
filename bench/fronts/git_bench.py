"""PLAN §6.5, Git protocol: o5s's /git front vs `git daemon` serving a bare repo.

Corpus: ripgrep, jq, redis (heads + tags). Per corpus and run:
- full push into an empty repository;
- clone (protocol v2 and v0), each followed by `git fsck --full` (functional);
- incremental fetch: a client 10 commits behind on the default branch;
- clone bytes on the wire, through a counting TCP proxy (separate, untimed clone).

usage: git_bench.py <o5s-server binary>
"""
import os
import sys

from common import CountingProxy, Proc, RUNS, SCRATCH, TOKEN, TOOLS, check, free_port, log, o5s_server, record, run, timed, wait_port, workdir

MIRRORS = f"{SCRATCH}/bench-data/mirrors"
CORPUS = os.environ.get("FRONTS_GIT_CORPUS", "ripgrep,jq,redis").split(",")
REFSPECS = ["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"]


def git(*a, cwd=None, proto=None, **kw):
    cmd = ["git"]
    if proto is not None:
        cmd += ["-c", f"protocol.version={proto}"]
    return run(cmd + list(a), cwd=cwd, **kw)


def prepare(d, name):
    """A bare repo with only heads + tags, and a client 10 commits behind."""
    src = os.path.join(d, f"{name}-src.git")
    git("init", "-q", "--bare", src)
    git("-C", src, "fetch", "-q", os.path.join(MIRRORS, f"{name}.git"), *REFSPECS)
    head = git("-C", os.path.join(MIRRORS, f"{name}.git"), "symbolic-ref", "--short", "HEAD").stdout.strip()
    git("-C", src, "symbolic-ref", "HEAD", f"refs/heads/{head}")
    behind = os.path.join(d, f"{name}-behind.git")
    git("clone", "-q", "--bare", "--no-local", src, behind)
    git("-C", behind, "update-ref", f"refs/heads/{head}", f"refs/heads/{head}~10")
    git("-C", behind, "reflog", "expire", "--expire=now", "--all")
    git("-C", behind, "gc", "-q", "--prune=now")
    return src, head, behind


def bench_system(system, url_of, push_target, d, name, src, head, behind, wire_port_of):
    # Push: a fresh, empty repository per run.
    for i in range(RUNS):
        url = push_target(f"{name}-push{i}")
        t = timed(["git", "-C", src, "push", "-q", url, *REFSPECS])
        record("git", "push_s", system, t, "s", corpus=name, run=i)
        if system == "git-daemon":
            # Every daemon push goes into an empty repository: its push is a first push.
            record("git", "first_push_s", system, t, "s", corpus=name, run=i)
    served = url_of(f"{name}-push0")
    for proto in (2, 0):
        for i in range(RUNS):
            out = os.path.join(d, f"{system}-clone-v{proto}-{i}")
            t = timed(["git", "-c", f"protocol.version={proto}", "clone", "-q", "--bare", served, out])
            record("git", f"clone_v{proto}_s", system, t, "s", corpus=name, run=i)
            ok = True
            try:
                git("-C", out, "fsck", "--full", "--no-progress")
            except RuntimeError as e:
                ok = False
                log(e)
            check("git", f"fsck_{system}_{name}_v{proto}", ok)
            # The clone has every ref of the source.
            refs = lambda r: sorted(git("-C", r, "for-each-ref", "--format=%(refname) %(objectname)").stdout.split("\n"))
            check("git", f"refs_{system}_{name}_v{proto}", refs(out) == refs(src))
            run(["rm", "-rf", out])
    for i in range(RUNS):
        c = os.path.join(d, f"{system}-fetch-{i}")
        run(["cp", "-a", behind, c])
        t = timed(["git", "-C", c, "fetch", "-q", served, f"+refs/heads/{head}:refs/heads/{head}"])
        record("git", "fetch10_s", system, t, "s", corpus=name, run=i)
        check("git", f"fetch_{system}_{name}", git("-C", c, "rev-parse", head).stdout == git("-C", src, "rev-parse", head).stdout)
        run(["rm", "-rf", c])
    # Bytes on the wire (v2 clone through the counting proxy).
    port, rewrite = wire_port_of()
    proxy = CountingProxy(port)
    out = os.path.join(d, f"{system}-wire")
    git("clone", "-q", "--bare", rewrite(served, proxy.port), out)
    record("git", "clone_wire_bytes", system, proxy.reset(), "B", corpus=name)
    proxy.close()
    run(["rm", "-rf", out])


def main():
    server_bin = sys.argv[1]
    for name in CORPUS:
        with workdir(f"git-{name}") as d:
            log("git: preparing", name)
            src, head, behind = prepare(d, name)
            # Baseline: git daemon (git:// protocol) with receive-pack enabled.
            root = os.path.join(d, "daemon")
            os.makedirs(root)
            for i in range(RUNS):
                git("init", "-q", "--bare", os.path.join(root, f"{name}-push{i}.git"))
            dport = free_port()
            daemon = Proc(
                [f"{TOOLS}/bin/git-daemon", "--reuseaddr", f"--base-path={root}", "--export-all", "--enable=receive-pack",
                 "--listen=127.0.0.1", f"--port={dport}", root],
                log_path=os.path.join(d, "daemon.log"),
            )
            try:
                wait_port(dport)
                bench_system(
                    "git-daemon",
                    lambda r: f"git://127.0.0.1:{dport}/{r}.git",
                    lambda r: f"git://127.0.0.1:{dport}/{r}.git",
                    d, name, src, head, behind,
                    lambda: (dport, lambda u, p: u.replace(f":{dport}/", f":{p}/")),
                )
            finally:
                daemon.stop()
            run(["rm", "-rf", root])
            o5s = os.path.join(d, "o5s")
            os.makedirs(o5s)
            with o5s_server(o5s, server_bin) as (base, _):
                port = int(base.rsplit(":", 1)[1])
                url = lambda r: f"http://bench:{TOKEN}@127.0.0.1:{port}/git/bench/{r}.git"
                bench_system(
                    "o5s",
                    url,
                    url,
                    d, name, src, head, behind,
                    lambda: (port, lambda u, p: u.replace(f":{port}/", f":{p}/")),
                )
            # push_s runs 2..n go into the same store (content already there). A first push into
            # an empty store needs a fresh server per run (reported untiered; see NET_PROGRESS).
            for i in range(RUNS):
                fresh = os.path.join(d, f"o5s-fresh{i}")
                os.makedirs(fresh)
                with o5s_server(fresh, server_bin) as (base, _):
                    port = int(base.rsplit(":", 1)[1])
                    t = timed(["git", "-C", src, "push", "-q", f"http://bench:{TOKEN}@127.0.0.1:{port}/git/bench/{name}.git", *REFSPECS])
                    record("git", "first_push_s", "o5s", t, "s", corpus=name, run=i)
                run(["rm", "-rf", fresh])


if __name__ == "__main__":
    main()
