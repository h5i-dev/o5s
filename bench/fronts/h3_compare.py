"""PLAN §6.8 rule for net's §6.5 batch: H3 rows against the pre-h5i baseline.

usage: h3_compare.py <baseline.jsonl> <h3.jsonl> [<baseline-report.md> <h3-report.md>]

Per (front, metric, corpus, system), the medians are compared. A metric fails when it is worse
by more than max(3 %, its baseline range (max-min)/median). Worse = higher, except throughputs
(`_mbps`, `_per_s`, `throughput`), where lower is worse. Functional checks must all be true. With
the two reports, tiers are compared too (no tier may get worse). Reference systems (the
specialists) are listed but are not pico2peta's regressions.
"""
import collections, json, statistics, sys

PICO = ("pico2peta",)
TIER = {"STRONG PASS": 3, "PASS": 2, "FAIL": 1}


def load(p):
    rows = collections.defaultdict(list)
    func = {}
    for line in open(p):
        r = json.loads(line)
        v = r["value"]
        if r["metric"].startswith("functional"):
            func[(r["front"], r["metric"])] = v
            continue
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            continue
        rows[(r["front"], r["metric"], r.get("corpus") or "", r["system"])].append(v)
    return rows, func


def higher_is_worse(metric):
    return not any(t in metric for t in ("_mbps", "_per_s", "throughput"))


def tiers(report):
    out = {}
    for line in open(report):
        c = [x.strip() for x in line.split("|")]
        if len(c) > 8 and c[7] in TIER:
            out[(c[1], c[2], c[3], c[4].split(" (")[1].split(",")[0] if " (" in c[4] else "")] = c[7]
    return out


def main():
    b, _ = load(sys.argv[1])
    h, func = load(sys.argv[2])
    fails, lines = [], []
    for k in sorted(h):
        if k not in b:
            continue
        bv, hv = b[k], h[k]
        bm, hm = statistics.median(bv), statistics.median(hv)
        rng = (max(bv) - min(bv)) / bm if len(bv) > 1 and bm else 0.0
        allow = max(0.03, rng)
        delta = (hm - bm) / bm if bm else 0.0
        worse = delta if higher_is_worse(k[1]) else -delta
        ok = worse <= allow
        ref = k[3] not in PICO and not k[3].startswith("pico2peta")
        tag = "ref" if ref else ("ok" if ok else "FAIL")
        lines.append(f"| {k[0]} | {k[1]} | {k[2]} | {k[3]} | {bm:.4g} | {hm:.4g} | {delta:+.1%} | {allow:.1%} | {tag} |")
        if not ok and not ref:
            fails.append(k)
    print("| front | metric | corpus | system | baseline | H3 | Δ | allowed | verdict |")
    print("|---|---|---|---|---|---|---|---|---|")
    print("\n".join(lines))
    bad = [k for k, v in func.items() if v is not True]
    print(f"\nfunctional checks: {len(func)}, failing: {bad or 'none'}")
    if len(sys.argv) > 4:
        bt, ht = tiers(sys.argv[3]), tiers(sys.argv[4])
        worse_t = [(k, bt[k], ht[k]) for k in bt if k in ht and TIER[ht[k]] < TIER[bt[k]]]
        print(f"tiers compared: {len([k for k in bt if k in ht])}, worse: {worse_t or 'none'}")
        fails += [k for k, _, _ in worse_t]
    print(f"metric FAILs: {len(fails)}")
    for k in fails:
        print("  FAIL", *k)


if __name__ == "__main__":
    main()
