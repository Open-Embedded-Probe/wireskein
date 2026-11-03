"""analyze on the large real records (RVSWD / SWIO flash writes, 0.9-1.6 M edges): the time, the claims, and how
much of the expected debug-module traffic the best RVSWD / SWIO claim reads."""
import time
from difflib import SequenceMatcher

from wireskein import decode

import corpus

for p in corpus.cases(corpus.REAL):
    if not corpus.large(p):
        continue
    cap, t = corpus.load(p)
    t0 = time.perf_counter()
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    dt = time.perf_counter() - t0
    edges = sum(len(c.edges) for c in cap.channels)
    print(f"== {p.stem}: {edges} edges, {dt:.1f} s", flush=True)
    print("   claims", [(c["verdict"], c["protocol"], c["score"]) for c in claims], flush=True)
    best = max((c for c in claims if c["protocol"] in ("rvswd", "swio")), key=lambda c: c["score"], default=None)
    if best is None:
        continue
    exp = t["buses"][0].get("expect", {})
    if "dmi" in exp:
        names = {x["reg"]: x["addr"] for x in best["layers"]["dmi"] if x.get("reg")}
        want = [(op, names.get(nm, int(nm, 16) if nm.startswith("0x") else -1), dd) for op, nm, dd in exp["dmi"]]
        got = [(x["op"], x["addr"], x["data"]) for x in best["layers"]["dmi"] if x["op"] != "BURST"]
        print(f"    DMI got {len(got)} want {len(want)} ratio {SequenceMatcher(None, want, got, autojunk=False).ratio():.4f}",
              flush=True)
