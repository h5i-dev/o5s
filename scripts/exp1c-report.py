#!/usr/bin/env python3
"""Experiment 1c report (PLAN.md §4.9). Reads Exp1's frozen baselines (results.jsonl)
and 1c results (results-1c.jsonl); prints the EXP1.md tables and the verdict."""
import json, math, sys
D = "/mnt/scratch/pico2peta/bench-data/exp1"
base = {}
for l in open(f"{D}/results.jsonl"):
    d = json.loads(l)
    if d.get("variant") == "git-aggressive":
        base[d["repo"]] = d  # last wins (re-runs)
res, extra = {}, {}
for l in open(f"{D}/results-1c.jsonl"):
    d = json.loads(l)
    if "variant" in d:
        res.setdefault(d["repo"], {})[d["variant"]] = d
    else:
        extra.setdefault(d["repo"], {})[d["baseline"]] = d
repos = [r for r in ["ripgrep", "jq", "redis", "git"] if r in res]
R = lambda r, v: res[r][v]["total_bytes"] / base[r]["total_bytes"]
print("| repo | objects | B (git --aggressive) | V6 R | V7 R | B' path-walk | B' name-hash v2 | V\\* R' (path-walk / nh2) |")
print("|---|---|---|---|---|---|---|---|")
gm = {}
for v in ["v6", "v7"]:
    vs = [R(r, v) for r in repos if v in res[r]]
    gm[v] = math.exp(sum(map(math.log, vs)) / len(vs)) if vs else float("inf")
vstar = min(gm, key=gm.get)
for r in repos:
    e = extra.get(r, {})
    pw = e.get("git-2.56-path-walk", {}).get("bytes")
    nh = e.get("git-2.56-name-hash-version=2", {}).get("bytes")
    vs = res[r].get(vstar)
    rp = lambda b: f"{vs['total_bytes'] / b:.3f}" if b and vs else "–"
    cell = lambda v: f"{R(r, v):.3f}" if v in res[r] else "–"
    num = lambda x: f"{x:,}" if x else "–"
    print(f"| {r} | {base[r]['objects']:,} | {base[r]['total_bytes']:,} | {cell('v6')} | {cell('v7')} | {num(pw)} | {num(nh)} | {rp(pw)} / {rp(nh)} |")
print()
print("| repo | V\\* build s | git --aggressive s | ratio | V\\* read p50 / p99 (ms) | mean / max depth | peak RSS | verified reads |")
print("|---|---|---|---|---|---|---|---|")
for r in repos:
    v = res[r].get(vstar)
    if not v:
        continue
    gb = base[r]["build_secs"]
    print(f"| {r} | {v['build_secs']:.1f} | {gb:.1f} | {v['build_secs'] / gb:.2f} | {v['read_p50_ms']:.2f} / {v['read_p99_ms']:.2f} | {v['mean_depth']:.1f} / {v['max_depth']} | {v['peak_rss_kb'] // 1024} MB | {v['verified']} |")
print()
rs = [R(r, vstar) for r in repos if vstar in res[r]]
if not rs:
    sys.exit("no V* results yet")
print(f"V* = {vstar} (geomean V6 {gm['v6']:.3f}, V7 {gm['v7']:.3f}); max R = {max(rs):.3f}; repos complete: {len(repos)}/4")
strong = gm[vstar] <= 1.00 and max(rs) <= 1.15
ok = gm[vstar] <= 1.15 and max(rs) <= 1.30
print("size verdict:", "STRONG PASS" if strong else "PASS (size)" if ok else "FAIL")
