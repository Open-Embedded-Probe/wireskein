"""What would a pruning rule have lost? Reads corpus/work/eval-<tag>.json.

For each in-scope true bus the evaluator recorded the L3 role scores of its
channels, the UART unit-candidate rank, and the layer score of the true
hypothesis. A rule "drop hypotheses whose X < threshold" loses the bus when the
true hypothesis falls below the threshold.
"""

import json
import sys
from collections import defaultdict

from corpus import ROOT

tag = sys.argv[1] if len(sys.argv) > 1 else "v4"
d = json.load(open(ROOT / f"corpus/work/eval-{tag}.json"))
buses = [b for r in d["records"] for b in r["buses"] if b["in_scope"]]
by = defaultdict(list)
for b in buses:
    by[b["protocol"]].append(b)

print(f"{len(buses)} in-scope buses from {tag}")
for proto, bs in sorted(by.items()):
    print(f"\n[{proto}] n={len(bs)}")
    role = {"uart": "data", "i2c": "scl", "spi": "clk"}[proto]
    key = {"uart": "async", "i2c": "clock", "spi": "clock"}[proto]
    vals = sorted(b["l3"][role][key] for b in bs)
    print(f"  L3 {role}.{key}: min={vals[0]:.3f}  p5={vals[len(vals) // 20]:.3f}  median={vals[len(vals) // 2]:.3f}")
    for thr in (0.3, 0.5, 0.7, 0.9):
        lost = sum(1 for v in vals if v < thr)
        print(f"    prune {role}.{key} < {thr}: lose {lost}/{len(vals)}")
    lv = sorted(b["true_layer"] for b in bs if b.get("true_layer") is not None)
    missing = sum(1 for b in bs if b.get("true_layer") is None)
    if lv:
        print(f"  true layer score: min={lv[0]:.3f} p5={lv[len(lv) // 20]:.3f}  (no true node: {missing})")
        for thr in (0.2, 0.4, 0.6):
            print(f"    prune layer < {thr}: lose {sum(1 for v in lv if v < thr) + missing}/{len(bs)}")
    if proto == "uart":
        ranks = [b.get("unit_rank") for b in bs]
        print("  true baud among L2 unit candidates, rank:",
              {k: ranks.count(k) for k in sorted(set(ranks), key=lambda x: (x is None, x))})
    rk = [b.get("rank") for b in bs]
    print("  rank of true hypothesis among rivals on its channels:",
          {k: rk.count(k) for k in sorted(set(rk), key=lambda x: (x is None, x))})
