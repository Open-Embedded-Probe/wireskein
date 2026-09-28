"""M7 step 1-2: where does the time go, and what crosses the core/plugin boundary?

    PYTHONPATH=. uv run python boundary_probe.py [--probe] [seeds...]
Writes ../corpus/work/boundary-<probe|full>.json
"""

import json
import sys
import time

from wsproto import corpus, kernels, pipeline, synth
from wsproto.stack import ProbeConfig

use_probe = "--probe" in sys.argv
seeds = [int(a) for a in sys.argv[1:] if a.isdigit()] or list(range(20))
cases = list(corpus.real_cases())[:5] + [synth.scenario(s) for s in seeds]
agg, kagg, total = {}, {}, {"wall": 0.0, "features": 0.0, "pack": 0.0, "score": 0.0, "score_calls": 0}
for cap, truth in cases:
    kernels.reset()
    t0 = time.perf_counter()
    r = pipeline.analyze(cap, probe=ProbeConfig() if use_probe else None)
    total["wall"] += time.perf_counter() - t0
    e = r.engine
    total["score"] += e.score_seconds
    total["score_calls"] += e.score_calls
    for k, v in e.stats.items():
        a = agg.setdefault(k, [0, 0.0, 0.0, 0, 0])
        for i in range(5):
            a[i] += v[i]
    for k, v in kernels.STATS.items():
        a = kagg.setdefault(k, [0, 0.0, 0, 0])
        for i in range(4):
            a[i] += v[i]
print(f"{len(cases)} cases, wall {total['wall']:.1f}s, scorer {total['score_calls']} calls {total['score']:.2f}s")
print(f"{'analyzer':18} {'calls':>8} {'wall s':>8} {'kernel s':>9} {'glue s':>8} {'glue/call us':>12} {'in/call':>9} {'out/call':>9}")
for k, (c, w, ks, i, o) in sorted(agg.items(), key=lambda x: -x[1][1]):
    print(f"{k:18} {c:8d} {w:8.2f} {ks:9.2f} {w - ks:8.2f} {(w - ks) / c * 1e6:12.1f} {i / c:9.0f} {o / c:9.0f}")
print(f"\n{'kernel':18} {'calls':>8} {'s':>8} {'us/call':>8} {'in/call':>9} {'out/call':>9}")
for k, (c, t, i, o) in sorted(kagg.items(), key=lambda x: -x[1][1]):
    print(f"{k:18} {c:8d} {t:8.2f} {t / c * 1e6:8.1f} {i / c:9.0f} {o / c:9.0f}")
out = {"cases": len(cases), "probe": use_probe, "total": total, "analyzers": agg, "kernels": kagg}
(corpus.ROOT / f"corpus/work/boundary-{'probe' if use_probe else 'full'}.json").write_text(json.dumps(out, indent=1))
