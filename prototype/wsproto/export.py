"""Result export for different consumers.

    final   the top interpretation of every claim only (CLI, scripts, AI)
    all     every layer of every claim down to sample positions (GUI annotations)
    select  only the layers/fields named by paths such as "i2c.transactions",
            "uart.lines", "*.final", "spi.frames", "rvswd.dm"

Times are given in samples ("s", "e") and the capture's sample rate is in the
header, so a consumer can convert without re-reading the capture.
"""

from __future__ import annotations

import fnmatch
import json

import numpy as np

from . import typed
from .rvswd import DM_NAMES


def _hex(vals) -> str:
    return bytes(int(v) & 0xFF for v in vals).hex()


# ---------------- layer serializers (claim node -> {layer: data}) ----------------

def _sync_layers(n) -> dict:
    lay = getattr(n, "layers", None) or {}
    sb, fr = lay.get("bits"), lay.get("frames")
    out = {}
    if sb is not None:
        out["bits"] = {"clock": sb.clock, "data": list(sb.data), "sample_edge": sb.sample_edge,
                       "t": sb.t.tolist(), "values": sb.bits.tolist(),
                       "deglitch": getattr(sb, "deglitched", 0)}
    if fr is not None:
        t = sb.t
        out["frames"] = [{"s": int(t[a]), "e": int(t[b - 1]), "bits": int(b - a)} for a, b in fr.bounds.tolist() if b > a]
        if fr.events:
            out["conditions"] = [{"kind": k, "s": int(x)} for k, x in fr.events]
    return out


def _words(n, size: int) -> list[dict]:
    lay = n.layers
    fr, sb = lay["frames"], lay["bits"]
    rows = []
    for row, pin in enumerate(sb.data):
        w = typed.words(fr, size, row)
        for i, (v, f) in enumerate(zip(w.values.tolist(), w.frame_of.tolist())):
            a = fr.bounds[f][0]
            k = int(np.sum(w.frame_of[:i] == f))
            rows.append({"pin": pin, "s": int(sb.t[a + k * size]), "e": int(sb.t[min(a + (k + 1) * size, len(sb.t)) - 1]),
                         "value": int(v)})
    return rows


def layers_of(n) -> dict:
    """All layers of a claim, lowest first, as plain data."""
    a = n.analyzer
    out = {}
    if a in ("i2c", "spi", "sync_unknown", "rvswd"):
        out.update(_sync_layers(n))
    if a == "i2c":
        out["words"] = _words(n, 9)
        out["transactions"] = [{"s": t.get("start"), "e": t.get("end"), "addr": t.get("addr"), "rw": t.get("rw"),
                                "addr_ack": t.get("addr_ack"), "bytes": _hex(t.get("bytes", [])),
                                "acks": [bool(x) for x in t.get("acks", [])]} for t in n.output.items if "addr" in t]
    elif a == "spi":
        out["words"] = _words(n, 8)
        fr, sb = n.layers["frames"], n.layers["bits"]
        tr = []
        for i, (x, y) in enumerate(fr.bounds.tolist()):
            if y <= x:
                continue
            rec = {"s": int(sb.t[x]), "e": int(sb.t[y - 1])}
            for pin, frames in n.output.items["lines"].items():
                rec[pin] = _hex(frames[i]) if i < len(frames) else ""
            tr.append(rec)
        out["transfers"] = tr
    elif a == "sync_unknown":
        out["summary"] = {"frame_bits": n.metrics.get("frame_bits"), "delimiter": n.metrics.get("delimiter")}
    elif a in ("rvswd", "swio"):
        if a == "swio" and getattr(n, "layers", None):
            ps = n.layers["pulses"]
            out["pulses"] = {"s": ps.start.tolist(), "width": ps.width.tolist(), "short": ps.short.tolist(),
                             "split": ps.split}
        out["dmi"] = [{"s": x["t"], "op": x["op"], "addr": x["addr"], "reg": DM_NAMES.get(x["addr"]) if x["addr"] is not None else None,
                       "data": x["data"], "ok": x.get("ok", True)} for x in n.output.items]
        dm = [ch for ch in n.children if ch.analyzer == "riscv_dm"]
        if dm:
            out["dm"] = [{"op": k, "addr": ad, "value": v} for k, ad, v in dm[0].output.items]
    elif a in ("uart", "lin", "dmx512"):
        lay = getattr(n, "layers", None) or {}
        if "blocks" in lay:
            out["blocks"] = [{"s": s0, "e": s1, "unit_samples": u} for s0, s1, u in lay["blocks"].blocks]
            chars = []
            for (s0, s1, u), cands, ch in lay["chars"]:
                if ch is None:
                    continue
                chars += [{"s": int(st), "value": int(v), "ok": bool(ok)} for st, v, ok in zip(ch.start.tolist(), ch.values.tolist(), ch.ok.tolist())]
                out.setdefault("breaks", []).extend(int(b) for b in ch.breaks.tolist())
            out["chars"] = chars
        if a == "uart":
            it = n.output.items
            out["bytes"] = {"s": np.asarray(it["start"]).tolist(), "hex": _hex(it["value"])}
            for ch in n.children:
                if ch.output is None or ch.total < 0.3:
                    continue
                if ch.analyzer == "lines":
                    out["lines"] = list(ch.output.items)
                elif ch.analyzer in ("nmea", "modbus_rtu"):
                    out[ch.analyzer] = list(ch.output.items)
        elif a == "lin":
            out["frames"] = n.output.items
        else:
            out["packets"] = n.output.items
    return out


FINAL = {"i2c": "transactions", "spi": "transfers", "uart": ("lines", "nmea", "modbus_rtu", "bytes"),
         "lin": "frames", "dmx512": "packets", "rvswd": ("dm", "dmi"), "swio": ("dm", "dmi"), "sync_unknown": "summary"}


def final_of(n, lay: dict) -> dict:
    want = FINAL.get(n.analyzer, ())
    want = (want,) if isinstance(want, str) else want
    for k in want:
        if lay.get(k):
            return {k: lay[k]}
    return {}


def _window(lay: dict, s0: int, s1: int) -> dict:
    """Keep only the items inside [s0, s1): what a GUI needs for the visible range."""
    out = {}
    for k, v in lay.items():
        if isinstance(v, list) and v and isinstance(v[0], dict) and "s" in v[0]:
            out[k] = [x for x in v if x.get("s") is not None and s0 <= x["s"] < s1]
        elif isinstance(v, dict) and "t" in v and isinstance(v["t"], list):
            t = np.asarray(v["t"])
            sel = (t >= s0) & (t < s1)
            w = dict(v)
            w["t"] = t[sel].tolist()
            if "values" in v:
                w["values"] = [list(np.asarray(row)[sel]) for row in v["values"]]
            out[k] = w
        elif isinstance(v, dict) and "s" in v and isinstance(v["s"], list):
            st = np.asarray(v["s"])
            sel = (st >= s0) & (st < s1)
            w = {kk: (list(np.asarray(vv)[sel]) if isinstance(vv, list) and len(vv) == len(st) else vv) for kk, vv in v.items()}
            if "hex" in v:
                b = bytes.fromhex(v["hex"])
                w["hex"] = bytes(x for x, m in zip(b, sel) if m).hex()
            out[k] = w
        else:
            out[k] = v
    return out


DEPTHS = ("transport", "frames", "protocol", "device")
LOWER = {"i2c": "transactions", "spi": "transfers", "uart": "bytes", "lin": "frames", "dmx512": "packets",
         "rvswd": "dmi", "swio": "dmi", "sync_unknown": "summary"}


def devices_of(n, with_candidates: bool) -> list[dict]:
    out = []
    for m in getattr(n, "devices", []) or []:
        if m.level in ("identified", "consistent") or (with_candidates and m.level == "address_only"):
            out.append({"device": m.device, "level": m.level, "evidence": m.evidence,
                        **({"messages": m.messages} if m.level != "address_only" else {})})
    return out


def export(res, cap, mode: str = "final", select: list[str] | None = None, alternatives: bool = False,
           window: tuple[int, int] | None = None, depth: str = "protocol") -> dict:
    doc = {"rate": cap.rate, "n_samples": cap.n_samples, "claims": []}
    if window:
        doc["window"] = list(window)
    for c in res.claims:
        n = c.node
        rec = {"protocol": c.protocol, "roles": c.roles, "verdict": c.verdict, "score": round(float(c.total), 4),
               "margin": round(float(c.margin), 4),
               "params": {k: (float(v) if isinstance(v, (np.floating, float)) else v) for k, v in c.params.items()}}
        lay = layers_of(n)
        if window:
            lay = _window(lay, *window)
        if mode == "final":
            # depth: how far up the interpretation goes; lower layers stay the reference
            if depth == "transport":
                pass
            elif depth == "frames":
                k = LOWER.get(n.analyzer)
                if k and k in lay:
                    rec["result"] = {k: lay[k]}
            else:
                rec["result"] = final_of(n, lay)
                if depth == "device":
                    ds = devices_of(n, with_candidates=True)
                    if ds:
                        rec["devices"] = ds
        elif mode == "all":
            rec["layers"] = lay
            rec["evidence"] = {k: (float(v) if isinstance(v, (np.floating, float, np.integer)) else v)
                               for k, v in n.metrics.items() if not isinstance(v, (dict, list))}
            if alternatives and c.runner_up is not None:
                rec["runner_up"] = {"protocol": c.runner_up.analyzer, "roles": c.runner_up.roles,
                                    "score": round(float(c.runner_up.total), 4)}
        elif mode == "select":
            picked = {}
            for path in select or []:
                proto, _, layer = path.partition(".")
                if not fnmatch.fnmatch(c.protocol, proto):
                    continue
                if layer == "final":
                    picked.update(final_of(n, lay))
                else:
                    for k in fnmatch.filter(lay.keys(), layer or "*"):
                        picked[k] = lay[k]
            if not picked:
                continue
            rec["result"] = picked
        doc["claims"].append(rec)
    if hasattr(res, "relations"):
        rel = []
        for r in res.relations:
            sc = [ch for ch in r.children if ch.analyzer == "scpi"]
            if sc and (mode != "select" or any(fnmatch.fnmatch("scpi", p.partition(".")[0]) for p in select or [])):
                rel.append({"protocol": "scpi", "roles": sc[0].roles, "score": round(float(sc[0].total), 4),
                            "exchanges": sc[0].output.items})
        if rel:
            doc["relations"] = rel
    return doc


def dumps(doc) -> str:
    def default(o):
        if isinstance(o, (np.integer,)):
            return int(o)
        if isinstance(o, (np.floating,)):
            return float(o)
        if isinstance(o, np.ndarray):
            return o.tolist()
        if isinstance(o, (bytes, bytearray)):
            return o.hex()
        return str(o)
    return json.dumps(doc, default=default, ensure_ascii=False, separators=(",", ":"))
