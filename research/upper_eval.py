"""Effect of upper-layer support on UART verdicts (NMEA, Modbus RTU, text, binary payloads).

    uv run python upper_eval.py [n]
"""
import sys
from collections import defaultdict

import numpy as np

from wireskein._engine import staged, synth

n = int(sys.argv[1]) if len(sys.argv) > 1 else 80
rows = defaultdict(list)
checks = defaultdict(list)
for s in range(n):
    cap, t = synth.scenario(s, "upper")
    r = staged.analyze(cap)
    for b in t["buses"]:
        pin = b["roles"]["data"]
        nodes = [x for x in r.roots if x.analyzer == "uart" and x.roles.get("data") == pin and x.output is not None]
        if not nodes:
            rows[b["params"]["payload"]].append((0, 0, "missed"))
            continue
        nd = max(nodes, key=lambda x: x.total)
        cl = [c for c in r.claims if c.node is nd]
        rows[b["params"]["payload"]].append((nd.layer_score, nd.total, cl[0].verdict if cl else "not claimed"))
        best_child = max(nd.children, key=lambda c: c.total, default=None)
        checks[b["params"]["payload"]].append(best_child.analyzer if best_child is not None and best_child.total > 0.3 else "-")
        if b["params"]["payload"] == "modbus":
            mb = [c for c in nd.children if c.analyzer == "modbus_rtu"]
            got = mb[0].output.items if mb and mb[0].output else []
            checks["modbus frames exact"].append(got == b["expect"]["modbus"])
print(f"{'payload':8} {'n':>3} {'layer':>6} {'total':>6} {'>=0.85 layer':>13} {'>=0.85 total':>13}  verdicts / best upper child")
for k, v in rows.items():
    L = np.array([x[0] for x in v]); T = np.array([x[1] for x in v])
    verd = {x: sum(1 for y in v if y[2] == x) for x in set(y[2] for y in v)}
    ch = {x: checks[k].count(x) for x in set(checks[k])}
    print(f"{k:8} {len(v):3d} {L.mean():6.3f} {T.mean():6.3f} {int((L >= 0.85).sum()):13d} {int((T >= 0.85).sum()):13d}  {verd} {ch}")
print("modbus frames exact:", sum(checks["modbus frames exact"]), "/", len(checks["modbus frames exact"]))
