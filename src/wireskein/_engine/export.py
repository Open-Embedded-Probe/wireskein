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
from .riscv_dm import NAMES as DM_NAMES


def _hex(vals) -> str:
    return bytes(int(v) & 0xFF for v in vals).hex()


def _ninth_bits(n, vals) -> str | None:
    """A UART of 9 data bits: each character's ninth bit ("0" / "1", in order) beside `hex`'s low eight (in
    multidrop use the address flag). 9 data bits where the parity check says so, or characters of 11 bits without
    parity whose ninth bit varies (one always 1 is an 8N2's second stop bit); None otherwise."""
    p = getattr(n, "params", {}) or {}
    v = np.asarray(vals, np.int64)
    if not len(v):
        return None
    if p.get("data_bits") == 9:
        pass
    elif p.get("data_bits") is None and p.get("L") == 11 and len(set(((v >> 8) & 1).tolist())) > 1:
        pass
    else:
        return None
    return "".join(str(int(x)) for x in ((v >> 8) & 1))


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


def layers_of(n, up=None) -> dict:
    """All layers of a reading, lowest first, as plain data: the engine's, then the upper stage's (up: its Upper,
    wireskein._upper)."""
    if up is not None:
        return {**layers_of(n), **up.layers}
    own = getattr(n, "own_layers", None)
    if own is not None:                     # another engine's reading: its layers as the format has them
        return dict(own)
    a = n.analyzer
    out = {}
    if a in ("i2c", "spi", "sync_unknown", "rvswd"):
        out.update(_sync_layers(n))
    if a == "i2c":
        out["words"] = _words(n, 9)
        out["transactions"] = [{"s": t.get("start"), "e": t.get("end"), "addr": t.get("addr"), "rw": t.get("rw"),
                                "addr_ack": t.get("addr_ack"), "bytes": _hex(t.get("bytes", [])),
                                "acks": [bool(x) for x in t.get("acks", [])],
                                **{k: t[k] for k in ("start_cond", "end_cond", "scl_low_ticks", "scl_waits", "restored")
                                   if k in t},
                                **({"complete": False, "pending_bits": t.get("pending_bits", []),
                                    **({"clock_lost": t["clock_lost"]} if "clock_lost" in t else {})}
                                   if t.get("complete") is False else {})}
                               for t in n.output.items if "addr" in t]
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
        view = getattr(sb, "view", None)                  # the modes a receiver could read the lines in, from them
        if view is not None and sb.select:
            try:
                from . import timing
                clk, cs = view.channel(sb.clock), view.channel(sb.select)
                data = [view.channel(d) for d in sb.data]
                cpol = timing.spi_mode(clk, data, view.n_samples, cs)["cpol"]
                out["modes"] = timing.spi_modes(clk, data, cpol, view.rate, view.n_samples, cs)
                out["modes_ok"] = [m["mode"] for m in out["modes"] if m["readable"]]
            except (KeyError, ValueError, TypeError):
                pass
    elif a == "sync_unknown":
        out["summary"] = {"frame_bits": n.metrics.get("frame_bits"), "delimiter": n.metrics.get("delimiter")}
    elif a in ("rvswd", "swio"):
        if a == "swio" and getattr(n, "layers", None):
            ps = n.layers["pulses"]
            out["pulses"] = {"s": ps.start.tolist(), "width": ps.width.tolist(), "short": ps.short.tolist(),
                             "split": ps.split}
        sb = (getattr(n, "layers", None) or {}).get("bits")
        view = getattr(sb, "view", None) if sb is not None else None
        if a == "rvswd" and sb is not None and view is not None:
            from .rvswd import frame_details
            details = [frame_details(x, sb, view) if "_span" in x else {} for x in n.output.items]
        else:
            details = [{} for _ in n.output.items]
        out["dmi"] = [{"s": x["t"], **({"e": x["e"]} if "e" in x else {}), "op": x["op"], "addr": x["addr"], "reg": DM_NAMES.get(x["addr"]) if x["addr"] is not None else None,
                       "data": x["data"], "ok": x.get("ok", True),
                       **({k: x[k] for k in ("form", "status", "answered", "late", "answer_delay", "answer_delay_ns", "period_ns", "bits", "repaired", "restored", "fields")
                           if k in x} if a == "rvswd" else {}),
                       **({k: x[k] for k in ("fast", "rw_low_ns", "rw_gap_ns", "short_max_ns", "long_min_ns", "short_median_ns",
                                    "long_median_ns", "space_ns", "pitch_1_ns", "pitch_0_ns") if k in x}
                          if a == "swio" else {})}
                      for x, dx in zip(n.output.items, details)]
        for row, dx in zip(out["dmi"], details):
            row.update(dx)
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
            ninth = _ninth_bits(n, it["value"])
            if ninth is not None:
                out["bytes"]["bit8"] = ninth
        elif a == "lin":
            out["frames"] = n.output.items
        else:
            out["packets"] = n.output.items
    elif a == "j1850":
        out["frames"] = list(n.output.items)
    elif a == "sent":
        out["messages"] = list(n.output.items)
    elif a == "dcc":
        out["packets"] = list(n.output.items)
    elif a == "dali":
        out["frames"] = list(n.output.items)
    elif a == "spdif":
        out["subframes"] = list(n.output.items)
    elif a == "usb_pd":
        out["packets"] = list(n.output.items)
    elif a == "cec":
        out["messages"] = list(n.output.items)
    elif a == "swim":
        out["transactions"] = list(n.output.items)
    elif a == "wiegand":
        out["frames"] = list(n.output.items)
    elif a == "usb":
        from .usb import transactions
        out["packets"] = list(n.output.items)
        out["transactions"] = transactions(out["packets"])
    elif a in ("swd", "mdio", "can", "ps2", "sdmmc") or getattr(getattr(n, "plugin", None), "external", False) or n.params.get("external"):
        rows = [{{"start": "s", "end": "e"}.get(k, k): (int(v) if isinstance(v, np.integer) else v) for k, v in x.items()}
                for x in (n.output.items or [])]
        if a == "mdio":
            _mdio_addresses(rows)
        out["packets" if a == "swd" else "frames"] = rows
    elif a == "i2s":
        out["frames"] = list(n.output.items)
    elif a == "jtag":
        out["scans"] = list(n.output.items)
    elif a == "dht":
        out["readings"] = list(n.output.items)
    elif a == "rgb_led":
        out["frames"] = [{"s": f["t"], "bits": f["bits"], "data": f["data"]} for f in n.output.items["frames"]]
    elif a == "onewire":
        out["resets"] = [{"s": z["t"], "reset_ticks": z["reset"], "presence": z["presence"], "data": z["data"],
                          **{k: z[k] for k in ("speed", "overdrive_after_bits") if k in z}}
                         for z in n.output.items]
    elif a == "ir_nec":
        out["frames"] = [{"s": f["t"], **{k: f[k] for k in ("repeat", "addr", "ext", "cmd", "cmd_ok") if k in f}}
                         for f in n.output.items]
    return out


def _mdio_addresses(rows: list) -> None:
    """Clause 45's register address on its reads and writes (`addr`): the last ADDR frame's for the port and device,
    a read-and-increment (RINC) stepping it after its read; unknown (no ADDR seen) left out."""
    at = {}
    for r in rows:
        if "dev" not in r:
            continue
        key = (r.get("phy"), r.get("dev"))
        if r.get("op") == "ADDR":
            at[key] = r.get("data")
        elif key in at and at[key] is not None:
            r["addr"] = at[key]
            if r.get("op") == "RINC":
                at[key] = (at[key] + 1) & 0xFFFF


FINAL = {"i2c": "transactions", "spi": ("sd", "transfers"), "uart": ("nmea", "modbus_rtu", "lines", "bytes"),
         "lin": "frames", "dmx512": "packets", "rvswd": ("dm", "dmi"), "swio": ("dm", "dmi"), "sync_unknown": "summary",
         "swd": "packets", "mdio": "frames", "ps2": "frames", "sdmmc": "frames", "wiegand": "frames", "swim": "transactions", "cec": "messages", "usb_pd": "packets", "spdif": "subframes", "dali": "frames", "dcc": "packets", "sent": "messages", "j1850": "frames", "i2s": "frames", "jtag": "scans", "dht": "readings", "can": "frames", "rgb_led": "frames", "onewire": "resets",
         "ir_nec": "frames", "usb": ("transactions", "packets")}


def final_of(n, lay: dict, up=None) -> dict:
    from .._upper import final_user
    best = final_user(up)
    if best is not None:                       # a protocol of others whose checks held is the bus's final reading
        return {best: lay[best]}
    want = FINAL.get(n.analyzer, ("frames",) if n.params.get("external") else ())
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
         "rvswd": "dmi", "swio": "dmi", "swd": "packets", "mdio": "frames", "ps2": "frames", "sdmmc": "frames", "wiegand": "frames", "swim": "transactions", "cec": "messages", "usb_pd": "packets", "spdif": "subframes", "dali": "frames", "dcc": "packets", "sent": "messages", "j1850": "frames", "i2s": "frames", "jtag": "scans", "dht": "readings", "can": "frames", "sync_unknown": "summary", "rgb_led": "frames", "onewire": "resets",
         "ir_nec": "frames", "usb": "packets"}


def devices_of(up, with_candidates: bool) -> list[dict]:
    """The device packs' matches the upper stage gave a reading (up: its Upper)."""
    out = []
    for m in getattr(up, "devices", None) or []:
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
        if getattr(c, "bus", None):
            rec["bus"] = c.bus                  # a target's claim: the claims with the same bus share its lines
        up = getattr(c, "upper", None)
        lay = layers_of(n, up)
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
                rec["result"] = final_of(n, lay, up)
                if depth == "device":
                    ds = devices_of(up, with_candidates=True)
                    if ds:
                        rec["devices"] = ds
        elif mode == "all":
            rec["layers"] = lay
            rec["evidence"] = {k: (float(v) if isinstance(v, (np.floating, float, np.integer)) else v)
                               for k, v in {**n.metrics, **(up.evidence if up else {})}.items()
                               if not isinstance(v, (dict, list))}
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
                    picked.update(final_of(n, lay, up))
                else:
                    for k in fnmatch.filter(lay.keys(), layer or "*"):
                        picked[k] = lay[k]
            if not picked:
                continue
            rec["result"] = picked
        doc["claims"].append(rec)
    if hasattr(res, "relations"):
        rel = []
        for r in res.relations:          # the UART pairs' (wireskein._upper.pairs): SCPI, protocols of others on a pair
            if r.get("user") and r["score"] < 0.3:
                continue
            if mode == "select" and not any(fnmatch.fnmatch(r["protocol"], p.partition(".")[0]) for p in select or []):
                continue
            rel.append({k: v for k, v in r.items() if k != "user"})
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
