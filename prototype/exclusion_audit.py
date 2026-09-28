"""Audit exclusion rules: do they ever remove a true hypothesis, and how much do they cut?

    PYTHONPATH=. uv run python exclusion_audit.py [n_synth] [--stress NAME]

Only proposals are enumerated (no decoding), so this is fast. A proposal is
"true" when it matches a ground-truth bus exactly (roles, and for UART baud
within 3 %, idle level and the exact frame format; for SPI the sampling edge
implied by the mode, CS polarity active-low, and bit order).
"""

import sys
from collections import Counter, defaultdict

from wsproto import corpus, synth
from wsproto.analyzers.i2c import I2c
from wsproto.analyzers.spi import Spi
from wsproto.analyzers.uart import Uart
from wsproto.exclude import RULES
from wsproto.stack import Context
from wsproto.survey import survey


def is_true(proto, p, bus, cap):
    if bus["protocol"] != proto:
        return False
    r, pm = bus["roles"], bus["params"]
    if proto == "uart":
        return (p["ch"] == r["data"] and p["idle"] == pm["idle"] and abs(p["baud"] / pm["baud"] - 1) <= 0.03
                and p["data_bits"] == pm["data_bits"] and p["parity"] == pm["parity"]
                and p["stop_bits"] == int(pm["stop_bits"]))
    if proto == "i2c":
        return p["scl"] == r["scl"] and p["sda"] == r["sda"]
    if proto == "spi":
        cpol, cpha = pm["mode"] >> 1, pm["mode"] & 1
        edge = "rise" if cpol == cpha else "fall"
        return (p["clk"] == r["clk"] and p["cs"] == r.get("cs") and (p["cs"] is None or p["cs_active"] == 0)
                and p["sample_edge"] == edge and p["bit_order"] == pm["bit_order"])
    return False


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1].isdigit() else 200
    stress = sys.argv[sys.argv.index("--stress") + 1] if "--stress" in sys.argv else None
    cases = list(corpus.real_cases()) + [synth.scenario(s, "mixed", stress) for s in range(n)]
    analyzers = {"uart": Uart(), "i2c": I2c(), "spi": Spi()}
    total = Counter()
    per_rule = defaultdict(Counter)
    lost = defaultdict(list)
    for cap, truth in cases:
        sv = survey(cap)
        ctx = Context(cap, sv.features, sv.active)
        buses = [b for b in truth["buses"] if b["protocol"] in analyzers]
        for proto, a in analyzers.items():
            for p in a.propose(ctx, None):
                t = any(is_true(proto, p, b, cap) for b in buses)
                total[(proto, "all")] += 1
                total[(proto, "true")] += t
                hit = False
                for r in RULES:
                    if r.protocol != proto:
                        continue
                    if r.test(p, sv, cap):
                        hit = True
                        per_rule[r.name]["excluded"] += 1
                        if t:
                            per_rule[r.name]["true_excluded"] += 1
                            lost[r.name].append(truth["id"])
                total[(proto, "excluded_any")] += hit
                if t and hit:
                    total[(proto, "true_lost_any")] += 1
    print(f"{len(cases)} cases{' stress=' + stress if stress else ''}")
    print(f"{'rule':22} {'kind':12} {'excluded':>10} {'share':>7} {'true lost':>9}")
    for r in RULES:
        c = per_rule[r.name]
        share = c["excluded"] / max(1, total[(r.protocol, "all")])
        print(f"{r.name:22} {r.kind:12} {c['excluded']:10d} {share:7.1%} {c['true_excluded']:9d}  {sorted(set(lost[r.name]))[:4]}")
    for proto in analyzers:
        a, e = total[(proto, "all")], total[(proto, "excluded_any")]
        print(f"[{proto}] proposals={a} true={total[(proto, 'true')]} excluded by any rule={e} ({e / max(1, a):.1%}) "
              f"true lost={total[(proto, 'true_lost_any')]}")


if __name__ == "__main__":
    main()
