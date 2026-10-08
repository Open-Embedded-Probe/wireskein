"""Measure the staged pipeline (T1..T5) along each true bus: does every stage
shrink the information handled, is the typed result still correct, and how
long does each stage take?

    uv run python stage_eval.py [n_synth]
"""

import sys
import time
from collections import defaultdict
from difflib import SequenceMatcher

import numpy as np

import corpus
from wireskein._engine import synth, typed
from wireskein._engine.taxonomy import classify

TRUE_CLASS = {("uart", "data"): "data", ("i2c", "scl"): "clock", ("i2c", "sda"): "data", ("spi", "clk"): "clock",
              ("spi", "mosi"): "data", ("spi", "miso"): "data", ("spi", "cs"): "sparse",
              ("rvswd", "clk"): "clock", ("rvswd", "dio"): "data", ("swio", "dio"): "pulse"}
DECOY_CLASS = {"pwm": "clock", "clock": "clock", "burst_clock": "clock", "random": "data"}


def ratio(a, b):
    return 1.0 if a == b else SequenceMatcher(None, a, b, autojunk=False).ratio()


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 60
    stress = sys.argv[sys.argv.index("--stress") + 1] if "--stress" in sys.argv else None
    real = [] if stress else list(corpus.real_cases())
    cases = real + [synth.scenario(s, "mixed", stress) for s in range(n)]
    agg = defaultdict(list)          # (protocol, stage) -> bytes
    tm = defaultdict(float)
    pin_ok = pin_n = pin_top = 0
    group_ok = defaultdict(lambda: [0, 0])
    decode = defaultdict(list)
    for cap, truth in cases:
        tx = classify(cap)
        for k, v in tx.seconds.items():
            tm[k] += v
        # T1 accuracy
        want = {}
        for b in truth["buses"]:
            for role, ch in b["roles"].items():
                want[ch] = TRUE_CLASS.get((b["protocol"], role))
        for d in truth.get("decoys", []):
            want[d["channel"]] = DECOY_CLASS.get(d["kind"])
        for ch, w in want.items():
            if w is None or ch not in tx.pins:
                continue
            pin_n += 1
            pin_ok += w in tx.pins[ch].candidates
            sc = tx.pins[ch].scores
            pin_top += max(sc, key=sc.get) == w
        for b in truth["buses"]:
            proto, r = b["protocol"], b["roles"]
            chans = set(r.values())
            agg[(proto, "0 edges(bus pins)")].append(sum(len(cap.channel(c).edges) for c in chans) * 8)
            if proto in ("i2c", "spi", "rvswd"):
                clk = r.get("scl") or r.get("clk")
                data = tuple(sorted(x for k, x in r.items() if k in ("sda", "mosi", "miso", "dio")))
                sel = r.get("cs")
                g = [x for x in tx.groups if x.kind == "sync" and x.clock == clk and x.select == sel]
                exact = any(set(x.data) == set(data) for x in g)
                group_ok[proto][0] += exact
                group_ok[proto][1] += 1
                if clk not in tx.survey.clocks:
                    continue
                t0 = time.perf_counter()
                edges = typed.sample_edge_for(cap, clk, data)
                agg[(proto, "edge candidates")].append(len(edges))
                sb = typed.sync_bits(cap, tx.survey, clk, data, sel, edges[0])
                if proto == "i2c" and len(edges) > 1:
                    # sibling SyncBits: the I2C stage picks the one whose ACK slots are mostly ACK
                    def ack_rate(e):
                        s2 = typed.sync_bits(cap, tx.survey, clk, data, sel, e)
                        f2 = typed.frames_startstop(cap, s2)
                        w2 = typed.words(f2, 9)
                        return float(np.mean((w2.values & 1) == 0)) if len(w2.values) else 0.0, s2
                    sb = max((ack_rate(e) for e in edges), key=lambda x: x[0])[1]
                agg[(proto, "2 SyncBits")].append(sb.nbytes())
                agg[(proto, "rate segments")].append(len(typed.clock_rate_segments(sb).segments))
                if proto == "spi":
                    fr = typed.frames_select(cap, tx.survey, sb) if sel else typed.frames_gap(sb)
                    agg[(proto, "3 Frames")].append(fr.nbytes())
                    rows = {d: i for i, d in enumerate(sb.data)}
                    got_all = []
                    for role in ("mosi", "miso"):
                        if role not in r:
                            continue
                        w = typed.words(fr, 8, rows[r[role]])
                        agg[(proto, f"4 Words({role})")].append(w.nbytes())
                        vals = w.values if b["params"]["bit_order"] == "msb" else np.array(
                            [int(f"{int(v):08b}"[::-1], 2) for v in w.values])
                        got = [bytes(int(x) for x in vals[w.frame_of == i]).hex() for i in range(len(fr.bounds))]
                        decode[proto].append(ratio(b["expect"][role], got))
                        got_all.append(sum(len(x) // 2 for x in got))
                    agg[(proto, "5 bytes")].append(sum(got_all))
                elif proto in ("i2c", "rvswd"):
                    fr = typed.frames_startstop(cap, sb)
                    agg[(proto, "3 Frames")].append(fr.nbytes())
                    if proto == "i2c":
                        w = typed.words(fr, 9)
                        agg[(proto, "4 Words(9)")].append(w.nbytes())
                        txs = typed.i2c_from_words(fr, w)
                        agg[(proto, "5 transactions")].append(sum(2 + len(x["bytes"]) for x in txs))
                        want_tx = [(x["addr"], x["rw"], tuple(x["bytes"])) for x in b["expect"]["transactions"]]
                        decode[proto].append(ratio(want_tx, [(x["addr"], x["rw"], tuple(x["bytes"])) for x in txs]))
                    else:
                        lens = np.diff(fr.bounds, axis=1).ravel()
                        vals, cnt = np.unique(lens, return_counts=True)
                        decode["rvswd frame bits (top)"].append(vals[np.argsort(cnt)[::-1][:4]].tolist())
                tm["path-sync"] += time.perf_counter() - t0
            elif proto == "uart":
                pin = r["data"]
                t0 = time.perf_counter()
                rb = typed.rate_blocks(cap, tx.survey, pin)
                agg[(proto, "2 RateBlocks")].append(rb.nbytes())
                agg[(proto, "blocks")].append(len(rb.blocks))
                pm = b["params"]
                L_true = 1 + pm["data_bits"] + (0 if pm["parity"] == "none" else 1) + int(pm["stop_bits"])
                vals, nb = [], 0
                for blk, cands, ch in typed.block_chars(cap, tx.survey, rb):
                    if ch is None:
                        continue
                    agg[(proto, "L estimate ok")].append(int(ch.bits in (L_true, L_true - 1)))
                    nb += ch.nbytes()
                    vals += (ch.values[ch.ok] & ((1 << pm["data_bits"]) - 1)).tolist()
                agg[(proto, "4 Chars")].append(nb)
                agg[(proto, "5 bytes")].append(len(vals))
                if "expect" in b:
                    decode[proto].append(ratio(list(bytes.fromhex(b["expect"]["bytes"])), vals))
                tm["path-uart"] += time.perf_counter() - t0
    print(f"{len(cases)} cases")
    print(f"T1 pin class: true class among candidates {pin_ok}/{pin_n}, top-1 {pin_top}/{pin_n}")
    for p, (ok, tot) in group_ok.items():
        print(f"T2 true group found exactly [{p}]: {ok}/{tot}")
    print("\nmedian bytes per bus along the true path (and ratio to the previous stage):")
    for proto in ("uart", "i2c", "spi", "rvswd"):
        stages = sorted(k[1] for k in agg if k[0] == proto and k[1][0].isdigit())
        prev = None
        line = []
        for s in stages:
            m = float(np.median(agg[(proto, s)]))
            line.append(f"{s}={m:.0f}" + (f" (x{m / prev:.2f})" if prev else ""))
            prev = m if not s.startswith("4 Words(miso") else prev
        print(f"  {proto:6} " + "  ".join(line))
    for proto in ("i2c", "spi", "rvswd"):
        e = agg.get((proto, "edge candidates"), [])
        if e:
            print(f"  sampling-edge siblings [{proto}]: {sum(x > 1 for x in e)}/{len(e)} buses kept both edges")
    for proto in ("uart", "i2c", "spi", "rvswd"):
        for key in ("blocks", "rate segments", "L estimate ok"):
            v = agg.get((proto, key))
            if v:
                print(f"  {proto} {key}: " + (f"{sum(v)}/{len(v)}" if key == "L estimate ok" else
                      f"1 block/segment {sum(x == 1 for x in v)}/{len(v)}, more {sum(x > 1 for x in v)}"))
    print("\ndecode agreement with ground truth:")
    for k, v in decode.items():
        if k.startswith("rvswd"):
            print(f"  {k}: {v}")
        else:
            vv = [x for x in v if x is not None]
            print(f"  {k}: mean {np.mean(vv):.4f}, exact {sum(x >= 0.999 for x in vv)}/{len(vv)}")
    print("\nseconds:", {k: round(v, 2) for k, v in tm.items()})


if __name__ == "__main__":
    main()
