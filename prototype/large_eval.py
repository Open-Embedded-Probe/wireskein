"""Staged engine on the large real captures (flash_pattern4k, 0.9-1.6 M edges)."""
import time
from difflib import SequenceMatcher

from wsproto import corpus, fixture, staged
from wsproto.rvswd import DM_NAMES

inv = {v: k for k, v in DM_NAMES.items()}
for d in sorted(corpus.REAL.iterdir()):
    if "flash" not in d.name or d.name.startswith("i2cdb"):
        continue
    cap, t = fixture.load_capture(d), fixture.load_truth(d)
    t0 = time.perf_counter()
    r = staged.analyze(cap)
    dt = time.perf_counter() - t0
    edges = sum(len(c.edges) for c in cap.channels)
    print(f"== {d.name}: {edges} edges, {dt:.1f} s", {k: round(v, 1) for k, v in r.seconds.items()}, flush=True)
    print("   claims", [(c.verdict, c.protocol, round(float(c.total), 3)) for c in r.claims], flush=True)
    best = max([n for n in r.roots if n.analyzer in ("rvswd", "swio")], key=lambda n: n.total, default=None)
    if best is None:
        continue
    print("   ", best.analyzer, {k: (round(float(v), 3) if isinstance(v, float) else v) for k, v in best.metrics.items()}, flush=True)
    exp = t["buses"][0].get("expect", {})
    if "dmi" in exp:
        want = [(op, inv.get(nm, int(nm, 16) if nm.startswith("0x") else -1), dd) for op, nm, dd in exp["dmi"]]
        got = [(x["op"], x["addr"], x["data"]) for x in best.output.items if x["op"] != "BURST"]
        print(f"    DMI got {len(got)} want {len(want)} ratio {SequenceMatcher(None, want, got, autojunk=False).ratio():.4f}", flush=True)
