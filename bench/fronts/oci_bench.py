"""PLAN §6.5, OCI: o5s's /v2 front vs CNCF Distribution (registry v3) on local disk.

Image set: 3 related tags of one image plus 1 unrelated image (linux/amd64), fetched once
from Docker Hub into OCI layouts with crane. Per system and run (fresh, empty registry):
push all, pull all; then stored bytes (cross-image dedup) and digest equality (functional).

usage: oci_bench.py <o5s-server binary>
"""
import base64
import json
import os
import sys

from common import Proc, RUNS, TOKEN, TOOLS, check, du, free_port, log, o5s_server, record, run, timed, wait_port, workdir

CRANE = f"{TOOLS}/bin/crane"
REGISTRY = f"{TOOLS}/bin/registry"
IMAGES = [
    ("python", "3.11-slim"),
    ("python", "3.12-slim"),
    ("python", "3.13-slim"),
    ("alpine", "3.20"),
]


def crane(*a, env=None):
    return run([CRANE, *a], env=env)


def fetch_images(d):
    out = []
    for img, tag in IMAGES:
        p = os.path.join(d, "src", f"{img}-{tag}")
        crane("pull", "--platform", "linux/amd64", "--format", "oci", f"docker.io/library/{img}:{tag}", p)
        out.append((img, tag, p))
    return out


def bench_system(system, host, env, d, images, stored_path):
    for i in range(RUNS):
        reg = f"{host}/run{i}"
        t = 0.0
        for img, tag, p in images:
            t += timed([CRANE, "push", "--insecure", p, f"{reg}/{img}:{tag}"], env=env)
        record("oci", "push_s", system, t, "s", run=i, images=len(images))
        if i == 0:
            # Stored bytes for one copy of the image set (cross-image dedup inside it).
            record("oci", "stored_bytes", system, du(stored_path), "B")
        t = 0.0
        for img, tag, _ in images:
            out = os.path.join(d, f"pull-{system}-{i}-{img}-{tag}")
            t += timed([CRANE, "pull", "--insecure", "--format", "oci", f"{reg}/{img}:{tag}", out], env=env)
            run(["rm", "-rf", out])
        record("oci", "pull_s", system, t, "s", run=i, images=len(images))
        for img, tag, p in images:
            want = json.load(open(os.path.join(p, "index.json")))["manifests"][0]["digest"]
            got = crane("digest", "--insecure", f"{reg}/{img}:{tag}", env=env).stdout.strip()
            check("oci", f"digest_{system}_{img}_{tag}_run{i}", got == want, f"{got} vs {want}")


def main():
    server_bin = sys.argv[1]
    with workdir("oci") as d:
        log("oci: fetching images")
        images = fetch_images(d)
        src_bytes = sum(du(p) for _, _, p in images)
        record("oci", "image_set_bytes", "source", src_bytes, "B")
        # Baseline: CNCF Distribution.
        root = os.path.join(d, "registry")
        port = free_port()
        cfg = os.path.join(d, "registry.yml")
        with open(cfg, "w") as f:
            f.write(f"version: 0.1\nlog:\n  level: error\nstorage:\n  filesystem:\n    rootdirectory: {root}\n  delete:\n    enabled: true\nhttp:\n  addr: 127.0.0.1:{port}\n")
        reg = Proc([REGISTRY, "serve", cfg], log_path=os.path.join(d, "registry.log"))
        try:
            wait_port(port)
            bench_system("distribution", f"127.0.0.1:{port}", None, d, images, root)
        finally:
            reg.stop()
        run(["rm", "-rf", root])
        o5s = os.path.join(d, "o5s")
        os.makedirs(o5s)
        with o5s_server(o5s, server_bin) as (base, repo):
            host = base.removeprefix("http://")
            conf = os.path.join(d, "docker")
            os.makedirs(conf)
            auth = base64.b64encode(f"bench:{TOKEN}".encode()).decode()
            json.dump({"auths": {host: {"auth": auth}}}, open(os.path.join(conf, "config.json"), "w"))
            env = dict(os.environ, DOCKER_CONFIG=conf)
            bench_system("o5s", host, env, d, images, repo)


if __name__ == "__main__":
    main()
