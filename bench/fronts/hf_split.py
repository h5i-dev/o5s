"""H4 step 1: how much of an HF upload/download is the server's time?

Runs one Xet (and one LFS) upload and download of the bench model against a pico2peta server
logging every response with its latency, then reports, per operation, the client's wall time and
the union of the server's request intervals (each `[end - latency, end]`, so overlapping parallel
requests count once), split by route. The rest is the client's own work and the network.

usage: hf_split.py <pico2peta-server binary>      (run under the bench lock)
"""
import datetime, json, os, sys, tempfile
from common import SCRATCH, busy, log_lines, pico_server, run, server_cpu, server_intervals
from hf_bench import BASE, REPO_TOML, client

ROUTES = (("xorb_post", "/v1/xorbs/"), ("shard_post", "/v1/shards"), ("xorb_get", "/xorb/"),
          ("reconstruction", "/v1/reconstructions/"), ("lfs", "/lfs/"), ("preupload", "/preupload/"),
          ("commit", "/commit/"))


def route(path):
    for name, frag in ROUTES:
        if frag in path:
            return name
    return "other"


def intervals(log_path, start_line=0):
    return [(a, b, route(p) if route(p) != "other" else "other:" + "/".join(p.split("/")[:4]))
            for a, b, p in server_intervals(log_path, start_line)]


def union(iv):
    return busy(iv)


def main():
    server_bin = sys.argv[1]
    d = tempfile.mkdtemp(dir=SCRATCH, prefix="hf-split-")
    try:
        for mode, xet in (("xet", True), ("lfs", False)):
            p2p = os.path.join(d, f"p2p-{mode}")
            os.makedirs(p2p)
            with pico_server(p2p, server_bin, repo_toml=REPO_TOML, log_format="json", log_filter="info") as (base, _):
                runs = [(f"{op}{i}", args) for i in range(2) for op, args in (
                    ("upload", ("upload", f"bench/split{i}", BASE, "model.safetensors")),
                    ("download", ("download", f"bench/split{i}", "model.safetensors", os.path.join(d, f"cache-{mode}-{i}"))))]
                for op, args in runs:
                    log = os.path.join(p2p, "server.log")
                    seen, c0 = log_lines(log), server_cpu(base)
                    r = client(base, d, xet, *args)
                    scpu = server_cpu(base) - c0
                    iv = intervals(log, seen)
                    by = {}
                    for s, e, rt in iv:
                        by.setdefault(rt, []).append((s, e))
                    print(json.dumps({"mode": mode, "op": op, "client_s": round(r["s"], 3), "client_cpu_s": round(r.get("cpu_s", 0), 3),
                                      "server_busy_s": round(union([(s, e) for s, e, _ in iv]), 3), "server_cpu_s": round(scpu, 3),
                                      "requests": len(iv),
                                      "by_route": {k: {"n": len(v), "busy_s": round(union(v), 3)} for k, v in sorted(by.items())}}),
                          flush=True)
                    run(["rm", "-rf", os.path.join(d, "hfhome")])
    finally:
        run(["rm", "-rf", d])


if __name__ == "__main__":
    main()
