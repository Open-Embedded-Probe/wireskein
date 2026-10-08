"""TX/RX pairing and SCPI on top of two single-line UART results.

    uv run python scpi_eval.py [n]
"""
import sys
from collections import Counter

from wireskein._engine import staged, synth

n = int(sys.argv[1]) if len(sys.argv) > 1 else 60
c = Counter()
for s in range(n):
    cap, t = synth.scenario(s, "duplex")
    r = staged.analyze(cap)
    truth = [b for b in t["buses"] if b["protocol"] == "scpi"]
    uart_only = [b for b in t["buses"] if b["protocol"] == "uart"]
    pairs = [(tuple(sorted(x.roles.values())), x) for x in r.relations]
    for b in truth:
        c["scpi buses"] += 1
        want = tuple(sorted(b["roles"].values()))
        hit = [x for key, x in pairs if key == want]
        c["pair found"] += bool(hit)
        if hit:
            d = hit[0]
            sc = [ch for ch in d.children if ch.analyzer == "scpi"]
            if sc:
                c["scpi found"] += 1
                c["host/device right"] += sc[0].roles == b["roles"]
                c["exchanges exact"] += sc[0].output.items == b["expect"]["exchanges"]
    # false pairs: relations that are not a true scpi pair
    true_keys = {tuple(sorted(b["roles"].values())) for b in truth}
    c["relations"] += len(r.relations)
    c["false relations"] += sum(1 for key, _ in pairs if key not in true_keys)
    for b in uart_only + [x for x in t["buses"] if x["protocol"] == "uart"]:
        pass
    # single-line UART claims still right
    for b in t["buses"]:
        if b["protocol"] != "uart":
            continue
        c["uart lines"] += 1
        ok = [cl for cl in r.claims if cl.protocol == "uart" and cl.roles.get("data") == b["roles"]["data"]]
        c["uart line claimed"] += bool(ok)
        c["uart line confirmed"] += bool(ok) and ok[0].verdict == "confirmed"
for k in sorted(c):
    print(f"{k:22} {c[k]}")
