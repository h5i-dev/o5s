"""PLAN §6.5, npm: pico2peta's npm front as a proxy-cache of registry.npmjs.org vs verdaccio.

Per system and run (a fresh, empty proxy each run): for each of 50 packages, `npm pack <pkg>`
(fetches the packument and that one tarball) once cold and once hot, each with a fresh npm
cache. Then stored bytes, and the check that pico2peta fetched each tarball once.

usage: npm_bench.py <pico2peta-server binary>
"""
import os
import shutil
import sys
import time

from common import Proc, RUNS, TOKEN, TOOLS, check, du, free_port, log, metric_value, pico_server, record, run, wait_port, workdir

VERDACCIO = f"{TOOLS}/verdaccio/node_modules/.bin/verdaccio"
UPSTREAM = "https://registry.npmjs.org"
PACKAGES = (
    "lodash ms debug chalk commander semver uuid minimist yargs-parser glob rimraf mkdirp inherits once wrappy "
    "safe-buffer kind-of is-number isarray balanced-match brace-expansion concat-map path-is-absolute ansi-regex "
    "strip-ansi color-name color-convert has-flag supports-color escape-string-regexp js-tokens loose-envify "
    "object-assign react-is prop-types picomatch fill-range to-regex-range normalize-path readable-stream "
    "string_decoder util-deprecate core-util-is process-nextick-args isexe which signal-exit cross-spawn path-key shebang-regex"
).split()
assert len(PACKAGES) == 50, len(PACKAGES)


def npm_pack(rc, dest, pkg, d):
    cache = os.path.join(d, f"npmcache-{time.monotonic_ns()}")
    t = time.perf_counter()
    run(["npm", "pack", pkg, "--userconfig", rc, "--cache", cache, "--pack-destination", dest, "--silent",
         "--no-audit", "--no-fund", "--fetch-retries", "0", "--update-notifier", "false"],
        env=dict(os.environ, TMPDIR=os.path.join(d, "tmp")))
    s = time.perf_counter() - t
    shutil.rmtree(cache, ignore_errors=True)
    return s


def bench_pass(system, rc, d, i):
    os.makedirs(os.path.join(d, "tmp"), exist_ok=True)
    n = 0
    for phase in ("cold", "hot"):
        dest = os.path.join(d, f"dl-{system}-{i}-{phase}")
        os.makedirs(dest)
        lat = sorted(npm_pack(rc, dest, p, d) for p in PACKAGES)
        record("npm", f"{phase}_p50_s", system, lat[len(lat) // 2], "s", run=i, files=len(lat))
        record("npm", f"{phase}_total_s", system, sum(lat), "s", run=i, files=len(lat))
        n = len(os.listdir(dest))
        run(["rm", "-rf", dest])
    return n


def main():
    server_bin = sys.argv[1]
    for i in range(RUNS):
        with workdir(f"npm-{i}") as d:
            port = free_port()
            store = os.path.join(d, "verdaccio-storage")
            cfg = os.path.join(d, "verdaccio.yaml")
            with open(cfg, "w") as f:
                f.write(f"storage: {store}\nuplinks:\n  npmjs:\n    url: {UPSTREAM}/\npackages:\n  '**':\n    access: $all\n"
                        f"    proxy: npmjs\nlisten: 127.0.0.1:{port}\nlog: {{ type: stdout, format: pretty, level: error }}\n")
            v = Proc([VERDACCIO, "--config", cfg], log_path=os.path.join(d, "verdaccio.log"))
            try:
                wait_port(port, 120)
                rc = os.path.join(d, "verdaccio.npmrc")
                open(rc, "w").write(f"registry=http://127.0.0.1:{port}/\n")
                log("npm: verdaccio run", i)
                bench_pass("verdaccio", rc, d, i)
                record("npm", "stored_bytes", "verdaccio", du(store), "B", run=i)
            finally:
                v.stop()
            p2p = os.path.join(d, "p2p")
            os.makedirs(p2p)
            with pico_server(p2p, server_bin, extra_toml=f'\n[npm.upstream.mirror]\nurl = "{UPSTREAM}"\n') as (base, repo):
                host = base.removeprefix("http://")
                rc = os.path.join(d, "p2p.npmrc")
                open(rc, "w").write(f"registry=http://{host}/npm/mirror/\n//{host}/npm/mirror/:_authToken={TOKEN}\n")
                log("npm: pico2peta run", i)
                n = bench_pass("pico2peta", rc, d, i)
                record("npm", "stored_bytes", "pico2peta", du(repo), "B", run=i)
                fetched = metric_value(base, "p2p_upstream_fetches", 'front="npm"')
                check("npm", f"one_fetch_per_file_run{i}", fetched == len(PACKAGES), f"{fetched} upstream fetches for {len(PACKAGES)} packages")
                check("npm", f"all_files_run{i}", n == len(PACKAGES), f"{n} tarballs")


if __name__ == "__main__":
    main()
