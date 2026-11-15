"""PLAN §6.5, HF/Xet: pico2peta's HF front with the real huggingface_hub (default hf_xet
install, and HF_HUB_DISABLE_XET=1) vs the same files as plain objects through /v1.

- upload / download time of SmolLM2-135M (270 MB) vs the /v1 reference (curl PUT / GET);
- storage of the family (SmolLM2-135M + -Instruct) vs Xet chunk dedup of the same files
  (examples/xet_dedup: what HF's Xet storage keeps), with the Xet chunker on `hf/`.

usage: hf_bench.py <pico2peta-server binary> <xet_dedup binary>
"""
import json
import os
import subprocess
import sys

from common import RUNS, SCRATCH, TOKEN, TOOLS, check, du, log, pico_server, record, run, sha256_file, timed, workdir

PY = f"{TOOLS}/venv/bin/python"
CLIENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "hf_client.py")
W = f"{SCRATCH}/bench-data/weights"
BASE, INSTRUCT = f"{W}/SmolLM2-135M.safetensors", f"{W}/SmolLM2-135M-Instruct.safetensors"
REPO_TOML = '[profiles.hf]\nchunker = "xet"\n\n[namespaces]\n"hf/" = "hf"\n'


def client(base, d, xet, *args):
    env = dict(os.environ, HF_ENDPOINT=base, HF_TOKEN=TOKEN, HF_HOME=os.path.join(d, "hfhome"),
               HF_HUB_DISABLE_TELEMETRY="1", HF_HUB_DISABLE_PROGRESS_BARS="1", TMPDIR=os.path.join(d, "tmp"))
    os.makedirs(env["TMPDIR"], exist_ok=True)
    if not xet:
        env["HF_HUB_DISABLE_XET"] = "1"
    p = run([PY, "-I", CLIENT, *args], env=env)
    return json.loads(p.stdout.strip().splitlines()[-1])


def main():
    server_bin, dedup_bin = sys.argv[1], sys.argv[2]
    want = sha256_file(BASE)
    with workdir("hf") as d:
        # Reference: plain objects through /v1.
        p2p = os.path.join(d, "ref")
        os.makedirs(p2p)
        with pico_server(p2p, server_bin) as (base, _):
            for i in range(RUNS):
                t = timed(["curl", "-sf", "-o", os.path.join(d, "put.json"), "-H", f"Authorization: Bearer {TOKEN}", "-T", BASE, f"{base}/v1/objects"])
                h = json.load(open(os.path.join(d, "put.json")))["hash"]
                record("hf", "upload_s", "v1-reference", t, "s", run=i)
                out = os.path.join(d, "get.bin")
                t = timed(["curl", "-sf", "-o", out, "-H", f"Authorization: Bearer {TOKEN}", f"{base}/v1/objects/{h}"])
                record("hf", "download_s", "v1-reference", t, "s", run=i)
                os.remove(out)
        run(["rm", "-rf", p2p])
        for mode, xet in (("xet", True), ("lfs", False)):
            p2p = os.path.join(d, f"p2p-{mode}")
            os.makedirs(p2p)
            with pico_server(p2p, server_bin, repo_toml=REPO_TOML) as (base, repo):
                for i in range(RUNS):
                    r = client(base, d, xet, "upload", f"bench/base-{i}", BASE, "model.safetensors")
                    record("hf", "upload_s", f"pico2peta-{mode}", r["s"], "s", run=i)
                    cache = os.path.join(d, f"cache-{mode}-{i}")
                    r = client(base, d, xet, "download", f"bench/base-{i}", "model.safetensors", cache)
                    record("hf", "download_s", f"pico2peta-{mode}", r["s"], "s", run=i)
                    check("hf", f"download_sha256_{mode}_{i}", r["sha256"] == want)
                    run(["rm", "-rf", cache, os.path.join(d, "hfhome")])
                if mode == "xet":
                    # Storage of the family in a fresh repo-set: base + instruct, one copy each.
                    before = du(repo)
                    client(base, d, xet, "upload", "bench/family-base", BASE, "model.safetensors")
                    client(base, d, xet, "upload", "bench/family-instruct", INSTRUCT, "model.safetensors")
                    record("hf", "family_added_bytes", "pico2peta-xet", du(repo) - before, "B",
                           note="growth of the repo for base+instruct; base content was already stored by the timing runs")
            run(["rm", "-rf", p2p])
        # Storage of the family alone (fresh server, nothing else stored).
        p2p = os.path.join(d, "p2p-family")
        os.makedirs(p2p)
        with pico_server(p2p, server_bin, repo_toml=REPO_TOML) as (base, repo):
            client(base, d, True, "upload", "bench/base", BASE, "model.safetensors")
            client(base, d, True, "upload", "bench/instruct", INSTRUCT, "model.safetensors")
            record("hf", "family_stored_bytes", "pico2peta-xet", du(repo), "B")
        ref = json.loads(subprocess.run([dedup_bin, BASE, INSTRUCT], capture_output=True, text=True, check=True).stdout)
        record("hf", "family_stored_bytes", "xet-dedup", ref["xet_stored_bytes"], "B", **{k: v for k, v in ref.items() if k != "xet_stored_bytes"})
        record("hf", "family_raw_bytes", "source", os.path.getsize(BASE) + os.path.getsize(INSTRUCT), "B")


if __name__ == "__main__":
    main()
