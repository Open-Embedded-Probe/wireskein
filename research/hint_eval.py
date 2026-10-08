"""Effect of hints given before analysis: none, protocol list only, per-pin protocol/role/baud.

    uv run python hint_eval.py [n]
"""
import sys
import time
from collections import Counter

from evaluate import match
import corpus
from wireskein._engine import staged, synth


def hints_from(truth, level):
    if level == "none":
        return None
    protos = sorted({b["protocol"] for b in truth["buses"]})
    if level == "protocols":
        return {"protocols": protos}
    pins = {}
    for b in truth["buses"]:
        for role, ch in b["roles"].items():
            pins[ch] = {"protocol": b["protocol"], "role": role}
            if b["protocol"] == "uart":
                pins[ch]["baud"] = b["params"]["baud"]
    return {"protocols": protos, "pins": pins}


n = int(sys.argv[1]) if len(sys.argv) > 1 else 100
cases = list(corpus.real_cases()) + [synth.scenario(s) for s in range(n)]
for level in ("none", "protocols", "pins"):
    c, secs = Counter(), 0.0
    for cap, truth in cases:
        t0 = time.perf_counter()
        r = staged.analyze(cap, hints_from(truth, level))
        secs += time.perf_counter() - t0
        c["runs"] += r.runs
        for b in truth["buses"]:
            chans = set(b["roles"].values())
            if all(len(cap.channel(x).edges) == 0 for x in chans):
                continue
            c["buses"] += 1
            ok = [cl for cl in r.claims if (m := match(b, cl.node, cap.rate))["roles"] and m["params"]]
            if ok:
                c[ok[0].verdict] += 1
            elif any(chans & set(cl.roles.values()) for cl in r.claims):
                c["wrong"] += 1
            else:
                c["missed"] += 1
        c["false_confirmed"] += sum(1 for cl in r.claims if cl.verdict == "confirmed" and not any(
            (m := match(b, cl.node, cap.rate))["roles"] and m["params"] for b in truth["buses"]))
    tot = c["buses"]
    print(f"hint={level:9} time={secs:6.1f}s plugin runs={c['runs']:6d} confirmed={c['confirmed'] / tot:.3f} "
          f"conf+likely={(c['confirmed'] + c['likely']) / tot:.3f} wrong={c['wrong']} missed={c['missed']} false_confirmed={c['false_confirmed']}")
