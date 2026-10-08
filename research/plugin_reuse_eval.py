"""Reuse of the RateBlocks -> Chars stages by UART-like plugins (UART, LIN, DMX512).

    uv run python plugin_reuse_eval.py [n]
"""
import inspect
import sys
from collections import Counter
from difflib import SequenceMatcher

from wireskein._engine import plugins_uartlike as P
from wireskein._engine import synth, typed
from wireskein._engine.taxonomy import classify

n = int(sys.argv[1]) if len(sys.argv) > 1 else 60
res = Counter()
for s in range(n):
    cap, t = synth.scenario(s, "uartlike")
    tx = classify(cap)
    for b in t["buses"]:
        pin = b["roles"]["data"]
        rb = typed.rate_blocks(cap, tx.survey, pin)
        for blk, cands, ch in typed.block_chars(cap, tx.survey, rb):
            if ch is None:
                continue
            proto = b["protocol"]
            res[(proto, "cases")] += 1
            res[(proto, f"L={ch.bits}")] += 1
            res[(proto, "breaks>0")] += len(ch.breaks) > 0
            if proto == "lin":
                out = P.lin(ch)
                res[(proto, "exact")] += out["frames"] == b["expect"]["frames"]
                c = out["checks"]
                res[(proto, "all checks pass")] += c["frames"] > 0 and c["sync"] == c["pid_parity"] == c["checksum"] == c["frames"]
            elif proto == "dmx512":
                out = P.dmx512(ch)
                res[(proto, "exact")] += out["packets"] == b["expect"]["packets"]
                c = out["checks"]
                res[(proto, "all checks pass")] += c["packets"] > 0 and c["start_code_0"] == c["packets"]
            else:
                pm = b["params"]
                got = (ch.values[ch.ok] & ((1 << pm["data_bits"]) - 1)).tolist()
                res[(proto, "exact")] += SequenceMatcher(None, list(bytes.fromhex(b["expect"]["bytes"])), got, autojunk=False).ratio() >= 0.999
for k in sorted(res):
    print(k, res[k])
for f in (P._split_on_breaks, P.lin, P.dmx512):
    src = [l for l in inspect.getsource(f).splitlines() if l.strip() and not l.strip().startswith(('"""', "#"))]
    print(f"plugin code lines {f.__name__}: {len(src)}")
