"""PLAN §6.5, Maven: o5s's Maven front as a proxy-cache of Maven Central vs reposilite.

Per system and run (a fresh, empty proxy each run): for each of 50 artifacts,
`maven-dependency-plugin:get -Dtransitive=false` (the jar, its pom and parent poms) once cold
and once hot. Each invocation uses a fresh copy of a local repository that already holds
Maven's plugins (warmed from Central under the same mirror id), so the timed requests are the
artifact's own files, all through the proxy (mirrorOf *). Then stored bytes, and the check
that hot requests caused no upstream fetches (each file fetched once).

usage: maven_bench.py <o5s-server binary>
"""
import json
import os
import shutil
import sys
import time

from common import Proc, RUNS, TOKEN, TOOLS, check, du, free_port, log, metric_value, o5s_server, record, run, wait_port, workdir

MVN = f"{TOOLS}/maven/bin/mvn"
REPOSILITE = f"{TOOLS}/reposilite/reposilite.jar"
CENTRAL = "https://repo.maven.apache.org/maven2"
PLUGIN = "org.apache.maven.plugins:maven-dependency-plugin:3.7.1:get"
ARTIFACTS = [l.strip() for l in open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "maven_artifacts.txt")) if l.strip()]
assert len(ARTIFACTS) == 50, len(ARTIFACTS)


def settings(path, url, user=None, password=None):
    creds = f"<servers><server><id>o5s</id><username>{user}</username><password>{password}</password></server></servers>" if user else ""
    open(path, "w").write(
        f"<settings>{creds}<mirrors><mirror><id>o5s</id><mirrorOf>*</mirrorOf><url>{url}</url></mirror></mirrors></settings>"
    )


def mvn(s, local, d, gav):
    return [MVN, "-B", "-q", "-ntp", "-s", s, f"-Dmaven.repo.local={local}", PLUGIN, f"-Dartifact={gav}", "-Dtransitive=false"]


def warm(d):
    """A local repo with the plugin and its dependencies, recorded under mirror id o5s."""
    s = os.path.join(d, "warm-settings.xml")
    settings(s, CENTRAL)
    l0 = os.path.join(d, "m2-warm")
    run(mvn(s, l0, d, "org.reactivestreams:reactive-streams:1.0.4"), env=dict(os.environ, MAVEN_OPTS=f"-Djava.io.tmpdir={d}/tmp"))
    # Drop the warm-up artifact itself so the measurement fetches it like the others.
    shutil.rmtree(os.path.join(l0, "org/reactivestreams"), ignore_errors=True)
    return l0


def bench_pass(system, s, l0, d, i):
    env = dict(os.environ, MAVEN_OPTS=f"-Djava.io.tmpdir={d}/tmp")
    os.makedirs(os.path.join(d, "tmp"), exist_ok=True)
    for phase in ("cold", "hot"):
        lat = []
        for gav in ARTIFACTS:
            local = os.path.join(d, "m2-run")
            shutil.rmtree(local, ignore_errors=True)
            shutil.copytree(l0, local, symlinks=True)
            t = time.perf_counter()
            run(mvn(s, local, d, gav), env=env)
            lat.append(time.perf_counter() - t)
            g, a, v = gav.split(":")
            jar = os.path.join(local, *g.split("."), a, v, f"{a}-{v}.jar")
            if not os.path.exists(jar):
                check("maven", f"resolved_{system}_{gav}_{phase}_run{i}", False, "jar missing")
        shutil.rmtree(os.path.join(d, "m2-run"), ignore_errors=True)
        lat.sort()
        record("maven", f"{phase}_p50_s", system, lat[len(lat) // 2], "s", run=i, files=len(lat))
        record("maven", f"{phase}_total_s", system, sum(lat), "s", run=i, files=len(lat))
        yield phase


def main():
    server_bin = sys.argv[1]
    with workdir("maven-warm") as wd:
        os.makedirs(os.path.join(wd, "tmp"))
        log("maven: warming the plugin repository")
        l0 = warm(wd)
        for i in range(RUNS):
            with workdir(f"maven-{i}") as d:
                rp = os.path.join(d, "reposilite")
                os.makedirs(rp)
                shared = os.path.join(d, "shared.json")
                run(["java", "-jar", REPOSILITE, "-wd", rp, "-gc", "configuration.shared.json"], cwd=rp)
                cfg = json.load(open(os.path.join(rp, "configuration.shared.json")))
                for r in cfg["maven"]["repositories"]:
                    if r["id"] == "releases":
                        r["proxied"] = [{"reference": CENTRAL, "store": True, "connectTimeout": 10, "readTimeout": 60,
                                         "allowedGroups": [], "allowedExtensions": [], "proxy": ""}]
                json.dump(cfg, open(shared, "w"))
                port = free_port()
                rs = Proc(["java", "-Xmx1g", "-jar", REPOSILITE, "-wd", rp, "-p", str(port), "-sc", shared], cwd=rp,
                          log_path=os.path.join(d, "reposilite.log"))
                try:
                    wait_port(port, 180)
                    s = os.path.join(d, "rp-settings.xml")
                    settings(s, f"http://127.0.0.1:{port}/releases")
                    log("maven: reposilite run", i)
                    for _ in bench_pass("reposilite", s, l0, d, i):
                        pass
                    record("maven", "stored_bytes", "reposilite", du(os.path.join(rp, "repositories")), "B", run=i)
                finally:
                    rs.stop()
                o5s = os.path.join(d, "o5s")
                os.makedirs(o5s)
                extra = f'\n[maven.upstream.central]\nurl = "{CENTRAL}"\n'
                with o5s_server(o5s, server_bin, extra_toml=extra) as (base, repo):
                    s = os.path.join(d, "o5s-settings.xml")
                    settings(s, f"{base}/maven/central", "bench", TOKEN)
                    log("maven: o5s run", i)
                    after = {}
                    for phase in bench_pass("o5s", s, l0, d, i):
                        after[phase] = metric_value(base, "o5s_upstream_fetches", 'front="maven"')
                    record("maven", "stored_bytes", "o5s", du(repo), "B", run=i)
                    check("maven", f"no_refetch_on_hit_run{i}", after["hot"] == after["cold"] and after["cold"] > 0,
                          f"{after['cold']} upstream fetches cold, {after['hot']} after hot")


if __name__ == "__main__":
    main()
