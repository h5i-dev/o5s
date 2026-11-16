"""PLAN §6.5 verdicts from the JSONL results: median of runs, ratio to the specialist, tier.

Tiers (lower is better for every measured metric: seconds and bytes):
STRONG PASS <= 1.00x, PASS <= 1.25x, FAIL otherwise. Any failed functional check fails the front.

usage: report.py <results.jsonl>   (prints Markdown)
"""
import collections
import json
import statistics
import sys

BASELINE = {"git": "git-daemon", "oci": "distribution", "pypi": "devpi", "npm": "verdaccio", "maven": "reposilite"}
# HF: times vs the /v1 reference; family storage vs Xet chunk dedup.
# Since the PLAN §6.8 HF amendment (2026-11-16): times are judged on server-side busy time; the
# end-to-end times and the client's CPU are reported untiered.
# Amendment of the same day (human rulings): downloads are judged on server CPU; their busy time
# (which includes the client's read speed) is informational.
HF_BASE = {"upload_server_s": "v1-reference", "download_server_cpu_s": "v1-reference", "family_stored_bytes": "xet-dedup"}
# Measured (tiered) metrics per front; everything else is reported but not tiered.
TIERED = {
    "git": ["clone_v2_s", "clone_v0_s", "clone_wire_bytes", "fetch10_s", "push_s"],
    "oci": ["push_s", "pull_s", "stored_bytes"],
    "hf": ["upload_server_s", "download_server_cpu_s", "family_stored_bytes"],
    "pypi": ["hot_p50_s", "cold_p50_s", "stored_bytes"],
    "npm": ["hot_p50_s", "cold_p50_s", "stored_bytes"],
    "maven": ["hot_p50_s", "cold_p50_s", "stored_bytes"],
}


def tier(r):
    return "STRONG PASS" if r <= 1.0 else "PASS" if r <= 1.25 else "FAIL"


def fmt(v, unit):
    if unit == "B":
        return f"{v / 1e6:.1f} MB"
    if unit == "s":
        return f"{v:.3f} s"
    return f"{v}"


def main():
    rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
    vals = collections.defaultdict(list)
    loads = collections.defaultdict(list)
    units = {}
    func = collections.defaultdict(list)
    for r in rows:
        if r["metric"].startswith("functional:"):
            func[r["front"]].append((r["metric"][11:], r["value"], r.get("detail", "")))
            continue
        key = (r["front"], r["metric"], r.get("corpus", ""), r["system"])
        vals[key].append(r["value"])
        if r.get("load1") is not None:
            loads[key].append(r["load1"])
        units[(r["front"], r["metric"])] = r["unit"]
    med = {k: statistics.median(v) for k, v in vals.items()}
    out = ["| front | metric | corpus | pico2peta | specialist | ratio | tier | load (max of both) |", "|---|---|---|---|---|---|---|---|"]
    verdict = {}
    for front, metrics in TIERED.items():
        fails = [n for n, ok, _ in func.get(front, []) if not ok]
        worst = "STRONG PASS"
        for (f, m, corpus, system), v in sorted(med.items()):
            if f != front or m not in metrics or not system.startswith("pico2peta"):
                continue
            base_sys = HF_BASE.get(m) if front == "hf" else BASELINE[front]
            b = med.get((f, m, corpus, base_sys))
            if b is None or b == 0:
                continue
            ratio = v / b
            t = tier(ratio)
            if ["STRONG PASS", "PASS", "FAIL"].index(t) > ["STRONG PASS", "PASS", "FAIL"].index(worst):
                worst = t
            u = units[(f, m)]
            n = len(vals[(f, m, corpus, system)])
            ld = max(loads[(f, m, corpus, system)] + loads[(f, m, corpus, base_sys)], default=float("nan"))
            out.append(f"| {front} | {m} | {corpus} | {fmt(v, u)} ({system}, n={n}) | {fmt(b, u)} ({base_sys}) | {ratio:.2f}x | {t} | {ld:.2f} |")
        verdict[front] = ("FUNCTIONAL FAIL: " + ", ".join(fails)) if fails else worst
    print("\n".join(out))
    print()
    print("| front | functional checks | verdict (worst tier) |")
    print("|---|---|---|")
    for front in TIERED:
        fs = func.get(front, [])
        print(f"| {front} | {sum(1 for _, ok, _ in fs if ok)}/{len(fs)} passed | {verdict.get(front, 'not run')} |")
    # Untiered ratios added after pre-registration (not part of the verdicts).
    for (f, m, base_sys) in [("git", "first_push_s", "git-daemon"), ("hf", "upload_s", "v1-reference"), ("hf", "download_s", "v1-reference"),
                             ("hf", "download_server_s", "v1-reference"), ("hf", "upload_server_cpu_s", "v1-reference")]:
        for (f2, m2, corpus, system), v in sorted(med.items()):
            if (f2, m2) == (f, m) and system.startswith("pico2peta") and (f, m, corpus, base_sys) in med:
                b = med[(f, m, corpus, base_sys)]
                print(f"\n(untiered) {f} {m} {corpus}: {v:.3f} s vs {b:.3f} s ({base_sys}) = {v / b:.2f}x")
    # Untiered context (e.g. HF LFS-mode times, cold/hot totals).
    extra = [(k, v) for k, v in sorted(med.items()) if k[1] not in TIERED.get(k[0], [])]
    if extra:
        print("\nOther measurements (medians, not tiered):\n")
        for (f, m, corpus, system), v in extra:
            print(f"- {f} {m} {corpus} {system}: {fmt(v, units[(f, m)])}")


if __name__ == "__main__":
    main()
