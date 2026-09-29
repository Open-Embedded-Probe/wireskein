"""Would best-first expansion find the final answer early?

For each case: run the exhaustive pipeline, give every root hypothesis a prior
computed from the common survey only (before decoding), drop proposals removed
by the safe exclusion rules, and report at which position (in prior order) the
nodes of the final best explanation appear. If they always come early, a
budgeted best-first search can stop long before exhaustion.

    PYTHONPATH=. uv run python bestfirst_study.py [n_synth]
"""

import json
import sys

import numpy as np

import corpus
from wireskein._engine import pipeline, synth
from wireskein._engine.exclude import RULES
from wireskein._engine.stack import ProbeConfig
from wireskein._engine.survey import survey

SAFE = [r for r in RULES if r.kind == "definitional"]  # audited: zero true losses incl. stress profiles


def prior(n, sv, cap) -> float:
    p = n.params
    if n.analyzer == "uart":
        f = sv.features[p["ch"]]
        T = cap.rate / p["baud"]
        unit_fit = max((u.fit for u in f.units if abs(u.samples / T - 1) <= 0.03), default=0.0)
        a = sv.asyncs.get(p["ch"])
        L = 1 + p["data_bits"] + (0 if p["parity"] == "none" else 1) + int(p["stop_bits"])
        char = 0.5
        if a and a.unit and abs(T / a.unit - 1) <= 0.03:
            char = dict(a.char_bits).get(L, 0.0)
        return f.scores.get("async", 0) * unit_fit * char
    if n.analyzer == "i2c":
        r = sv.pairs.get((p["scl"], p["sda"]))
        c = sv.clocks.get(p["scl"])
        return (c.clock_score if c else 0) * (r.data_score if r else 0)
    if n.analyzer == "spi":
        c = sv.clocks.get(p["clk"])
        best = max((r.data_score for (k, o), r in sv.pairs.items() if k == p["clk"] and o != p["cs"]), default=0)
        cs = sv.pairs[(p["clk"], p["cs"])].boundary if p["cs"] and (p["clk"], p["cs"]) in sv.pairs else 0.5
        return (c.clock_score if c else 0) * best * cs
    return 0.0


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 40
    cases = list(corpus.real_cases())[:4] + [synth.scenario(s) for s in range(n)]
    rows = []
    for cap, truth in cases:
        res = pipeline.analyze(cap, probe=ProbeConfig())
        sv = survey(cap, res.features)
        roots = [r for r in res.roots]
        keep = [r for r in roots if not any(rule.protocol == r.analyzer and rule.test(r.params, sv, cap) for rule in SAFE)]
        pri = np.array([prior(r, sv, cap) for r in keep])
        order = np.argsort(-pri, kind="stable")
        pos = {id(keep[i]): k for k, i in enumerate(order)}
        best = res.explanations[0][1] if res.explanations else []
        claim_pos = [pos.get(id(nd)) for nd in best]
        excluded_claim = sum(1 for p_ in claim_pos if p_ is None)
        found = [p_ for p_ in claim_pos if p_ is not None]
        rows.append({"id": truth["id"], "roots": len(roots), "kept": len(keep),
                     "claims": len(best), "claims_excluded": excluded_claim,
                     "max_pos": max(found) if found else None,
                     "max_pos_share": (max(found) + 1) / len(keep) if found and keep else None})
        print(json.dumps(rows[-1]))
    kept = sum(r["kept"] for r in rows) / sum(r["roots"] for r in rows)
    shares = [r["max_pos_share"] for r in rows if r["max_pos_share"] is not None]
    print(f"\ncases={len(rows)} kept after safe exclusion={kept:.1%} "
          f"claims excluded={sum(r['claims_excluded'] for r in rows)} "
          f"position of last claim (share of kept): median={np.median(shares):.2%} p90={np.quantile(shares, 0.9):.2%} "
          f"max={max(shares):.2%}; absolute max position median={np.median([r['max_pos'] for r in rows if r['max_pos'] is not None]):.0f}")


if __name__ == "__main__":
    main()
