"""PLAN §6.5, PyPI: o5s's PyPI front as a proxy-cache of pypi.org vs devpi-server.

Per system and run (a fresh, empty proxy each run): for each of 50 packages,
`pip download --no-deps --only-binary :all:` once (cold miss: fetched from pypi.org) and once
more (hit: served from the cache), with pip's own cache off. Then stored bytes, and the
functional check that o5s fetched each file from upstream exactly once.

usage: pypi_bench.py <o5s-server binary>
"""
import os
import sys
import time

from common import Proc, RUNS, TOKEN, TOOLS, check, du, free_port, log, metric_value, o5s_server, record, run, wait_port, workdir

PIP_PY = f"{TOOLS}/pkgvenv/bin/python"
DEVPI = f"{TOOLS}/devpivenv/bin"
UPSTREAM = "https://pypi.org/simple"
PACKAGES = (
    "six idna certifi urllib3 requests packaging pyparsing attrs click jinja2 itsdangerous werkzeug flask colorama "
    "tqdm toml tomli pluggy iniconfig exceptiongroup typing-extensions zipp importlib-metadata filelock platformdirs "
    "distlib pytz python-dateutil tzdata soupsieve beautifulsoup4 chardet docutils pygments wheel decorator cachetools "
    "pyasn1 rsa jmespath pycparser mccabe pyflakes pycodestyle isort sniffio h11 more-itertools blinker markdown"
).split()
assert len(PACKAGES) == 50, len(PACKAGES)


def pip_download(index, dest, pkg):
    t = time.perf_counter()
    run([PIP_PY, "-m", "pip", "download", "--disable-pip-version-check", "--no-input", "--no-cache-dir", "--no-deps",
         "--only-binary", ":all:", "-q", "--index-url", index, "-d", dest, pkg])
    return time.perf_counter() - t


def bench_pass(system, index, d, i):
    """Cold then hot pass over all packages; per-file latencies recorded."""
    files = {}
    for phase in ("cold", "hot"):
        dest = os.path.join(d, f"dl-{system}-{i}-{phase}")
        os.makedirs(dest)
        lat = []
        for pkg in PACKAGES:
            before = set(os.listdir(dest))
            lat.append(pip_download(index, dest, pkg))
            new = set(os.listdir(dest)) - before
            if phase == "cold":
                files[pkg] = sorted(new)
        lat.sort()
        record("pypi", f"{phase}_p50_s", system, lat[len(lat) // 2], "s", run=i, files=len(lat))
        record("pypi", f"{phase}_total_s", system, sum(lat), "s", run=i, files=len(lat))
        run(["rm", "-rf", dest])
    return files


def main():
    server_bin = sys.argv[1]
    for i in range(RUNS):
        with workdir(f"pypi-{i}") as d:
            # Baseline: devpi-server, root/pypi mirrors pypi.org.
            sd = os.path.join(d, "devpi")
            run([f"{DEVPI}/devpi-init", "--serverdir", sd])
            port = free_port()
            dv = Proc([f"{DEVPI}/devpi-server", "--serverdir", sd, "--host", "127.0.0.1", "--port", str(port)],
                      log_path=os.path.join(d, "devpi.log"))
            try:
                wait_port(port, 120)
                log("pypi: devpi run", i)
                bench_pass("devpi", f"http://127.0.0.1:{port}/root/pypi/+simple/", d, i)
                record("pypi", "stored_bytes", "devpi", du(sd), "B", run=i)
            finally:
                dv.stop()
            o5s = os.path.join(d, "o5s")
            os.makedirs(o5s)
            extra = f'\n[pypi.upstream.mirror]\nurl = "{UPSTREAM}"\n'
            with o5s_server(o5s, server_bin, extra_toml=extra) as (base, repo):
                log("pypi: o5s run", i)
                host = base.removeprefix("http://")
                files = bench_pass("o5s", f"http://bench:{TOKEN}@{host}/pypi/mirror/simple/", d, i)
                record("pypi", "stored_bytes", "o5s", du(repo), "B", run=i)
                fetched = metric_value(base, "o5s_upstream_fetches", 'front="pypi"')
                n = sum(len(v) for v in files.values())
                check("pypi", f"one_fetch_per_file_run{i}", fetched == n, f"{fetched} upstream fetches for {n} files")
                check("pypi", f"all_files_run{i}", n == len(PACKAGES), f"{n} files")


if __name__ == "__main__":
    main()
