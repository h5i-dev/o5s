"""Times one huggingface_hub operation against HF_ENDPOINT; prints JSON.

usage: hf_client.py upload <repo> <file> <path_in_repo> | download <repo> <path_in_repo> <cache_dir>
"""
import hashlib, json, os, sys, time
from huggingface_hub import HfApi, hf_hub_download

op = sys.argv[1]
api = HfApi()
if op == "upload":
    repo, f, path = sys.argv[2:5]
    api.create_repo(repo, exist_ok=True)
    t = time.perf_counter()
    api.upload_file(path_or_fileobj=f, path_in_repo=path, repo_id=repo)
    print(json.dumps({"s": time.perf_counter() - t}))
elif op == "download":
    repo, path, cache = sys.argv[2:5]
    t = time.perf_counter()
    p = hf_hub_download(repo, path, cache_dir=cache)
    s = time.perf_counter() - t
    h = hashlib.sha256(open(p, "rb").read()).hexdigest()
    print(json.dumps({"s": s, "sha256": h}))
