#!/usr/bin/env python3
"""Summarize (and compare) the core suite's raw outputs (scripts/baseline-core.sh; PLAN §6.8).

  scripts/baseline-summary.py BASE_DIR              # per metric: n, median, min, max, range %, loads
  scripts/baseline-summary.py BASE_DIR NEW_DIR      # plus the §6.8 verdict per metric

Each metric has a direction: "+" (higher is better: throughput) or "-" (lower is better: time, latency,
size, memory). §6.8: the rerun's median may be worse than the baseline median by at most
max(3 %, the baseline's run-to-run range), where range = (max - min) / median over the baseline reps.
"""
import glob
import json
import os
import re
import statistics
import sys


def num(s):
    return float(re.sub(r"[^0-9.]", "", s))


def parse(path):
    """Metrics of one raw file: {name: (value, direction)}."""
    out = {}
    lines = open(path).read().splitlines()
    base = os.path.basename(path).rsplit("-r", 1)[0]
    rows = [[c.strip() for c in l.strip("|").split("|")] for l in lines if l.startswith("|") and "---" not in l]
    if base == "small":
        for r in rows:
            if r[0].startswith("re-put"):
                out["small re-put k/s"] = (num(r[1]), "+")
            elif r[0] == "ensure_live":
                out["small ensure_live k/s"] = (num(r[1]), "+")
            elif r[0].startswith("put"):
                out["small put 1t k/s"] = (num(r[1]), "+")
                out["small put 4t k/s"] = (num(r[2]), "+")
            elif r[0].startswith("get"):
                out["small get 1t k/s"] = (num(r[1]), "+")
                out["small get 4t k/s"] = (num(r[2]), "+")
    elif base == "commits":
        for r in rows:
            if r[0].isdigit():
                out[f"commits {r[0]} threads /s"] = (num(r[1]), "+")
    elif base == "stream":
        for r in rows:
            if r[0] in ("incompressible", "code-like text"):
                k = "random" if r[0] == "incompressible" else "text"
                out[f"stream {k} put MB/s"] = (num(r[1]), "+")
                out[f"stream {k} re-put MB/s"] = (num(r[2]), "+")
                out[f"stream {k} read MB/s"] = (num(r[3]), "+")
        for l in lines:
            if l.startswith("peak RSS"):
                out["stream peak RSS MB"] = (num(l.split(":")[1]), "-")
    else:
        for l in lines:
            if not l.startswith("{"):
                continue
            d = json.loads(l)
            if d.get("mode") == "reads":
                k = f"reads {d['input']} {d['size_kib']}KiB"
                for m in ("get_mb_s", "open_strict_mb_s", "open_eof_mb_s"):
                    out[f"{k} {m}"] = (d[m], "+")
            elif d.get("mode") == "ingest-level":
                out[f"ingest zstd{d['level']} MB/s"] = (d["mb_s"], "+")
                out[f"ingest zstd{d['level']} ratio"] = (d["ratio"], "-")
            elif d.get("mode") == "ingest":
                out[f"ingest {d['api']} MB/s"] = (d["mb_s"], "+")
            elif d.get("part") == "objects":
                k = f"scale objects {d['n']:.0e}"
                for m, dirn in (("put_rate", "+"), ("open_s", "-"), ("get_p99_ms", "-"), ("anon_mb", "-"),
                                ("rss_mb", "-"), ("disk_bytes", "-")):
                    out[f"{k} {m}"] = (d[m], dirn)
            elif d.get("part") == "gc":
                out[f"scale gc {d['n']:.0e} gc_s"] = (d["gc_s"], "-")
                out[f"scale gc {d['n']:.0e} peak_rss_mb"] = (d["peak_rss_mb"], "-")
                out[f"scale gc {d['n']:.0e} all_live_kept"] = (1.0 if d["all_live_kept"] else 0.0, "+")
            elif d.get("part") == "names":
                k = f"scale names {d['m']:.0e}"
                for m, dirn in (("replace_rate", "+"), ("resolve_p99_ms", "-"), ("open_s", "-"), ("rss_mb", "-")):
                    out[f"{k} {m}"] = (d[m], dirn)
            elif d.get("part") == "prov":
                out[f"scale prov {d['r']:.0e} load_s"] = (d["load_s"], "-")
                out[f"scale prov {d['r']:.0e} record_s"] = (d["record_s"], "-")
    return out


def loads(path):
    h = open(path).read().splitlines()
    a = re.search(r"load_start (\S+)", h[0]) if h else None
    b = re.search(r"load_end (\S+)", h[-1]) if h else None
    return [float(x.group(1)) for x in (a, b) if x]


def collect(d):
    """{metric: (direction, [values], [loads])}."""
    acc = {}
    for f in sorted(glob.glob(os.path.join(d, "*.txt"))):
        ls = loads(f)
        for k, (v, dirn) in parse(f).items():
            e = acc.setdefault(k, (dirn, [], []))
            e[1].append(v)
            e[2].extend(ls)
    return acc


def fmt(x):
    return f"{x:,.4g}" if abs(x) < 1000 else f"{x:,.0f}"


def main():
    base = collect(sys.argv[1])
    new = collect(sys.argv[2]) if len(sys.argv) > 2 else None
    head = "| metric | dir | n | median | min | max | range % | load |"
    if new is not None:
        head += " new median | change % | allowed % | verdict |"
    print(head)
    print("|" + "---|" * (head.count("|") - 1))
    fails = 0
    for k in sorted(base):
        dirn, vs, ls = base[k]
        med = statistics.median(vs)
        rng = (max(vs) - min(vs)) / med * 100 if med else 0.0
        row = (f"| {k} | {dirn} | {len(vs)} | {fmt(med)} | {fmt(min(vs))} | {fmt(max(vs))} | {rng:.1f} | "
               f"{min(ls):.2f}–{max(ls):.2f} |" if ls else f"| {k} | {dirn} | {len(vs)} | {fmt(med)} | | | {rng:.1f} | |")
        if new is not None:
            if k not in new:
                row += " missing | | | **FAIL** |"
                fails += 1
            else:
                nm = statistics.median(new[k][1])
                worse = ((med - nm) / med if dirn == "+" else (nm - med) / med) * 100 if med else 0.0
                allowed = max(3.0, rng)
                ok = worse <= allowed
                fails += 0 if ok else 1
                row += f" {fmt(nm)} | {-worse:+.1f} | {allowed:.1f} | {'ok' if ok else '**FAIL**'} |"
        print(row)
    if new is not None:
        print(f"\n{fails} metric(s) regressed beyond the allowance.")
        sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
