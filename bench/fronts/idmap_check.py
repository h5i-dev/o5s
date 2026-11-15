"""Dev check (not a §6.5 batch result): git front re-push and clone, cold and warm, vs git daemon.

usage: idmap_check.py <label> <pico2peta-server binary> [corpus]

- push: a first push of heads + tags into a fresh server (fills the oid map);
- re-push: the same refs into a new repo name (every object already stored: pure lookups);
- clone: a v2 bare clone of the pushed repo (first = encodes the pack; cold/warm = later
  clones, served from the clone pack cache when the server has one);
- fetch10: a client 10 commits behind on the default branch fetches.
COLD = fresh server process (empty segment/page caches) after dropping the OS page cache;
WARM = the same operation again on the same process. git daemon gets the same treatment.
Rows go to $FRONTS_RESULTS (front "idmap-check").
"""
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(__file__))
from common import TOKEN, Proc, free_port, log, pico_server, record, run, timed, wait_port, workdir, TOOLS  # noqa: E402
from git_bench import REFSPECS, prepare  # noqa: E402


def drop_caches():
    subprocess.run(["sync"], check=True)
    subprocess.run(["sudo", "-n", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"], check=True)


def main():
    label, server_bin = sys.argv[1], sys.argv[2]
    name = sys.argv[3] if len(sys.argv) > 3 else "redis"
    rec = lambda metric, system, v, **kw: record("idmap-check", metric, system, v, "s", corpus=name, label=label, **kw)
    with workdir(f"idmap-check-{label}") as d:
        src, head, behind = prepare(d, name)

        def fetch(url, c):
            run(["cp", "-a", behind, c])
            t = timed(["git", "-C", c, "fetch", "-q", url, f"+refs/heads/{head}:refs/heads/{head}"])
            run(["rm", "-rf", c])
            return t

        def wait_cache(base):
            req = urllib.request.Request(base + "/v1/refs/git/_clonepack/bench/a", headers={"Authorization": f"Bearer {TOKEN}"})
            for _ in range(120):
                try:
                    urllib.request.urlopen(req, timeout=5)
                    return
                except urllib.error.HTTPError:
                    time.sleep(0.5)
            log("clone pack never cached")
        push = lambda url: timed(["git", "-C", src, "push", "-q", url, *REFSPECS])
        clone = lambda url, out: (timed(["git", "-c", "protocol.version=2", "clone", "-q", "--bare", url, out]), run(["rm", "-rf", out]))[0]
        # pico2peta: first push, then restart for each cold measurement.
        sys_name = f"pico2peta-{label}"
        with pico_server(d, server_bin) as (base, _):
            url = lambda r: base.replace("http://", f"http://u:{TOKEN}@") + f"/git/bench/{r}.git"
            rec("push_s", sys_name, push(url("a")))
            # Derived data (commit graph, clone pack) is refreshed in the background after a push.
            t0 = time.perf_counter()
            wait_cache(base)
            rec("derived_after_push_s", sys_name, time.perf_counter() - t0)
        with pico_server(d, server_bin) as (base, _):
            url = lambda r: base.replace("http://", f"http://u:{TOKEN}@") + f"/git/bench/{r}.git"
            drop_caches()
            rec("repush_cold_s", sys_name, push(url("b")))
            rec("repush_warm_s", sys_name, push(url("c")))
        with pico_server(d, server_bin) as (base, _):
            url = lambda r: base.replace("http://", f"http://u:{TOKEN}@") + f"/git/bench/{r}.git"
            drop_caches()
            # The first clone of a refs state encodes the pack (and fills the clone pack cache).
            rec("clone_first_s", sys_name, clone(url("a"), os.path.join(d, "c0")))
            wait_cache(base)
        with pico_server(d, server_bin) as (base, _):
            url = lambda r: base.replace("http://", f"http://u:{TOKEN}@") + f"/git/bench/{r}.git"
            drop_caches()
            rec("clone_cold_s", sys_name, clone(url("a"), os.path.join(d, "c1")))
            rec("clone_warm_s", sys_name, clone(url("a"), os.path.join(d, "c2")))
        with pico_server(d, server_bin) as (base, _):
            url = lambda r: base.replace("http://", f"http://u:{TOKEN}@") + f"/git/bench/{r}.git"
            drop_caches()
            rec("fetch10_cold_s", sys_name, fetch(url("a"), os.path.join(d, "f1")))
            rec("fetch10_warm_s", sys_name, fetch(url("a"), os.path.join(d, "f2")))
        # git daemon.
        root = os.path.join(d, "daemon")
        os.makedirs(root)
        for r in ("a", "b", "c"):
            run(["git", "init", "-q", "--bare", os.path.join(root, f"{r}.git")])
        port = free_port()
        daemon = Proc([f"{TOOLS}/bin/git-daemon", "--reuseaddr", f"--base-path={root}", "--export-all", "--enable=receive-pack",
                       f"--port={port}", "--listen=127.0.0.1", root], log_path=os.path.join(d, "daemon.log"))
        try:
            wait_port(port)
            g = lambda r: f"git://127.0.0.1:{port}/{r}.git"
            rec("push_s", "git-daemon", push(g("a")))
            drop_caches()
            rec("repush_cold_s", "git-daemon", push(g("b")))
            rec("repush_warm_s", "git-daemon", push(g("c")))
            drop_caches()
            rec("clone_cold_s", "git-daemon", clone(g("a"), os.path.join(d, "c1")))
            rec("clone_warm_s", "git-daemon", clone(g("a"), os.path.join(d, "c2")))
            drop_caches()
            rec("fetch10_cold_s", "git-daemon", fetch(g("a"), os.path.join(d, "f1")))
            rec("fetch10_warm_s", "git-daemon", fetch(g("a"), os.path.join(d, "f2")))
        finally:
            daemon.stop()
    log("done", label)


if __name__ == "__main__":
    main()
