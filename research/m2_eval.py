"""M2: single-channel features against ground truth."""

import sys
from collections import Counter

import corpus
from wireskein._engine import features

n = int(sys.argv[1]) if len(sys.argv) > 1 else 200
stats = Counter()
baud_err = []
fails = []
for cap, truth in [*corpus.real_cases(), *corpus.synth_cases(n)]:
    fs = features.features(cap)
    for name, info in truth["channels"].items():
        stats["static_total"] += info["static"]
        stats["static_ok"] += info["static"] and fs[name].static
        stats["static_false"] += (not info["static"]) and fs[name].static
    for bus in truth["buses"]:
        if bus["protocol"] == "uart":
            f = fs[bus["roles"]["data"]]
            want = cap.rate / bus["params"]["baud"]
            best = f.units[0].samples if f.units else float("nan")
            err = best / want - 1
            anyc = min((abs(u.samples / want - 1) for u in f.units), default=9)
            baud_err.append(err)
            stats["uart_total"] += 1
            stats["uart_top1_2pct"] += abs(err) <= 0.02
            stats["uart_any_2pct"] += anyc <= 0.02
            stats["uart_idle_ok"] += f.idle_level == bus["params"]["idle"]
            stats["uart_clock_gt_async"] += f.scores["clock"] > f.scores["async"]
            if abs(err) > 0.02:
                fails.append((truth["id"], "uart", round(want, 2), [round(u.samples, 2) for u in f.units]))
        clk_role = {"i2c": "scl", "spi": "clk"}.get(bus["protocol"])
        if clk_role:
            f = fs[bus["roles"][clk_role]]
            want = cap.rate / bus["params"]["clock_hz"]
            stats[f"{bus['protocol']}_clk_total"] += 1
            stats[f"{bus['protocol']}_clk_period_5pct"] += bool(f.period) and abs(f.period / want - 1) <= 0.05
            stats[f"{bus['protocol']}_clk_score_gt_async"] += f.scores["clock"] > f.scores["async"]
            if not (f.period and abs(f.period / want - 1) <= 0.05):
                fails.append((truth["id"], clk_role, round(want, 1), f.period))
for k in sorted(stats):
    print(f"{k:28} {stats[k]}")
for f in fails[:15]:
    print("FAIL", f)
