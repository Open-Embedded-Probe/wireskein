"""Evaluate what analyze claims against the ground truth (the real records, a frozen set, or seeded scenarios).

    uv run python evaluate.py --set heldout --tag NAME

Writes work/eval-<tag>.json with one record per case and prints a summary. Reads only what analyze exports
(wireskein.decode.analyze and export, mode "all"): each claim's roles, verdict, params, evidence and layers.
"""

from __future__ import annotations

import argparse
import sys
import json
import multiprocessing as mp
import time
from collections import Counter
from difflib import SequenceMatcher

import numpy as np

from wireskein import decode

import corpus
import synth

IN_SCOPE = {"uart", "i2c", "spi", "lin", "dmx512", "rvswd", "swio", "swd", "can"}
WORK = corpus.WORK


def _ratio(a, b) -> float:
    if a == b:
        return 1.0
    return SequenceMatcher(None, a, b, autojunk=False).ratio()


def decoded(claim: dict) -> object:
    lay = claim["layers"]
    if claim["protocol"] == "uart":
        return list(bytes.fromhex(lay["bytes"]["hex"]))
    if claim["protocol"] == "i2c":
        return [(t["addr"], t["rw"], tuple(bytes.fromhex(t["bytes"]))) for t in lay["transactions"]]
    if claim["protocol"] == "spi":
        pins = [v for k, v in claim["roles"].items() if k.startswith("data")]
        return {ch: [t.get(ch, "") for t in lay["transfers"]] for ch in pins}
    return None


def match(bus: dict, claim: dict) -> dict:
    """Compare one true bus with one claim (analyze's export, mode "all"). Returns role/param/decode agreement."""
    r = {"roles": False, "params": False, "decode": 0.0}
    if claim["protocol"] != bus["protocol"]:
        return r
    tr, nr, lay, pm = bus["roles"], claim["roles"], claim["layers"], claim["params"]
    if bus["protocol"] == "uart":
        r["roles"] = nr["data"] == tr["data"]
        want = bytes.fromhex(bus["expect"]["bytes"]) if "expect" in bus else None
        got = decoded(claim)
        tp = bus["params"]
        bauds = [x["baud"] for x in tp.get("baud_segments", [])] or [tp["baud"]]
        r["params"] = any(abs(pm["baud"] / b - 1) <= 0.03 for b in bauds) and pm["idle"] == tp["idle"]
        if want is not None:
            mask = 0x7F if tp["data_bits"] == 7 else 0xFF
            r["decode"] = _ratio([b & mask for b in want], [b & mask for b in got])
        else:
            r["decode"] = None
    elif bus["protocol"] == "i2c":
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        want = [(t["addr"], t["rw"], tuple(t["bytes"])) for t in bus["expect"]["transactions"]]
        r["decode"] = _ratio(want, decoded(claim))
    elif bus["protocol"] in ("swio", "rvswd"):
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        got = [(x["op"], x["addr"], x["data"]) for x in lay["dmi"]]
        if bus["protocol"] == "swio" and "expect" in bus and "dmi_addr" in bus["expect"]:
            r["decode"] = _ratio([tuple(x) for x in bus["expect"]["dmi_addr"]], got)
        elif bus["protocol"] == "rvswd" and "expect" in bus and "dmi" in bus["expect"]:
            names = {x["reg"]: x["addr"] for x in lay["dmi"] if x.get("reg")}
            addr = lambda name: names.get(name, int(name, 16) if name.startswith("0x") else -1)  # noqa: E731
            r["decode"] = _ratio([(op, addr(nm), d) for op, nm, d in bus["expect"]["dmi"]], got)
        else:
            r["decode"] = None
    elif bus["protocol"] in ("lin", "dmx512"):
        r["roles"] = nr.get("data") == tr["data"]
        r["params"] = abs(pm["baud"] / bus["params"]["baud"] - 1) <= 0.03
        if bus["protocol"] == "lin":
            r["decode"] = _ratio([tuple(f) for f in bus["expect"]["frames"]], [tuple(f) for f in lay["frames"]])
        else:
            r["decode"] = _ratio(bus["expect"]["packets"], lay["packets"])
    elif bus["protocol"] == "swd":
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        ack = {1: "OK", 2: "WAIT", 4: "FAULT"}
        want = [("AP" if a else "DP", "R" if w else "W", x, ack[k], d) for a, w, x, k, d in bus["expect"]["packets"]]
        got = [(p["apndp"], p["rnw"], p["a"], p["ack"], p.get("data")) for p in lay["packets"]]
        r["decode"] = _ratio(want, got)
    elif bus["protocol"] == "can":
        r["roles"] = nr.get("data") == tr["data"]
        r["params"] = abs(pm["bitrate"] / bus["params"]["bitrate"] - 1) <= 0.03
        want = [(i, bool(e), d) for i, e, d in bus["expect"]["frames"]]
        got = [(f["id"], bool(f["ide"]), bytes.fromhex(f"{f['data']:0{2 * f['dlc']}x}").hex() if f["dlc"] else "")
               for f in lay["frames"] if f["ok"]]
        r["decode"] = _ratio(want, got)
    elif bus["protocol"] == "spi":
        lines_true = {tr["mosi"], *([tr["miso"]] if "miso" in tr else [])}
        lines_got = {v for k, v in nr.items() if k.startswith("data")}
        r["roles"] = nr["clk"] == tr["clk"] and nr.get("cs") == tr.get("cs") and lines_got == lines_true
        r["params"] = claim["evidence"].get("mode") == bus["params"]["mode"]
        r["bit_order_ok"] = pm["bit_order"] == bus["params"]["bit_order"]
        got = decoded(claim)
        want = {tr["mosi"]: bus["expect"]["mosi"]}
        if "miso" in tr:
            want[tr["miso"]] = bus["expect"]["miso"]
        scores = []
        for ch, frames in want.items():
            g = got.get(ch)
            if g is None:
                scores.append(0.0)
                continue
            if not r["bit_order_ok"]:
                g = [bytes(int(f"{b:08b}"[::-1], 2) for b in bytes.fromhex(x)).hex() for x in g]
            scores.append(_ratio(frames, g))
        r["decode"] = float(np.mean(scores))
    return r


def bus_channels(bus):
    return set(bus["roles"].values())


def _label(c: dict) -> str:
    return f"{c['protocol']}({', '.join(f'{k}={v}' for k, v in sorted(c['roles'].items()))})"


def evaluate_case(args):
    kind, ref = args
    cap, truth = corpus.load(ref) if kind == "file" else synth.scenario(*ref)
    t0 = time.perf_counter()
    res = decode.analyze(cap)
    seconds = time.perf_counter() - t0
    claims = decode.export(res, cap, mode="all", alternatives=True)["claims"]
    rec = {"id": truth["id"], "rate": cap.rate, "seconds": seconds, "buses": [], "claims": []}
    claimed_correct = set()
    for bus in truth["buses"]:
        chans = bus_channels(bus)
        b = {"protocol": bus["protocol"], "in_scope": bus["protocol"] in IN_SCOPE, "outcome": "missed"}
        if all(len(cap.channel(ch).edges) == 0 for ch in chans):
            # nothing of this bus is in the capture (e.g. it ended before a mid-stream start)
            b["outcome"], b["in_scope"] = "absent", False
            rec["buses"].append(b)
            continue
        for ci, c in enumerate(claims):
            if not (set(c["roles"].values()) & chans):
                continue
            if not b["in_scope"] and c["protocol"] == "sync_unknown" and chans <= set(c["roles"].values()):
                b["outcome"] = "unknown-sync"
                b["frame_bits"] = c["evidence"].get("frame_bits")
                claimed_correct.add(ci)
                continue
            m = match(bus, c) if b["in_scope"] else {"roles": False, "params": False}
            if m["roles"] and m["params"]:
                b["outcome"] = c["verdict"]
                b["decode"] = m["decode"]
                b["margin"] = c["margin"]
                b["bit_order_ok"] = m.get("bit_order_ok")
                claimed_correct.add(ci)
            elif b["outcome"] == "missed":
                b["outcome"] = "wrong"
                b["wrong_claim"] = {"label": _label(c), "roles": c["roles"], "verdict": c["verdict"]}
        rec["buses"].append(b)
    decoy_ch = {d["channel"] for d in truth.get("decoys", [])}
    for ci, c in enumerate(claims):
        rec["claims"].append({
            "protocol": c["protocol"], "roles": c["roles"], "label": _label(c), "verdict": c["verdict"],
            "total": c["score"], "margin": c["margin"], "correct": ci in claimed_correct,
            "on_decoy": bool(set(c["roles"].values()) & decoy_ch),
            "runner_up": _label(c["runner_up"]) if c.get("runner_up") else None,
        })
    return rec


def summarize(records: list[dict]) -> dict:
    s = Counter()
    per_proto = {}
    for r in records:
        for b in r["buses"]:
            if b["outcome"] == "absent":
                continue
            key = b["protocol"] if b["in_scope"] else "out-of-scope"
            per_proto.setdefault(key, Counter())[b["outcome"]] += 1
            if b["in_scope"] and b["outcome"] in ("confirmed", "likely"):
                s["decode_sum"] += b.get("decode") or 0
                s["decode_n"] += b.get("decode") is not None
                s["decode_exact"] += (b.get("decode") or 0) >= 0.999
        for c in r["claims"]:
            s["claims"] += 1
            s[f"claims_{c['verdict']}"] += 1
            if not c["correct"]:
                s[f"false_{c['verdict']}"] += 1
    n_bus = sum(sum(v.values()) for k, v in per_proto.items() if k != "out-of-scope")
    conf_ok = sum(v["confirmed"] for k, v in per_proto.items() if k != "out-of-scope")
    likely_ok = sum(v["likely"] for k, v in per_proto.items() if k != "out-of-scope")
    return {
        "cases": len(records),
        "skipped": [r["id"] for r in records if r.get("skipped")],
        "buses_in_scope": n_bus,
        "per_protocol": {k: dict(v) for k, v in per_proto.items()},
        "confirmed_correct_rate": conf_ok / n_bus if n_bus else None,
        "confirmed_or_likely_rate": (conf_ok + likely_ok) / n_bus if n_bus else None,
        "false_confirm_rate": s["false_confirmed"] / s["claims_confirmed"] if s["claims_confirmed"] else 0.0,
        "false_confirmed": s["false_confirmed"],
        "false_likely": s["false_likely"],
        "claims": dict((k, v) for k, v in s.items() if k.startswith("claims")),
        "decode_mean": s["decode_sum"] / s["decode_n"] if s["decode_n"] else None,
        "decode_exact": s["decode_exact"],
        "seconds_total": sum(r["seconds"] for r in records),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--synth", type=int, default=100)
    ap.add_argument("--start", type=int, default=0)
    ap.add_argument("--profile", default="mixed")
    ap.add_argument("--stress", default=None)
    ap.add_argument("--no-real", action="store_true")
    ap.add_argument("--large", action="store_true", help="include large real captures")
    ap.add_argument("--tag", default="run")
    ap.add_argument("--set", default=None, help="a frozen set under corpus/synth (replaces --synth)")
    ap.add_argument("-j", type=int, default=max(1, mp.cpu_count() - 2))
    args = ap.parse_args()
    jobs = []
    if not args.no_real:
        jobs += [("file", p) for p in corpus.cases(corpus.REAL) if args.large or not corpus.large(p)]
    if args.set:
        jobs += [("file", p) for p in corpus.cases(corpus.SETS / args.set)]
    else:
        jobs += [("synth", (s, args.profile, args.stress))
                 for s in range(args.start, args.start + args.synth)]
    t0 = time.time()
    records = []
    with mp.Pool(args.j) as pool:
        for rec in pool.imap_unordered(evaluate_case, jobs, chunksize=1):
            records.append(rec)
            print(f"[{len(records)}/{len(jobs)}] {rec['id']} {rec['seconds']:.1f}s", file=sys.stderr, flush=True)
    records.sort(key=lambda r: r["id"])
    summary = summarize(records)
    summary["wall_seconds"] = time.time() - t0
    summary["args"] = {k: v for k, v in vars(args).items()}
    WORK.mkdir(parents=True, exist_ok=True)
    out = WORK / f"eval-{args.tag}.json"
    out.write_text(json.dumps({"summary": summary, "records": records}, indent=1, default=str))
    print(json.dumps(summary, indent=1, default=str))
    print("written", out)


if __name__ == "__main__":
    main()
