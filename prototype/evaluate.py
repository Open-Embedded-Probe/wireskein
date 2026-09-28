"""Evaluate the pipeline against ground truth (real fixtures + synthetic seeds).

    PYTHONPATH=. uv run python evaluate.py --synth 200 --tag baseline

Writes corpus/work/eval-<tag>.json with one record per case and prints a summary.
Also records, for every true bus, where its true hypothesis ranked at each layer,
to estimate what a pruning rule would have lost.
"""

from __future__ import annotations

import argparse
import sys
import json
import multiprocessing as mp
import time
from collections import Counter
from difflib import SequenceMatcher
from pathlib import Path

import numpy as np

from wsproto import corpus, pipeline, synth
from wsproto.stack import ProbeConfig
from wsproto.scoring import DefaultScorer, LayerOnlyScorer

SCORERS = {"default": DefaultScorer, "layer-only": LayerOnlyScorer}
IN_SCOPE = {"uart", "i2c", "spi", "lin", "dmx512", "rvswd", "swio"}
WORK = corpus.ROOT / "corpus/work"
MAX_EDGES = 400_000  # exhaustive expansion on multi-million-edge clocks takes hours; reported as skipped


def _ratio(a, b) -> float:
    if a == b:
        return 1.0
    return SequenceMatcher(None, a, b, autojunk=False).ratio()


def decoded(node) -> object:
    it = node.output.items if node.output else None
    if node.analyzer == "uart":
        return [int(x) for x in it["value"]]
    if node.analyzer == "i2c":
        return [(t.get("addr"), t.get("rw"), tuple(t.get("bytes", []))) for t in it if "addr" in t]
    if node.analyzer == "spi":
        return {ch: [bytes(int(v) for v in f).hex() for f in fr] for ch, fr in it["lines"].items()}
    return None


def match(bus: dict, node, rate: float) -> dict:
    """Compare one true bus with one hypothesis. Returns role/param/decode agreement."""
    r = {"roles": False, "params": False, "decode": 0.0}
    if node.analyzer != bus["protocol"] or node.output is None:
        return r
    tr, nr = bus["roles"], node.roles
    if bus["protocol"] == "uart":
        r["roles"] = nr["data"] == tr["data"]
        want = bytes.fromhex(bus["expect"]["bytes"]) if "expect" in bus else None
        got = decoded(node)
        pm = bus["params"]
        bauds = [x["baud"] for x in pm.get("baud_segments", [])] or [pm["baud"]]
        r["params"] = any(abs(node.params["baud"] / b - 1) <= 0.03 for b in bauds) and node.params["idle"] == pm["idle"]
        if want is not None:
            mask = 0x7F if pm["data_bits"] == 7 else 0xFF
            r["decode"] = _ratio([b & mask for b in want], [b & mask for b in got])
        else:
            r["decode"] = None
    elif bus["protocol"] == "i2c":
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        want = [(t["addr"], t["rw"], tuple(t["bytes"])) for t in bus["expect"]["transactions"]]
        r["decode"] = _ratio(want, decoded(node))
    elif bus["protocol"] == "swio":
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        if "expect" in bus and "dmi_addr" in bus["expect"]:
            want = [tuple(x) for x in bus["expect"]["dmi_addr"]]
            r["decode"] = _ratio(want, [(x["op"], x["addr"], x["data"]) for x in node.output.items])
        else:
            r["decode"] = None
    elif bus["protocol"] == "rvswd":
        r["roles"] = nr == tr
        r["params"] = r["roles"]
        if "expect" in bus and "dmi" in bus["expect"]:
            from wsproto.rvswd import DM_NAMES
            inv = {v: k for k, v in DM_NAMES.items()}
            addr = lambda name: inv.get(name, int(name, 16) if name.startswith("0x") else -1)  # noqa: E731
            want = [(op, addr(nm), d) for op, nm, d in bus["expect"]["dmi"]]
            got = [(x["op"], x["addr"], x["data"]) for x in node.output.items]
            r["decode"] = _ratio(want, got)
        else:
            r["decode"] = None
    elif bus["protocol"] == "spi":
        lines_true = {tr["mosi"], *([tr["miso"]] if "miso" in tr else [])}
        lines_got = {v for k, v in nr.items() if k.startswith("data")}
        r["roles"] = nr["clk"] == tr["clk"] and nr.get("cs") == tr.get("cs") and lines_got == lines_true
        r["params"] = node.metrics.get("mode") == bus["params"]["mode"]
        r["bit_order_ok"] = node.params["bit_order"] == bus["params"]["bit_order"]
        got = decoded(node)
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


def evaluate_case(args):
    kind, ref, scorer_name, use_probe, use_excl, engine = args
    if kind in ("real", "fixture"):
        from wsproto import fixture
        cap, truth = fixture.load_capture(ref), fixture.load_truth(ref)
    else:
        cap, truth = synth.scenario(*ref)
    n_edges = sum(len(c.edges) for c in cap.channels)
    if kind == "synth" and n_edges > MAX_EDGES and not use_probe and engine != "staged":
        return {"id": truth["id"], "skipped": f"{n_edges} edges > {MAX_EDGES}", "seconds": 0.0, "runs": 0,
                "n_active": 0, "buses": [], "claims": []}
    if engine in ("staged", "declarative"):
        from wsproto import staged
        staged.use_declarative(engine == "declarative")
        res = staged.analyze(cap)
        res.seconds = sum(res.seconds.values())
        res.probe_runs = res.abandoned = res.excluded = 0
    else:
        res = pipeline.analyze(cap, scorer=SCORERS[scorer_name](), probe=ProbeConfig() if use_probe else None,
                               exclude=use_excl)
    rec = {"id": truth["id"], "rate": cap.rate, "seconds": res.seconds, "runs": res.runs,
           "n_active": sum(1 for f in res.features.values() if not f.static), "buses": [], "claims": [],
           "probe_runs": res.probe_runs, "abandoned": res.abandoned, "excluded": res.excluded}
    claimed_correct = set()
    for bi, bus in enumerate(truth["buses"]):
        chans = bus_channels(bus)
        b = {"protocol": bus["protocol"], "in_scope": bus["protocol"] in IN_SCOPE, "outcome": "missed"}
        if all(len(cap.channel(ch).edges) == 0 for ch in chans):
            # nothing of this bus is in the capture (e.g. it ended before a mid-stream start)
            b["outcome"], b["in_scope"] = "absent", False
            rec["buses"].append(b)
            continue
        if b["in_scope"]:
            # where does the best true hypothesis sit among all roots on these channels?
            true_nodes = [n for n in res.roots if (m := match(bus, n, cap.rate))["roles"] and m["params"]]
            best_true = max(true_nodes, key=lambda n: n.total, default=None)
            b["true_total"] = best_true.total if best_true else None
            b["true_layer"] = best_true.layer_score if best_true else None
            if best_true is not None:
                rivals = [n for n in res.roots if set(n.roles.values()) & chans and n not in true_nodes]
                b["rank"] = 1 + sum(1 for n in rivals if n.total > best_true.total)
                b["best_rival"] = max((n.total for n in rivals), default=0.0)
            # L3 evidence for its channels (pruning study)
            f = res.features
            b["l3"] = {role: {k: round(v, 3) for k, v in f[ch].scores.items()} for role, ch in bus["roles"].items()}
            if "params" in bus and bus["protocol"] == "uart":
                want = cap.rate / bus["params"]["baud"]
                b["unit_rank"] = next((i for i, u in enumerate(f[bus["roles"]["data"]].units)
                                       if abs(u.samples / want - 1) <= 0.03), None)
        for ci, c in enumerate(res.claims):
            if not (set(c.roles.values()) & chans):
                continue
            if not b["in_scope"] and c.protocol == "sync_unknown" and set(bus["roles"].values()) <= set(c.roles.values()):
                b["outcome"] = "unknown-sync"
                b["frame_bits"] = c.node.metrics.get("frame_bits")
                claimed_correct.add(ci)
                continue
            m = match(bus, c.node, cap.rate) if b["in_scope"] else {"roles": False, "params": False}
            if m["roles"] and m["params"]:
                b["outcome"] = c.verdict
                b["decode"] = m["decode"]
                b["margin"] = c.margin
                b["bit_order_ok"] = m.get("bit_order_ok")
                claimed_correct.add(ci)
            elif b["outcome"] == "missed":
                b["outcome"] = "wrong"
                b["wrong_claim"] = {"label": c.node.label(), "roles": c.roles, "verdict": c.verdict}
        rec["buses"].append(b)
    decoy_ch = {d["channel"] for d in truth.get("decoys", [])}
    for ci, c in enumerate(res.claims):
        rec["claims"].append({
            "protocol": c.protocol, "roles": c.roles, "label": c.node.label(), "verdict": c.verdict,
            "total": round(c.total, 4), "margin": round(c.margin, 4), "correct": ci in claimed_correct,
            "on_decoy": bool(set(c.roles.values()) & decoy_ch),
            "runner_up": c.runner_up.label() if c.runner_up else None,
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
    ap.add_argument("--scorer", default="default", choices=sorted(SCORERS))
    ap.add_argument("--tag", default="run")
    ap.add_argument("--probe", action="store_true", help="early abandonment on probe windows")
    ap.add_argument("--exclude", action="store_true", help="safe (definitional) exclusion rules before decoding")
    ap.add_argument("--engine", default="flat", choices=["flat", "staged", "declarative"])
    ap.add_argument("--set", default=None, help="frozen fixture set under corpus/fixtures/synth (replaces --synth)")
    ap.add_argument("-j", type=int, default=max(1, mp.cpu_count() - 2))
    args = ap.parse_args()
    jobs = []
    if not args.no_real:
        for d in sorted(corpus.REAL.iterdir()):
            if args.large or "flash" not in d.name or d.name.startswith("i2cdb"):
                jobs.append(("real", d, args.scorer, args.probe, args.exclude, args.engine))
    if args.set:
        base = corpus.ROOT / "corpus/fixtures/synth" / args.set
        jobs += [("fixture", d, args.scorer, args.probe, args.exclude, args.engine) for d in sorted(base.iterdir())]
    else:
        jobs += [("synth", (s, args.profile, args.stress), args.scorer, args.probe, args.exclude, args.engine)
                 for s in range(args.start, args.start + args.synth)]
    t0 = time.time()
    records = []
    with mp.Pool(args.j) as pool:
        for rec in pool.imap_unordered(evaluate_case, jobs, chunksize=1):
            records.append(rec)
            print(f"[{len(records)}/{len(jobs)}] {rec['id']} {rec['seconds']:.1f}s runs={rec['runs']} active={rec['n_active']}",
                  file=sys.stderr, flush=True)
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
