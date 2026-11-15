"""Shared helpers for the PLAN §6.5 front benchmarks (stdlib only, Python 3.9+).

Every front script:
- starts a pico2peta-server (release build) and the specialist baseline, both local;
- drives them with the real, unmodified client;
- appends one JSON line per measurement to the results file;
- removes everything it created (disk budget).

Run them through scripts/fronts-bench.sh, which holds the bench lock.
"""
import contextlib
import hashlib
import json
import os
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
import urllib.request

SCRATCH = "/mnt/scratch/pico2peta"
TOOLS = f"{SCRATCH}/tools"
WORK = os.environ.get("FRONTS_WORK", f"{SCRATCH}/bench-fronts")
RESULTS = os.environ.get("FRONTS_RESULTS", f"{SCRATCH}/bench-fronts-results.jsonl")
RUNS = int(os.environ.get("FRONTS_RUNS", "3"))
TOKEN = "bench-token-0123456789abcdef"


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, file=sys.stderr, flush=True)


def record(front, metric, system, value, unit, **extra):
    """One measurement (one run) as a JSON line."""
    try:
        load1 = float(open("/proc/loadavg").read().split()[0])
    except (OSError, ValueError):
        load1 = None
    # The batch protocol reports the load average with every result.
    row = {"front": front, "metric": metric, "system": system, "value": value, "unit": unit, "ts": time.time(), "load1": load1}
    row.update(extra)
    with open(RESULTS, "a") as f:
        f.write(json.dumps(row) + "\n")
    log(f"{front} {metric} {system}: {value} {unit}", extra if extra else "")


def check(front, name, ok, detail=""):
    """A functional check (pass/fail)."""
    record(front, "functional:" + name, "pico2peta", bool(ok), "bool", detail=detail)
    if not ok:
        log(f"FUNCTIONAL FAIL {front} {name}: {detail}")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port, timeout=60):
    t = time.time()
    while time.time() - t < timeout:
        with contextlib.suppress(OSError), socket.create_connection(("127.0.0.1", port), 0.5):
            return
        time.sleep(0.1)
    raise RuntimeError(f"port {port} did not open")


def run(cmd, timeout=3600, **kw):
    """Runs a command; raises with its output on failure."""
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, **kw)
    if p.returncode != 0:
        raise RuntimeError(f"{cmd!r} failed ({p.returncode}):\n{p.stdout[-3000:]}\n{p.stderr[-3000:]}")
    return p


def timed(cmd, **kw):
    t = time.perf_counter()
    run(cmd, **kw)
    return time.perf_counter() - t


def du(path):
    """Bytes on disk under path (allocated blocks)."""
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            with contextlib.suppress(OSError):
                total += os.lstat(os.path.join(root, f)).st_blocks * 512
    return total


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def median(xs):
    return statistics.median(xs)


@contextlib.contextmanager
def workdir(name):
    d = os.path.join(WORK, name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    try:
        yield d
    finally:
        shutil.rmtree(d, ignore_errors=True)


class Proc:
    """A background process, stopped (and its group killed) on exit."""

    def __init__(self, cmd, cwd=None, env=None, log_path=None):
        self.log = open(log_path or os.devnull, "w")
        self.p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)

    def stop(self):
        if self.p.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.p.pid, signal.SIGTERM)
            try:
                self.p.wait(30)
            except subprocess.TimeoutExpired:
                os.killpg(self.p.pid, signal.SIGKILL)
                self.p.wait()
        self.log.close()


@contextlib.contextmanager
def pico_server(d, server_bin, extra_toml="", repo_toml="", principal="bench"):
    """A pico2peta-server in d with one admin token. Yields (base_url, repo_path)."""
    port = free_port()
    repo = os.path.join(d, "repo")
    tok_sha = hashlib.sha256(TOKEN.encode()).hexdigest()
    cfg = f"""
[server]
listen = "127.0.0.1:{port}"
max_upload_bytes = 1099511627776
request_timeout_secs = 3600

[repo]
path = "{repo}"
create = true

[auth]
allow_anonymous = false
[[auth.tokens]]
principal = "{principal}"
sha256 = "{tok_sha}"

[[policy]]
principal = "{principal}"
namespace = ""
access = "admin"

[audit]
path = "{d}/audit.jsonl"
log_reads = false

[log]
format = "text"
filter = "warn"
{extra_toml}
"""
    path = os.path.join(d, "server.toml")
    with open(path, "w") as f:
        f.write(cfg)
    if repo_toml:
        # Initialize the repo first, then add repo-level settings (profiles, namespaces).
        p = Proc([server_bin, "serve", "--config", path], log_path=os.path.join(d, "server-init.log"))
        wait_port(port)
        p.stop()
        with open(os.path.join(repo, "config.toml"), "a") as f:
            f.write("\n" + repo_toml)
    p = Proc([server_bin, "serve", "--config", path], log_path=os.path.join(d, "server.log"))
    try:
        wait_port(port)
        yield f"http://127.0.0.1:{port}", repo
    finally:
        p.stop()


def metric_value(base, name, labels=""):
    """A counter from /metrics (sum over series matching the label substring)."""
    req = urllib.request.Request(base + "/metrics", headers={"Authorization": f"Bearer {TOKEN}"})
    total = 0.0
    for line in urllib.request.urlopen(req, timeout=10).read().decode().splitlines():
        if line.startswith(name) and labels in line and not line.startswith("#"):
            with contextlib.suppress(ValueError):
                total += float(line.rsplit(" ", 1)[1])
    return total


class CountingProxy:
    """TCP proxy counting bytes sent from the target to the client (bytes on the wire)."""

    def __init__(self, target_port):
        self.target = target_port
        self.down = 0
        self.lock = threading.Lock()
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(64)
        self.port = self.sock.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def _pipe(self, a, b, count):
        try:
            while True:
                data = a.recv(1 << 16)
                if not data:
                    break
                if count:
                    with self.lock:
                        self.down += len(data)
                b.sendall(data)
        except OSError:
            pass
        finally:
            with contextlib.suppress(OSError):
                b.shutdown(socket.SHUT_WR)

    def _accept(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            t = socket.create_connection(("127.0.0.1", self.target))
            threading.Thread(target=self._pipe, args=(c, t, False), daemon=True).start()
            threading.Thread(target=self._pipe, args=(t, c, True), daemon=True).start()

    def reset(self):
        with self.lock:
            n, self.down = self.down, 0
        return n

    def close(self):
        self.sock.close()
