"""H4 step 1: how much of an HF upload/download is the server's time?

Runs one Xet (and one LFS) upload and download of the bench model against a pico2peta server
logging every response with its latency, then reports, per operation, the client's wall time and
the union of the server's request intervals (each `[end - latency, end]`, so overlapping parallel
requests count once), split by route. The rest is the client's own work and the network.

usage: hf_split.py <pico2peta-server binary>      (run under the bench lock)
"""
import datetime, json, os, sys, tempfile
from common import SCRATCH, pico_server, run
from hf_bench import BASE, REPO_TOML, client

ROUTES = (("xorb_post", "/v1/xorbs/"), ("shard_post", "/v1/shards"), ("xorb_get", "/xorb/"),
          ("reconstruction", "/v1/reconstructions/"), ("lfs", "/lfs/"), ("preupload", "/preupload/"),
          ("commit", "/commit/"))


def route(path):
    for name, frag in ROUTES:
        if frag in path:
            return name
    return "other"


def intervals(log_path):
    """(start, end, route) of every finished request, in epoch seconds."""
    out = []
    for line in open(log_path, errors="replace"):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        f = e.get("fields", {})
        if f.get("message") != "finished processing request":
            continue
        lat = str(f.get("latency", ""))
        if not lat.endswith("μs") and not lat.endswith("µs"):
            continue
        us = float(lat.split()[0])
        end = datetime.datetime.fromisoformat(e["timestamp"].replace("Z", "+00:00")).timestamp()
        span = e.get("span", {}) or {}
        out.append((end - us / 1e6, end, route(span.get("path", ""))))
    return out


def union(iv):
    total, cur = 0.0, None
    for s, e in sorted(iv):
        if cur and s <= cur[1]:
            cur[1] = max(cur[1], e)
        else:
            if cur:
                total += cur[1] - cur[0]
            cur = [s, e]
    return total + (cur[1] - cur[0] if cur else 0.0)


def main():
    server_bin = sys.argv[1]
    d = tempfile.mkdtemp(dir=SCRATCH, prefix="hf-split-")
    try:
        for mode, xet in (("xet", True), ("lfs", False)):
            p2p = os.path.join(d, f"p2p-{mode}")
            os.makedirs(p2p)
            with pico_server(p2p, server_bin, repo_toml=REPO_TOML, log_format="json", log_filter="info") as (base, _):
                for op, args in (("upload", ("upload", "bench/split", BASE, "model.safetensors")),
                                 ("download", ("download", "bench/split", "model.safetensors", os.path.join(d, f"cache-{mode}")))):
                    log = os.path.join(p2p, "server.log")
                    seen = len(intervals(log))
                    r = client(base, d, xet, *args)
                    iv = intervals(log)[seen:]
                    by = {}
                    for s, e, rt in iv:
                        by.setdefault(rt, []).append((s, e))
                    print(json.dumps({"mode": mode, "op": op, "client_s": round(r["s"], 3),
                                      "server_busy_s": round(union([(s, e) for s, e, _ in iv]), 3),
                                      "requests": len(iv),
                                      "by_route": {k: {"n": len(v), "busy_s": round(union(v), 3)} for k, v in sorted(by.items())}}),
                          flush=True)
                    run(["rm", "-rf", os.path.join(d, "hfhome")])
    finally:
        run(["rm", "-rf", d])


if __name__ == "__main__":
    main()
