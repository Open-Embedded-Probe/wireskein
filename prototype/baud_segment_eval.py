"""UART with mid-stream baud changes: segment the line, then decode per segment.

    PYTHONPATH=. uv run python baud_segment_eval.py [n] [--stress baudhop|none]
"""
import sys
from difflib import SequenceMatcher

import numpy as np

from wsproto import corpus, synth, typed
from wsproto.taxonomy import classify

n = int(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1].isdigit() else 60
stress = sys.argv[sys.argv.index("--stress") + 1] if "--stress" in sys.argv else "baudhop"
stress = None if stress == "none" else stress
cases = [synth.scenario(s, "mixed", stress) for s in range(n)] + ([] if stress else list(corpus.real_cases())[:4])
res, seg_ok, tot, flat = [], 0, 0, []
for cap, t in cases:
    for b in t["buses"]:
        if b["protocol"] != "uart" or "expect" not in b:
            continue
        tx = classify(cap)
        pin, pm = b["roles"]["data"], b["params"]
        L = 1 + pm["data_bits"] + (0 if pm["parity"] == "none" else 1) + int(pm["stop_bits"])
        segs = typed.async_segments(cap, tx.survey, pin)
        want = []
        for x in pm.get("baud_segments", [{"baud": pm["baud"]}]):
            u = cap.rate / x["baud"]
            if not want or abs(u / want[-1] - 1) > 0.05:
                want.append(u)
        tot += 1
        seg_ok += len(segs) == len(want) and all(abs(u / w - 1) < 0.05 for (_, _, u), w in zip(segs, want))
        vals = []
        for s0, s1, u in segs:
            ch = typed.chars(cap, typed.async_symbols_segment(cap, tx.survey, pin, s0, s1, u), L)
            vals += (ch.values[ch.ok] & ((1 << pm["data_bits"]) - 1)).tolist()
        wb = list(bytes.fromhex(b["expect"]["bytes"]))
        res.append(SequenceMatcher(None, wb, vals, autojunk=False).ratio())
        # reference: one unit for the whole line
        sym = typed.async_symbols(cap, tx.survey, pin)
        ch = typed.chars(cap, sym, L)
        flat.append(SequenceMatcher(None, wb, (ch.values[ch.ok] & ((1 << pm["data_bits"]) - 1)).tolist(), autojunk=False).ratio())
print(f"stress={stress}: segments right {seg_ok}/{tot}; segmented decode mean {np.mean(res):.4f} exact {sum(r >= 0.999 for r in res)}/{len(res)}"
      f"; single-unit decode mean {np.mean(flat):.4f} exact {sum(r >= 0.999 for r in flat)}/{len(flat)}")
