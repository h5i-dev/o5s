"""Drives the HF front with the real, unmodified huggingface_hub client.

Run by tests/hf.rs with HF_ENDPOINT/HF_TOKEN pointing at a test server. Exits non-zero on
any mismatch.
"""
import atexit
import hashlib
import os
import shutil
import sys
import tempfile

from huggingface_hub import HfApi, hf_hub_download, snapshot_download

api = HfApi()
repo = os.environ.get("HF_TEST_REPO", "alice/tiny-model")
work = tempfile.mkdtemp()
# Always remove the download caches (~9 MB per run), also on assertion failure.
atexit.register(shutil.rmtree, work, ignore_errors=True)
rng = __import__("random").Random(7)

small = b'{"architectures": ["Tiny"], "hidden_size": 8}\n'
big = bytes(rng.getrandbits(8) for _ in range(3 * 1024 * 1024 + 17))  # LFS
binary_small = b"\x00\x01\x02" * 100  # NUL in sample -> LFS

if os.environ.get("HF_TEST_PHASE") == "verify":
    # After GC: everything the write phase left at HEAD must still download.
    snap = snapshot_download(repo, cache_dir=os.path.join(work, "cache-v"))
    assert open(os.path.join(snap, "model.safetensors"), "rb").read() == big
    assert open(os.path.join(snap, "copy.safetensors"), "rb").read() == big
    assert open(os.path.join(snap, "tok.bin"), "rb").read() == binary_small
    print("OK")
    sys.exit(0)

url = api.create_repo(repo, exist_ok=True)
print("created", url)
api.upload_file(path_or_fileobj=small, path_in_repo="config.json", repo_id=repo, commit_message="add config")
api.upload_file(path_or_fileobj=big, path_in_repo="model.safetensors", repo_id=repo)
d = os.path.join(work, "folder")
os.makedirs(os.path.join(d, "sub"))
open(os.path.join(d, "sub", "a.txt"), "wb").write(b"hello\n")
open(os.path.join(d, "tok.bin"), "wb").write(binary_small)
api.upload_folder(folder_path=d, repo_id=repo, commit_message="folder")

files = sorted(api.list_repo_files(repo))
print("files", files)
want = sorted([".gitattributes", "config.json", "model.safetensors", "sub/a.txt", "tok.bin"])
assert files == want, (files, want)

info = api.model_info(repo, files_metadata=True)
sizes = {s.rfilename: s.size for s in info.siblings}
assert sizes["model.safetensors"] == len(big), sizes
lfs = {s.rfilename: s.lfs for s in info.siblings}
assert lfs["model.safetensors"] is not None and lfs["model.safetensors"].sha256 == hashlib.sha256(big).hexdigest()
assert lfs["config.json"] is None

cache = os.path.join(work, "cache")
p = hf_hub_download(repo, "model.safetensors", cache_dir=cache)
assert open(p, "rb").read() == big, "big file mismatch"
p = hf_hub_download(repo, "config.json", cache_dir=cache)
assert open(p, "rb").read() == small
snap = snapshot_download(repo, cache_dir=os.path.join(work, "cache2"))
assert open(os.path.join(snap, "sub", "a.txt"), "rb").read() == b"hello\n"
assert open(os.path.join(snap, "tok.bin"), "rb").read() == binary_small
assert open(os.path.join(snap, "model.safetensors"), "rb").read() == big

# Old revision still resolvable by commit id.
first = info.sha
api.delete_file("config.json", repo_id=repo)
assert "config.json" not in api.list_repo_files(repo)
p = hf_hub_download(repo, "config.json", revision=first, cache_dir=os.path.join(work, "cache3"))
assert open(p, "rb").read() == small

# Re-uploading identical LFS content must not transfer it again (the batch API says we have it).
api.upload_file(path_or_fileobj=big, path_in_repo="copy.safetensors", repo_id=repo)
p = hf_hub_download(repo, "copy.safetensors", cache_dir=os.path.join(work, "cache4"))
assert open(p, "rb").read() == big
print("OK")
