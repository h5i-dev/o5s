#!/usr/bin/env python3
"""Experiment 1 report: applies the pre-registered rules of docs/PLAN.md §4.5
mechanically to results.jsonl and prints Markdown.

    scripts/exp1-report.py $BENCH_DATA/exp1/results.jsonl
"""
import json
import math
import sys

# Pre-registered thresholds (docs/PLAN.md §4.5). Do not edit.
STRONG_GEO, STRONG_MAX = 1.00, 1.15
PASS_GEO, PASS_MAX = 1.15, 1.30
P50_MS, P99_MS = 5.0, 25.0
BUILD_RATIO = 1.0
RSS_KB = 6 * 1024 * 1024
ELIGIBLE = ("v1", "v2")


def load(path):
    rows = {}
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        rows[(r["repo"], r["variant"])] = r  # last measurement wins
    return rows


def geomean(xs):
    return math.exp(sum(math.log(x) for x in xs) / len(xs))


def main(path):
    rows = load(path)
    repos = [r for r in ("ripgrep", "jq", "redis", "git") if (r, "git-aggressive") in rows]
    complete = [r for r in repos if all((r, v) in rows for v in ("v0", "v1", "v2", "v3"))]
    out = []
    out.append("| repo | objects | git --aggressive (B) | git gc (B) | V0 | V1 | V2 | V3 (ceiling) |")
    out.append("|---|---|---|---|---|---|---|---|")
    for r in repos:
        b = rows[(r, "git-aggressive")]["total_bytes"]
        plain = rows.get((r, "git-plain"), {}).get("total_bytes", "")
        cells = []
        for v in ("v0", "v1", "v2", "v3"):
            x = rows.get((r, v))
            cells.append(f"{x['total_bytes'] / b:.3f}" if x else "pending")
        obj = rows[(r, "git-aggressive")].get("objects", "")
        out.append(f"| {r} | {obj} | {b:,} | {plain:,} | " + " | ".join(cells) + " |")
    out.append("")
    out.append("Cells are R = S(V) / B, where S includes payload, dictionaries and a 48 B/object index model. Lower is better.")
    out.append("")

    # Secondary metrics for the eligible variants.
    out.append("| repo | variant | build s | git --aggressive s | build ratio | p50 ms | p99 ms | peak RSS MB |")
    out.append("|---|---|---|---|---|---|---|---|")
    for r in repos:
        gs = rows[(r, "git-aggressive")]["build_secs"]
        for v in ELIGIBLE:
            x = rows.get((r, v))
            if not x:
                continue
            out.append(
                f"| {r} | {v} | {x['build_secs']:.1f} | {gs:.1f} | {x['build_secs'] / gs:.2f} | "
                f"{x['read_p50_ms']} | {x['read_p99_ms']} | {x['peak_rss_kb'] / 1024:.0f} |"
            )
    out.append("")

    if len(complete) < len(("ripgrep", "jq", "redis", "git")):
        out.append(f"**Verdict: pending.** Complete repos so far: {', '.join(complete) or 'none'}.")
        if complete:
            for v in ELIGIBLE:
                rs = [rows[(r, v)]["total_bytes"] / rows[(r, "git-aggressive")]["total_bytes"] for r in complete]
                out.append(f"- Interim {v}: geomean R = {geomean(rs):.3f}, max R = {max(rs):.3f} (over {len(rs)} repos)")
        print("\n".join(out))
        return

    best = None
    for v in ELIGIBLE:
        rs = [rows[(r, v)]["total_bytes"] / rows[(r, "git-aggressive")]["total_bytes"] for r in complete]
        g = geomean(rs)
        if best is None or g < best[1]:
            best = (v, g, max(rs))
    v, g, mx = best
    sec_fail = []
    for r in complete:
        x = rows[(r, v)]
        gs = rows[(r, "git-aggressive")]["build_secs"]
        if x["read_p50_ms"] is None or x["read_p50_ms"] > P50_MS:
            sec_fail.append(f"{r}: p50 {x['read_p50_ms']} ms > {P50_MS}")
        if x["read_p99_ms"] is None or x["read_p99_ms"] > P99_MS:
            sec_fail.append(f"{r}: p99 {x['read_p99_ms']} ms > {P99_MS}")
        if x["build_secs"] / gs > BUILD_RATIO:
            sec_fail.append(f"{r}: build {x['build_secs']:.1f}s / git {gs:.1f}s = {x['build_secs'] / gs:.2f} > {BUILD_RATIO}")
        if x["peak_rss_kb"] > RSS_KB:
            sec_fail.append(f"{r}: peak RSS {x['peak_rss_kb'] / 1024:.0f} MB > 6144")
    size_strong = g <= STRONG_GEO and mx <= STRONG_MAX
    size_pass = g <= PASS_GEO and mx <= PASS_MAX
    if size_pass and not sec_fail:
        verdict = "STRONG PASS" if size_strong else "PASS"
    else:
        verdict = "FAIL"
    out.append(f"**V\\* = {v}**: geomean R = {g:.3f}, max R = {mx:.3f}.")
    out.append(f"- Size condition: {'STRONG PASS' if size_strong else 'PASS' if size_pass else 'FAIL'}"
               f" (pass: geomean ≤ {PASS_GEO} and max ≤ {PASS_MAX}; strong: ≤ {STRONG_GEO} and ≤ {STRONG_MAX}).")
    if sec_fail:
        out.append("- Secondary thresholds missed:")
        out.extend(f"  - {s}" for s in sec_fail)
    else:
        out.append("- Secondary thresholds: all met.")
    out.append(f"\n**Verdict (pre-registered rules): {verdict}.**")
    print("\n".join(out))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "results.jsonl")
