"""Times one huggingface_hub operation against HF_ENDPOINT; prints JSON.

usage: hf_client.py upload <repo> <file> <path_in_repo> | download <repo> <path_in_repo> <cache_dir>
"""
import hashlib, json, os, resource, sys, time
from huggingface_hub import HfApi, hf_hub_download

op = sys.argv[1]
api = HfApi()
if op == "upload":
    repo, f, path = sys.argv[2:5]
    api.create_repo(repo, exist_ok=True)
    t, c = time.perf_counter(), resource.getrusage(resource.RUSAGE_SELF)
    api.upload_file(path_or_fileobj=f, path_in_repo=path, repo_id=repo)
    s, c2 = time.perf_counter() - t, resource.getrusage(resource.RUSAGE_SELF)
    # The client's own CPU (all threads, hf_xet's included): work no server can remove.
    print(json.dumps({"s": s, "cpu_s": (c2.ru_utime - c.ru_utime) + (c2.ru_stime - c.ru_stime)}))
elif op == "download":
    repo, path, cache = sys.argv[2:5]
    t, c = time.perf_counter(), resource.getrusage(resource.RUSAGE_SELF)
    p = hf_hub_download(repo, path, cache_dir=cache)
    s, c2 = time.perf_counter() - t, resource.getrusage(resource.RUSAGE_SELF)
    cpu = (c2.ru_utime - c.ru_utime) + (c2.ru_stime - c.ru_stime)
    h = hashlib.sha256(open(p, "rb").read()).hexdigest()
    print(json.dumps({"s": s, "cpu_s": cpu, "sha256": h}))
