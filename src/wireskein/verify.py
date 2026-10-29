"""`ws verify`: check a recorded run (runlog.py) against its expectations.

The segment tree comes from the heading markers in the host log (markers.build
on the host clock, in microseconds). A capture belongs to the innermost segment
containing the time it was armed. Each segment path with expectations is checked
on every capture inside it (children included) that holds the pins a check names.

This is verification, not discovery: pins and roles are given, decoders run with
hints, and the decode of the hypothesis with exactly those roles is compared
even if the discovery verdict would be weaker (one short transaction is not
much evidence, but it is what the test asked for).
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

import numpy as np

from ._engine import analog, events, markers, timing
from . import runlog
from ._engine.model import AnalogTrace, Capture, Channel, EventTrace
from .runlog import FORMAT


MEASURED = "measured"             # a check function's verdict for "only measure" (uart(baud=None) with nothing else)
STATUSES = ("ok", "ng", "unchecked", "measured")      # docs/run-format.ja.md §3


@dataclass
class Result:
    path: str
    capture: str | None
    check: str
    ok: bool | None               # True: ok, False: ng, None: unchecked or measured (see status)
    expected: dict = field(default_factory=dict)
    measured: dict = field(default_factory=dict)
    reason: str = ""
    status: str = ""

    def __post_init__(self):
        if self.ok == MEASURED:
            self.ok, self.status = None, "measured"
        elif not self.status:
            self.status = {True: "ok", False: "ng", None: "unchecked"}[self.ok]


def load_capture(run_dir: Path, c: dict) -> list[Capture]:
    """A recorded capture's parts (wireskein-format §3.4), each checked on its own."""
    from ._engine import fileio
    parts = fileio.load_parts(run_dir / c["file"])
    for cap in parts:
        label = c["file"] if len(parts) == 1 else f"{c['file']}#part{cap.meta['part']}"
        cap.meta.update(file=c["file"], t0=c["t0"], label=label)
    return parts


def _lost(cap: Capture) -> str:
    """Why a capture is incomplete (wireskein-format §3.4), in one line."""
    return "data lost while capturing: " + "; ".join(
        f"after part {x['after_part']}: {x['reason']}" + (f" ({x['ticks']} ticks)" if x.get("ticks") else "")
        + (", stopped there" if x.get("stopped") else "") for x in cap.meta.get("lost", []))


def segment_tree(doc: dict):
    """The sections of the log's headings, on the host clock in microseconds. Each section keeps the log lines that
    open and close it (opened / closed: None when the log ends it), whose probe_ns place it in a capture."""
    log = sorted(doc["log"], key=lambda e: e["t"])
    lines = [(e["text"], i, i) for i, e in enumerate(log) if e["src"] == "marker"]
    end = int(max([e["t"] for e in log] + [c["t0"] for c in doc["captures"]] + [0]) * 1e6) + 1
    tree = markers.build(lines, markers.load_dialects()["heading"], len(log))
    us = lambda i: int(log[i]["t"] * 1e6) if i < len(log) else end          # noqa: E731
    for s in tree.walk():                      # built on the lines' order: back to their times
        s.opened = log[s.begin] if s.kind != "root" else None
        s.closed = log[s.end] if s.end is not None and s.end < len(log) else None
        s.begin, s.end = (0, end) if s.kind == "root" else (us(s.begin), us(s.end) if s.end is not None else None)
        for issue in s.issues:
            if "at" in issue:
                issue["at"] = us(issue["at"])
    return tree


def _window(cap: Capture, s0: int, s1: int) -> Capture:
    """Ticks [s0, s1) of a capture as a capture of its own (tick 0 at s0)."""
    chans = []
    for ch in cap.channels:
        e = ch.edges[(ch.edges > s0) & (ch.edges < s1)] - s0
        chans.append(Channel(ch.name, _level_at(ch, s0), e, ch.step, (ch.phase - s0) % ch.step, ch.acquisition))
    an = []
    for a in cap.analog:
        per = Fraction(cap.meta.get("tick_hz", cap.rate)) / a.rate_hz
        k0 = max(0, math.ceil((s0 - a.t0_ticks) / per))
        k1 = max(k0, min(len(a.values), math.ceil((s1 - a.t0_ticks) / per)))
        an.append(AnalogTrace(a.name, a.values[k0:k1], a.rate_hz, a.t0_ticks + k0 * per - s0, a.encoding, a.width,
                              a.value_bits, a.zero, a.scale_nv, a.unit, a.acquisition))
    evs = []
    for t in cap.events:
        k0 = max(0, -(-(s0 - t.phase) // t.step))
        k1 = max(k0, min(t.n, (s1 - t.phase) // t.step))
        before = np.searchsorted(t.quanta, k0)
        sel = (t.quanta >= k0) & (t.quanta < k1)
        evs.append(EventTrace(t.name, t.step, t.phase + k0 * t.step - s0, k1 - k0,
                              int(t.levels[before - 1]) if before else t.start_level, t.quanta[sel] - k0,
                              t.levels[sel], t.counts[sel], t.acquisition))
    meta = {**cap.meta, "window": [s0, s1]}
    if meta.get("trigger_tick") is not None:
        meta["trigger_tick"] -= s0
    return Capture(cap.rate, s1 - s0, chans, meta, an, evs)


def _on_probe_clock(seg, cap: Capture) -> tuple[tuple | None, str]:
    """A section opened and closed by the probe's lines, in one capture part: ((inner, outer, clipped), "") with the
    windows [s0, s1) the section's bounds give at either end of their uncertainty (inner: both moved in), or
    (None, why) when they do not fall in this part."""
    view = {"meta": cap.meta, "tick_hz": [cap.meta["tick_hz"].numerator, cap.meta["tick_hz"].denominator]}
    (a, ua, why), (b, ub, why_b) = (runlog.place(seg.opened, view), runlog.place(seg.closed, view))
    if a is None or b is None:
        return None, why or why_b
    if b < 0 or a > cap.n_samples:
        return None, "the section is outside this capture"
    clipped = [x for x, out in (("begin", a - ua < 0), ("end", b + ub > cap.n_samples)) if out]
    outer = (max(0, math.floor(a - ua)), min(cap.n_samples, math.ceil(b + ub)))
    inner = (max(0, math.ceil(a + ua)), min(cap.n_samples, math.floor(b - ub)))
    return ((inner if inner[0] < inner[1] else None), outer, clipped), ""


def _probe_section(seg) -> bool:
    return all(e is not None and e.get("probe_ns") is not None for e in (seg.opened, seg.closed))


# ---------------- checks ----------------

def _ch(cap: Capture, pin: str) -> Channel | None:
    try:
        return cap.channel(pin)
    except KeyError:
        return None


def _ev(cap: Capture, pin: str) -> EventTrace | None:
    """The pin kept as changes at step > 1: its counts answer exactly what the Channel view (when there is one)
    cannot see, so the checks that count or compare levels read it first."""
    return next((t for t in cap.events if t.name == pin), None)


def _level_at(ch: Channel, s: int) -> int:
    return int(ch.initial ^ (np.searchsorted(ch.edges, s, side="right") & 1))


def check_square(cap, x):
    ch = _ch(cap, x["pin"])
    if ch is None:
        return None, {}, "pin not captured"
    p = markers._periodic(ch.edges, ch.initial, cap.rate, ch.step)
    if p is None:
        return False, {"edges": int(len(ch.edges))}, "no steady square wave"
    ferr = p["freq_hz"] / x["freq_hz"] - 1
    ok = abs(ferr) <= x["tol_freq"]
    why = [] if ok else [f"frequency {p['freq_hz']:.2f} Hz is {ferr * 100:+.2f}% off"]
    if x.get("duty") is not None:
        if p["duty"] is None or abs(p["duty"] - x["duty"]) > x["tol_duty"] + p["resolution"]:
            ok = False
            why.append(f"duty {p['duty']:.4f} vs {x['duty']:.4f}")
    if x.get("max_jitter") is not None and p["period_spread"] > x["max_jitter"]:
        ok = False
        why.append(f"period spread {p['period_spread']:.4f} > {x['max_jitter']}")
    return ok, {**p, "freq_error": ferr}, "; ".join(why)


def check_level(cap, x):
    ch = _ch(cap, x["pin"])
    t = _ev(cap, x["pin"])
    if t is not None:
        ok = not events.moved(t) and t.start_level == x["value"]
        return ok, {"initial": t.start_level, "changing_quanta": int(len(t.quanta)), "step": t.step}, (
            "" if ok else f"not constant {x['value']}")
    if ch is None:
        return None, {}, "pin not captured"
    got = {"initial": ch.initial, "edges": int(len(ch.edges)),
           "high_fraction": float(np.mean(ch.level_at(np.arange(0, cap.n_samples, max(1, cap.n_samples // 4096)))))}
    ok = len(ch.edges) == 0 and ch.initial == x["value"]
    return ok, got, "" if ok else f"not constant {x['value']}"


def _levels(cap, want: dict, at_end: bool):
    got, bad, missing, unknown = {}, [], [], []
    for pin, v in want.items():
        ch = _ch(cap, pin)
        t = _ev(cap, pin)
        if t is not None:
            got[pin] = events.end_level(t) if at_end else events.first_level(t)
            if got[pin] is None:
                unknown.append(f"{t.name}: quantum 0 changed and its {t.step} ticks do not say whether at the first")
                continue
        elif ch is None:
            missing.append(pin)
            continue
        else:
            got[pin] = _level_at(ch, cap.n_samples - 1) if at_end else ch.initial
        if got[pin] != v:
            bad.append(f"{pin}={got[pin]} (want {v})")
    if missing and not got:
        return None, got, "pins not captured: " + ", ".join(missing)
    if bad:
        return False, got, "; ".join(bad)
    if unknown:
        return None, got, "; ".join(unknown) + f": the summaries do not tell its level at the {'end' if at_end else 'start'}"
    return True, got, ""


def check_ends(cap, x):
    return _levels(cap, x["levels"], True)


def check_starts(cap, x):
    return _levels(cap, x["levels"], False)


def check_only_moving(cap, x):
    kept = {t.name for t in cap.events}
    moved = {c.name: int(len(c.edges)) for c in cap.channels
             if len(c.edges) and c.name not in x["pins"] and c.name not in kept}
    for t in cap.events:
        if t.name not in x["pins"] and events.moved(t):
            moved[t.name] = int(np.sum(t.counts))
    if moved:
        return False, {"unexpected": moved}, "other pins moved: " + ", ".join(moved)
    return True, {"unexpected": moved}, ""


def check_pulses(cap, x):
    ch = _ch(cap, x["pin"])
    t = _ev(cap, x["pin"])
    if t is not None and (ch is None or x.get("period_s") is None):   # the count from the quanta, exact
        n, exact = events.rises(t)
        got = {"rises": n} if exact else {"rises_at_least": n}
        if x.get("count") is not None:
            if exact and n != x["count"] or not exact and n > x["count"]:
                return False, got, f"{'' if exact else 'at least '}{n} rising edges, want {x['count']}"
            if not exact:
                return None, got, f"{t.name}: at least {n} rising edges (a quantum's count saturated at 127)"
        if x.get("period_s") is not None:
            return None, got, f"{events.describe(t)}: the period needs every edge"
        return True, got, ""
    if ch is None:
        return None, {}, "pin not captured"
    lv = ch.initial
    rises = ch.edges[0::2] if lv == 0 else ch.edges[1::2]
    got = {"rises": int(len(rises))}
    ok, why = True, []
    if x.get("count") is not None and len(rises) != x["count"]:
        ok = False
        why.append(f"{len(rises)} rising edges, want {x['count']}")
    if x.get("period_s") is not None and len(rises) >= 2:
        per = float(np.median(np.diff(rises))) / cap.rate
        got["period_s"] = per
        if abs(per / x["period_s"] - 1) > x["tol_period"]:
            ok = False
            why.append(f"period {per * 1e6:.2f} us vs {x['period_s'] * 1e6:.2f} us")
    return ok, got, "; ".join(why)


def _decode(cap, hints):
    from ._engine import staged
    return staged.analyze(cap, hints)


def _best(res, analyzer, need: dict):
    cands = [n for n in res.roots if n.analyzer == analyzer and n.output is not None
             and all(n.roles.get(k) == v for k, v in need.items())]
    return max(cands, key=lambda n: n.total, default=None)


def _burst_hz(ch: Channel, rate: float) -> float | None:
    """Clock frequency inside bursts (rise-to-rise, gaps between bursts ignored)."""
    rises = ch.edges[0::2] if ch.initial == 0 else ch.edges[1::2]
    if len(rises) < 3:
        return None
    d = np.diff(rises)
    med = float(np.median(d))
    d = d[d < 1.5 * med]
    return rate / float(np.mean(d)) if len(d) else None


def _i2c_tx(t: dict) -> dict:
    out = {"addr": t["addr"], "rw": t["rw"], "bytes": list(t.get("bytes", [])),
           "ack": bool(t.get("addr_ack")) if t["addr"] is not None else None, "complete": t.get("complete", True)}
    if not out["complete"]:
        out["pending_bits"] = list(t.get("pending_bits", []))
    return out


def _i2c_want(t: dict) -> dict:
    out = {"addr": t["addr"], "rw": t.get("rw"), "bytes": list(t.get("bytes", [])),
           "ack": bool(t.get("ack", True)) if t["addr"] is not None else None, "complete": bool(t.get("complete", True))}
    if "pending_bits" in t:
        out["pending_bits"] = list(t["pending_bits"])
    return out


def _i2c_fmt(t: dict) -> str:
    if t["addr"] is None:
        return f"(cut in the address, bits {t.get('pending_bits', [])})"
    s = f"0x{t['addr']:02x} {t['rw']} [{' '.join(f'{b:02x}' for b in t['bytes'])}]" + ("" if t["ack"] else " NACK")
    if not t["complete"]:
        s += f" incomplete (pending bits {t.get('pending_bits', [])})"
    return s


def _i2c_diff(want: list[dict], got: list[dict]) -> str:
    """The first transaction that differs, both sides written out ("" if equal).
    pending_bits is compared only when the expectation names it."""
    for i, (w, g) in enumerate(zip(want, got)):
        if w != {k: g.get(k) for k in w}:
            return f"#{i} want {_i2c_fmt(w)}, got {_i2c_fmt(g)}"
    if len(want) != len(got):
        extra = f"missing {_i2c_fmt(want[len(got)])}" if len(want) > len(got) else f"extra {_i2c_fmt(got[len(want)])}"
        return f"{len(want)} expected, {len(got)} decoded; #{min(len(want), len(got))} {extra}"
    return ""


def check_i2c(cap, x):
    scl, sda = _ch(cap, x["scl"]), _ch(cap, x["sda"])
    if scl is None or sda is None:
        return None, {}, "pins not captured"
    res = _decode(cap, {"protocols": ["i2c"], "pins": {x["scl"]: {"protocol": "i2c", "role": "scl"},
                                                        x["sda"]: {"protocol": "i2c", "role": "sda"}}})
    n = _best(res, "i2c", {"scl": x["scl"], "sda": x["sda"]})
    got = {"transactions": [_i2c_tx(t) for t in (n.output.items if n else []) if "addr" in t]}
    ok, why = True, []
    if x.get("transactions") is not None:
        diff = _i2c_diff([_i2c_want(t) for t in x["transactions"]], got["transactions"])
        if diff:
            ok = False
            why.append("transactions differ: " + diff)
    hz = _burst_hz(scl, cap.rate)
    got["scl_hz"] = hz
    if x.get("hz") is not None and (hz is None or abs(hz / x["hz"] - 1) > x["tol_hz"]):
        ok = False
        why.append(f"SCL {hz and round(hz)} Hz vs {x['hz']}")
    if x.get("released", True):
        end = {"scl": _level_at(scl, cap.n_samples - 1), "sda": _level_at(sda, cap.n_samples - 1)}
        got["end_levels"] = end
        if end != {"scl": 1, "sda": 1}:
            ok = False
            why.append(f"bus not released at the end (SCL={end['scl']}, SDA={end['sda']})")
    return ok, got, "; ".join(why)


def check_spi(cap, x):
    pins = {"clk": x["clk"], **{k: x[k] for k in ("mosi", "miso", "cs") if x.get(k)}}
    if any(_ch(cap, p) is None for p in pins.values()):
        return None, {}, "pins not captured"
    res = _decode(cap, {"protocols": ["spi"], "pins": {p: {"protocol": "spi", "role": r} for r, p in pins.items()}})
    need = {"clk": x["clk"], **({"cs": x["cs"]} if x.get("cs") else {})}
    n = _best(res, "spi", need)
    if n is None:
        return False, {}, "nothing decoded as SPI on these pins"
    lines = n.output.items["lines"]
    rev = (lambda b: int(f"{b:08b}"[::-1], 2)) if x.get("bit_order", "msb") == "lsb" else (lambda b: b)
    hexes = {pin: bytes(rev(int(v) & 0xFF) for fr in frs for v in fr).hex() for pin, frs in lines.items()}
    got = {"mode": n.metrics.get("mode"), "bytes": hexes, "sck_hz": _burst_hz(cap.channel(x["clk"]), cap.rate)}
    ok, why = True, []
    # the mode read from the lines themselves (timing.spi_mode): SCK's level as CS becomes active, the edge MOSI
    # (else MISO) changes after; the decoder's choice is kept as decoded_mode
    m = timing.spi_mode(cap.channel(x["clk"]), [cap.channel(x[r]) for r in ("mosi", "miso") if x.get(r)],
                        cap.n_samples, cap.channel(x["cs"]) if x.get("cs") else None, x.get("cs_active", 0))
    line = next((m["lines"][x[r]] for r in ("mosi", "miso") if x.get(r) and m.get("lines", {}).get(x[r], {}).get(
        "cpha") is not None), None)
    if line is not None:
        got.update(decoded_mode=got["mode"], mode=m["cpol"] * 2 + line["cpha"], cpol=m["cpol"],
                   cpha=line["cpha"], cpha_votes={k: {e: v[e] for e in ("after_leading", "after_trailing",
                                                                        "before_first")}
                                                  for k, v in m["lines"].items()})
        if len(set(m["cpol_votes"])) > 1:
            ok = False
            why.append(f"SCK's level when CS became active was {m['cpol_votes'].count(0)} times 0 and "
                       f"{m['cpol_votes'].count(1)} times 1")
    for role in ("mosi", "miso"):
        want = x.get(f"{role}_bytes")
        if want is not None and hexes.get(x.get(role)) != want.lower():
            ok = False
            why.append(f"{role} {hexes.get(x.get(role))} vs {want}")
    if x.get("mode") is not None and got["mode"] != x["mode"]:
        ok = False
        why.append(f"mode {got['mode']} vs {x['mode']}")
    if x.get("hz") is not None and (got["sck_hz"] is None or abs(got["sck_hz"] / x["hz"] - 1) > x["tol_hz"]):
        ok = False
        why.append(f"SCK {got['sck_hz'] and round(got['sck_hz'])} Hz vs {x['hz']}")
    if x.get("cs"):
        cs_end = _level_at(cap.channel(x["cs"]), cap.n_samples - 1)
        got["cs_end"] = cs_end
        if cs_end != 1:
            ok = False
            why.append("CS not high at the end")
    return ok, got, "; ".join(why)


def _verdicts(parts: list[tuple[bool | None, str]]) -> tuple[bool | None, str]:
    """One verdict from several: any broken limit fails, else any undecided one leaves the check unchecked."""
    bad = [w for v, w in parts if v is False]
    if bad:
        return False, "; ".join(bad)
    open_ = [w for v, w in parts if v is None]
    return (None, "; ".join(open_)) if open_ else (True, "")


def _limit_of(name: str, ticks: list[tuple[int, int]], res: int, lo: float | None, hi: float | None, rate: float):
    """Every instance of a time (tick, length) against [lo, hi] seconds: the worst one decides."""
    v = np.array([d for _, d in ticks], np.float64)
    at = np.array([t for t, _ in ticks], np.float64)
    out = []
    if lo is not None:
        k = int(np.argmin(v))
        j = timing.judge(v[k], res, lo=lo * rate)
        out.append((j, f"{name} {_si(v[k] / rate)} {'<' if j is False else 'near'} {_si(lo)} at {_si(at[k] / rate)}"))
    if hi is not None:
        k = int(np.argmax(v))
        j = timing.judge(v[k], res, hi=hi * rate)
        out.append((j, f"{name} {_si(v[k] / rate)} {'>' if j is False else 'near'} {_si(hi)} at {_si(at[k] / rate)}"))
    return out


def _si(s: float) -> str:
    a = abs(s)
    for unit, f in (("s", 1), ("ms", 1e3), ("us", 1e6), ("ns", 1e9)):
        if a >= 1 / f or unit == "ns":
            return f"{s * f:.4g} {unit}"


def _times(ticks: list[tuple[int, int]], rate: float) -> dict:
    v = np.array([d for _, d in ticks], np.float64)
    if len(v) == 0:
        return {"count": 0}
    return {"count": int(len(v)), "min_s": float(v.min()) / rate, "max_s": float(v.max()) / rate,
            "median_s": float(np.median(v)) / rate}


def check_phase(cap, x):
    a, b = _ch(cap, x["a"]), _ch(cap, x["b"])
    if a is None or b is None:
        return None, {}, "pins not captured"
    p = timing.phase(a, b, cap.rate, x.get("edge", "rise"))
    deg = p.pop("deg", None)
    if not p["pairs"]:
        return False, p, f"no {x['b']} edge within a period after {x['a']}'s"
    parts = []
    if x.get("phase_deg") is not None:
        dev = np.abs([timing.circular_diff(float(d), x["phase_deg"]) for d in deg])
        k = int(np.argmax(dev))
        j = timing.judge(float(dev[k]), p["resolution_deg"], hi=x["tol_deg"])
        parts.append((j, f"phase {deg[k]:.2f} deg vs {x['phase_deg']} +- {x['tol_deg']} "
                         f"(resolution {p['resolution_deg']:.2f} deg)"))
    if x.get("freq_hz") is not None:
        for k in ("freq_hz", "freq_b_hz"):
            err = p[k] / x["freq_hz"] - 1
            parts.append((abs(err) <= x["tol_freq"], f"{x['a' if k == 'freq_hz' else 'b']} {p[k]:.2f} Hz is "
                                                     f"{err * 100:+.2f}% off"))
    if not parts:
        return MEASURED, p, ""
    ok, why = _verdicts(parts)
    return ok, p, why


def check_skew(cap, x):
    a, b = _ch(cap, x["a"]), _ch(cap, x["b"])
    if a is None or b is None:
        return None, {}, "pins not captured"
    r = timing.skew(a, b, cap.rate)
    d = r["delays"].astype(np.float64)
    res = timing.res_of(a, b)
    got = {"pairs": int(len(d)), "unpaired_a": r["unpaired_a"], "unpaired_b": r["unpaired_b"],
           "resolution_s": res / cap.rate}
    if len(d):
        got.update(min_s=float(d.min()) / cap.rate, max_s=float(d.max()) / cap.rate,
                   median_s=float(np.median(d)) / cap.rate)
    if not len(d):
        return False, got, f"no edge of {x['b']} near {x['a']}'s"
    parts = []
    if r["unpaired_a"] or r["unpaired_b"]:
        parts.append((False, f"{r['unpaired_a']} edges of {x['a']} and {r['unpaired_b']} of {x['b']} without a partner"))
    want = x.get("delay_s", 0.0) * cap.rate
    dev = np.abs(d - want)
    k = int(np.argmax(dev))
    parts.append((timing.judge(float(dev[k]), res, hi=x["tol_s"] * cap.rate),
                  f"{x['b']} {_si(d[k] / cap.rate)} after {x['a']} vs {_si(x.get('delay_s', 0.0))} +- {_si(x['tol_s'])}"))
    ok, why = _verdicts(parts)
    return ok, got, why


def check_dead_time(cap, x):
    hi, lo = _ch(cap, x["hi"]), _ch(cap, x["lo"])
    if hi is None or lo is None:
        return None, {}, "pins not captured"
    r = timing.dead_time(hi, lo, cap.n_samples, x.get("active", 1))
    res = timing.res_of(hi, lo)
    got = {"hi_to_lo": _times(r["hi_to_lo"], cap.rate), "lo_to_hi": _times(r["lo_to_hi"], cap.rate),
           "overlaps": _times(r["overlaps"], cap.rate), "resolution_s": res / cap.rate}
    if not r["hi_to_lo"] and not r["lo_to_hi"] and not r["overlaps"]:
        return False, got, "the outputs never switched from one to the other"
    parts = []
    if r["overlaps"] and not x.get("allow_overlap"):
        t, ln = max(r["overlaps"], key=lambda o: o[1])
        parts.append((False, f"both on {len(r['overlaps'])} times, longest {_si(ln / cap.rate)} at {_si(t / cap.rate)}"))
    for side in ("hi_to_lo", "lo_to_hi"):
        if r[side]:
            parts += _limit_of(side, r[side], res, x.get("min_s"), x.get("max_s"), cap.rate)
    if not parts:
        return MEASURED, got, ""
    ok, why = _verdicts(parts)
    return ok, got, why


def check_i2c_timing(cap, x):
    scl, sda = _ch(cap, x["scl"]), _ch(cap, x["sda"])
    if scl is None or sda is None:
        return None, {}, "pins not captured"
    lim = {**(timing.I2C_MODES[x["mode"]] if x.get("mode") else {}),
           **{k: tuple(v) if isinstance(v, (list, tuple)) else (v, None) for k, v in (x.get("limits") or {}).items()}}
    r = timing.i2c_times(scl, sda, cap.n_samples)
    res = timing.res_of(scl, sda)
    got = {k: _times(r[k], cap.rate) for k in timing.I2C_TIMES}
    got["resolution_s"] = res / cap.rate
    per = np.array([d for _, d in r["periods"]], np.float64)
    if len(per):
        got["f_scl"] = {"count": int(len(per)), "max_hz": cap.rate / float(per.min()),
                        "median_hz": cap.rate / float(np.median(per))}
    if not r["t_low"]:
        return None, got, "no clock pulse inside a transaction"
    parts = []
    for k, (lo, hi) in lim.items():
        if k == "f_scl":
            if hi is not None and len(per):
                at = r["periods"][int(np.argmin(per))][0]
                j = timing.judge(float(per.min()), res, lo=cap.rate / hi)
                parts.append((j, f"SCL {cap.rate / float(per.min()) / 1e3:.4g} kHz {'>' if j is False else 'near'} "
                                 f"{hi / 1e3:.4g} kHz at {_si(at / cap.rate)}"))
        elif r.get(k):
            parts += _limit_of(k, r[k], res, lo, hi, cap.rate)
    ok, why = _verdicts(parts)
    return ok, got, why


def _uart_rx(ch: Channel, n: int, u: float, idle: int, bits: int, parity: str, stop: float) -> dict:
    """A receiver at a given bit time: each character starts at the first edge
    leaving idle after the middle of the previous one's first stop bit, and its
    bits are sampled at their middles.

    The window may open inside a character, so reading starts only once the
    receiver is in step: after an idle gap of a whole character; or, when no
    such gap comes later in the window (a back-to-back stream with little idle
    at the start), at a start edge from which the characters that follow keep
    valid stop (and parity) bits (LOOKAHEAD of them, or all up to the end of
    the window). A gap is preferred because regular data can keep valid stop
    bits for a while from a wrong edge. Characters
    before that are lead-in, a character the window ends inside is cut; neither
    counts as an error. After a framing error the receiver gets in step again
    the same way, so one disturbance counts once (characters skipped meanwhile
    are resync_skipped)."""
    e = ch.edges
    after = ch.initial ^ ((np.arange(len(e)) + 1) & 1)          # level after each edge
    act = e[after != idle]
    npar = 0 if parity == "none" else 1
    L = 1 + bits + npar + int(np.ceil(stop))
    first_stop = 1 + bits + npar
    gap = L * u

    def frame(st: int):
        b = ch.level_at((st + (np.arange(L) + 0.5) * u).astype(np.int64)).astype(np.int64)
        if idle == 0:
            b = 1 - b
        value = int((b[1:1 + bits] << np.arange(bits)).sum())
        stop_ok = bool(b[0] == 0 and b[first_stop] == 1 and (stop < 2 or b[first_stop + 1] == 1))
        par_ok = True
        if npar:
            ones = int(b[1:1 + bits].sum()) + int(b[1 + bits])
            par_ok = (ones % 2 == 0) if parity == "even" else (ones % 2 == 1)
        return value, stop_ok, par_ok

    def next_start(i: int, st: int) -> int:
        return int(np.searchsorted(act, st + (first_stop + 0.5) * u))

    def in_step(i: int) -> bool:
        """The characters from start edge i on keep valid stop / parity bits."""
        good = 0
        while i < len(act) and good < LOOKAHEAD:
            st = int(act[i])
            if st + L * u > n:
                return good >= 3               # the window ends: all it held were good
            _, stop_ok, par_ok = frame(st)
            if not (stop_ok and par_ok):
                return False
            good += 1
            i = next_start(i, st)
        return good >= LOOKAHEAD or good >= 3

    # an idle gap of a whole character before start edge k, and whether one comes at k or later
    j = np.searchsorted(e, act)
    prev = np.where(j > 0, e[np.maximum(j - 1, 0)], 0)
    quiet = np.where(j > 0, act - prev, act if ch.initial == idle else 0)
    gap_at = quiet >= gap
    gap_later = np.flip(np.logical_or.accumulate(np.flip(gap_at))) if len(act) else gap_at

    out = {"frames": [], "lead_in": 0, "resync_skipped": 0, "cut_at_end": False}
    synced, lost, i = False, False, 0
    while i < len(act):
        st = int(act[i])
        if not synced:
            synced = bool(gap_at[i]) or (not gap_later[i] and in_step(i))
        if st + L * u > n:
            out["cut_at_end"] = True
            break
        value, stop_ok, par_ok = frame(st)
        if synced:
            out["frames"].append((st, value, stop_ok, par_ok))
            if not stop_ok:
                synced, lost = False, True
        else:
            out["resync_skipped" if lost else "lead_in"] += 1
        i = next_start(i, st)
    return out


LOOKAHEAD = 16          # characters in a row with valid stop bits that put the receiver in step


def _uart_idle(ch: Channel, n: int, u: float, idle: int, bits: int, parity: str, stop: float, rx: dict) -> int | None:
    """The level the line rests at, judged by reading: the idle level whose
    reading gives clearly more good characters. The longest run decides only
    when neither reads (a burst of zeros can outlast a short idle)."""
    good = sum(f[2] and f[3] for f in rx["frames"])
    other = _uart_rx(ch, n, u, 1 - idle, bits, parity, stop)
    good_other = sum(f[2] and f[3] for f in other["frames"])
    if good_other > 2 * good + 2:
        return 1 - idle
    if good:
        return idle
    _, length, level = ch.runs(n)
    return int(level[np.argmax(length)]) if len(length) else None


def _uart_bit_time(ch: Channel, frames: list, u0: float, idle: int, span_bits: int) -> float | None:
    """Bit time from edges of the same direction inside good characters (so a
    difference between rise and fall delays cancels): least squares on
    d = k * u, first with short distances, then with all of them. When no
    character is good (the rate is far off), all of them are used."""
    e = ch.edges
    after = ch.initial ^ ((np.arange(len(e)) + 1) & 1)
    good = [f for f in frames if f[2] and f[3]] or frames
    ds = []
    for st, *_ in good:
        a, b = np.searchsorted(e, st), np.searchsorted(e, st + (span_bits + 0.5) * u0)
        fe, fl = e[a:b], after[a:b]
        for lv in (0, 1):
            x = fe[fl == lv]
            if len(x) > 1:
                ds.append((x[1:] - x[0]).astype(np.float64))
    if not ds:
        return None
    d = np.concatenate(ds)
    u = u0
    for kmax in (4, None):
        k = np.round(d / u)
        sel = (k >= 1) & ((k <= kmax) if kmax else True)
        if sel.any():
            u = float((d[sel] * k[sel]).sum() / (k[sel] ** 2).sum())
    return u


def _uart_edge_offsets(ch: Channel, frames: list, u: float, span_bits: int) -> list[float]:
    """Per character: the largest distance (in bits) of an edge inside it from
    the bit grid anchored at its start edge. A transmitter keeps its edges on the
    grid, so a large offset in one character points at the capture's time base
    (a sampler that stalls) rather than at the line."""
    e = ch.edges
    out = []
    for st, *_ in frames:
        d = (e[(e > st) & (e < st + (span_bits + 0.5) * u)] - st) / u
        out.append(float(np.abs(d - np.round(d)).max()) if len(d) else 0.0)
    return out


def _uart_measure(ch: Channel, n: int, runs: np.ndarray, u_hint: float | None, idle: int, bits: int, parity: str,
                  stop: float) -> float | None:
    from ._engine.features import estimate_units
    cands = estimate_units(runs[1:-1])
    if not cands:
        return None
    u = (min(cands, key=lambda c: abs(np.log(c.samples / u_hint))) if u_hint else cands[0]).samples
    for _ in range(3):
        rx = _uart_rx(ch, n, u, idle, bits, parity, stop)
        u = _uart_bit_time(ch, rx["frames"], u, idle, 1 + bits + (parity != "none")) or u
    return u


def check_uart(cap, x):
    ch = _ch(cap, x["pin"])
    if ch is None:
        return None, {}, "pin not captured"
    idle, bits, parity, stop = x.get("idle", 1), x.get("bits", 8), x.get("parity", "none"), x.get("stop", 1)
    start, length, level = ch.runs(cap.n_samples)
    # the measured bit time does not depend on the expectation: candidates from
    # the runs (the expected rate only picks the nearest), refined by reading
    u = _uart_measure(ch, cap.n_samples, length, cap.rate / x["baud"] if x.get("baud") else None,
                      idle, bits, parity, stop)
    if not x.get("baud") and u is None:
        return None, {"edges": int(len(ch.edges))}, "measure only: too few edges to find a bit time"
    # characters, errors and data: read as a receiver at the expected rate would
    u0 = cap.rate / x["baud"] if x.get("baud") else u
    rx = _uart_rx(ch, cap.n_samples, u0, idle, bits, parity, stop)
    frames = rx["frames"]
    rest = _uart_idle(ch, cap.n_samples, u0, idle, bits, parity, stop, rx)     # the level the line rests at
    if not frames:
        got = {"idle": rest, "lead_in": rx["lead_in"], "cut_at_end": rx["cut_at_end"], "samples_per_bit": u0 / ch.step}
        if not len(ch.edges):
            return False, got, f"no activity (constant {ch.initial})"
        if rest != idle:
            return False, got, f"idle level {rest}, not {idle}"
        why = "no UART character (neither an idle gap of a whole character nor a run of valid characters)"
        return (None, got, "measure only: " + why) if not x.get("baud") else (False, got, why)
    frame_err = sum(not f[2] for f in frames)
    par_err = sum(f[2] and not f[3] for f in frames)
    data = bytes(f[1] & 0xFF for f in frames if f[2] and f[3]).hex() if bits <= 8 else None
    ref = cap.rate / x["baud"] if x.get("baud") else None
    got = {"baud": cap.rate / u if u else None, "baud_error": (ref / u - 1) if u and ref else None,
           "samples_per_bit": (u if u else u0) / ch.step,       # samples of this channel
           "chars": len(frames), "frame_errors": frame_err, "parity_errors": par_err,
           "lead_in": rx["lead_in"], "resync_skipped": rx["resync_skipped"], "cut_at_end": rx["cut_at_end"], "data": data}
    got["idle"] = rest
    off = _uart_edge_offsets(ch, frames, u or u0, 1 + bits + (parity != "none") + 1)
    got["edge_offset_max"] = max(off)
    got["edge_offset_p99"] = float(np.percentile(off, 99))
    bad = [(f[0], o) for f, o in zip(frames, off) if not (f[2] and f[3])]
    got["errors_at"] = [{"sample": int(t), "edge_offset": round(o, 3)} for t, o in bad[:8]]
    ok, why = True, []
    if not x.get("baud"):
        pass            # measure only: the rate is what is reported
    elif u is None:
        ok = False
        why.append("no edges to measure the bit time")
    elif abs(got["baud_error"]) > x.get("tol_baud", 0.03):
        ok = False
        why.append(f"baud {got['baud']:.0f} is {got['baud_error'] * 100:+.2f}% off {x['baud']:.0f}")
    if x.get("max_errors") is not None and frame_err + par_err > x["max_errors"]:
        ok = False
        msg = f"{frame_err} framing and {par_err} parity errors"
        skewed = [o for _, o in bad if o > max(0.2, 2 * got["edge_offset_p99"])]
        if bad and len(skewed) == len(bad):
            msg += (f" (each in a character whose edges are up to {max(skewed):.2f} bit off the bit grid,"
                    f" elsewhere {got['edge_offset_p99']:.2f} at p99: suspect the capture time base)")
        why.append(msg)
    if x.get("data") is not None and data != x["data"].lower():
        ok = False
        why.append("data differs")
    if got["idle"] != idle:
        ok = False
        why.append(f"idle level {got['idle']}")
    if not x.get("baud") and ok and x.get("data") is None and x.get("max_errors") is None:
        # nothing was expected: a measurement (status "measured", not a failure)
        return MEASURED, got, (f"measured {got['baud']:.0f} baud, " if got["baud"] else "") + \
            f"{got['chars']} chars, {frame_err} framing / {par_err} parity errors"
    return ok, got, "; ".join(why)


def _analog(cap: Capture, pin: str) -> AnalogTrace | None:
    return next((a for a in cap.analog if a.name == pin), None)


def check_voltage(cap, x):
    """Mean, range and ripple in volts. Samples clipped at an end of the converter (OEP capture §1.2) are not
    voltages: they say only "at or below the low end" / "at or above the high end". A range check they decide
    either way still counts (below a low end that is already under min_v is NG); what they leave open is
    unchecked, with the reason."""
    a = _analog(cap, x["pin"])
    if a is None:
        return None, {}, "not an analog channel in this capture" if _ch(cap, x["pin"]) else "pin not captured"
    got = analog.levels(a)
    if got is None:
        return None, {"samples": int(len(a.values))}, "no conversion to volts in the file"
    c_lo, c_hi = got.get("clipped_low", 0), got.get("clipped_high", 0)
    lo_v, hi_v = got.get("range_v", [None, None])
    if not got["samples"] and not (c_lo or c_hi):
        return False, got, "no samples"
    ok, why, open_ = True, [], []
    if x.get("min_v") is not None:
        if c_lo and lo_v < x["min_v"]:
            ok = False
            why.append(f"{c_lo} samples clipped at the low end (<= {lo_v:.3f} V) < {x['min_v']:g} V")
        elif c_lo:
            open_.append(f"{c_lo} samples clipped at <= {lo_v:.3f} V: min_v {x['min_v']:g} V cannot be told")
        elif got["samples"] and got["min_v"] < x["min_v"]:
            ok = False
            why.append(f"min {got['min_v']:.4f} V < {x['min_v']:g} V")
        if c_hi and hi_v < x["min_v"]:                  # >= the high end, but the high end is under min_v
            open_.append(f"{c_hi} samples clipped at >= {hi_v:.3f} V: min_v {x['min_v']:g} V cannot be told")
    if x.get("max_v") is not None:
        if c_hi and hi_v > x["max_v"]:
            ok = False
            why.append(f"{c_hi} samples clipped at the high end (>= {hi_v:.3f} V) > {x['max_v']:g} V")
        elif c_hi:
            open_.append(f"{c_hi} samples clipped at >= {hi_v:.3f} V: max_v {x['max_v']:g} V cannot be told")
        elif got["samples"] and got["max_v"] > x["max_v"]:
            ok = False
            why.append(f"max {got['max_v']:.4f} V > {x['max_v']:g} V")
        if c_lo and lo_v > x["max_v"]:                  # <= the low end, but the low end is over max_v
            open_.append(f"{c_lo} samples clipped at <= {lo_v:.3f} V: max_v {x['max_v']:g} V cannot be told")
    if x.get("volts") is not None or x.get("ripple") is not None:
        if c_lo or c_hi:
            open_.append(f"{c_lo + c_hi} samples clipped at an end of {lo_v:.3f}..{hi_v:.3f} V: "
                         "mean and ripple cannot be told")
        else:
            if x.get("volts") is not None and abs(got["mean_v"] - x["volts"]) > x["tol_v"]:
                ok = False
                why.append(f"mean {got['mean_v']:.4f} V vs {x['volts']:.4f} +- {x['tol_v']:g} V")
            if x.get("ripple") is not None and got["p2p_v"] > x["ripple"]:
                ok = False
                why.append(f"peak-to-peak {got['p2p_v']:.4f} V > {x['ripple']:g} V")
    if not ok:
        return False, got, "; ".join(why + open_)
    if open_:
        return None, got, "; ".join(open_)
    return True, got, ""


LOGIC = {"square", "level", "ends", "starts", "pulses", "i2c", "spi", "uart", "phase", "skew", "dead_time",
         "i2c_timing"}   # checks that read logic levels


def _as_logic(cap: Capture, x: dict) -> tuple[Capture | None, str]:
    """For a logic check naming analog channels: the capture with those read as
    logic at x["threshold"] (None and why when there is no threshold)."""
    if x["kind"] not in LOGIC:
        return cap, ""
    names = {c.name for c in cap.channels}
    wanted = [a for a in cap.analog if a.name in _pins_of(x) and a.name not in names]
    if not wanted:
        return cap, ""
    if x.get("threshold") is None:
        return None, (", ".join(a.name for a in wanted) + " analog: give threshold= (volts) to read it as logic")
    try:
        extra = [analog.to_logic(a, cap.meta.get("tick_hz", cap.rate), x["threshold"]) for a in wanted]
    except ValueError as e:
        return None, str(e)
    return Capture(cap.rate, cap.n_samples, cap.channels + extra, cap.meta, cap.analog, cap.events), ""


EVENT_CHECKS = {"level", "ends", "starts", "pulses", "only_moving"}   # what a line whose quanta hide changes answers
CHECKS = {"square": check_square, "level": check_level, "ends": check_ends, "starts": check_starts,
          "only_moving": check_only_moving, "pulses": check_pulses, "i2c": check_i2c, "spi": check_spi,
          "uart": check_uart, "voltage": check_voltage, "phase": check_phase, "skew": check_skew,
          "dead_time": check_dead_time, "i2c_timing": check_i2c_timing}


def _pins_of(x: dict) -> set[str]:
    out = set()
    for k in ("pin", "scl", "sda", "clk", "mosi", "miso", "cs", "a", "b", "hi", "lo"):
        if x.get(k):
            out.add(x[k])
    out |= set(x.get("levels", {}))
    return out


def _in_segment(c: dict, t: int, path: str, seg) -> bool:
    """A capture belongs to the section it was taken in (its recorded path) and that section's ancestors;
    one without a path (another recorder) to the section its start falls in."""
    if "path" in c:
        return c["path"] == path or c["path"].startswith(path + "/")
    return seg.begin <= t < (seg.end if seg.end is not None else 1 << 62)


EMPTY = object()                  # a section shorter than its bounds' uncertainty: no inner window


def _judge(cap: Capture, x: dict, pins: set[str]):
    """(ok, measured, why) of one check on one capture."""
    cap, why = _as_logic(cap, x)
    if cap is None:
        return None, {}, why
    hid = [t for t in cap.events if t.name in pins and t.hidden]
    if hid and x["kind"] not in EVENT_CHECKS:
        return None, {}, "; ".join(events.describe(t) for t in hid) + f": {x['kind']} needs every edge"
    return CHECKS[x["kind"]](cap, x)


def verify(run_dir: str | Path) -> dict:
    run_dir = Path(run_dir)
    doc = json.loads((run_dir / "run.json").read_text())
    if doc.get("format") != FORMAT:
        raise ValueError(f"{run_dir}: format {doc.get('format')!r}, expected {FORMAT!r}")
    tree = segment_tree(doc)
    segs = {s.path(): s for s in tree.walk() if s.kind != "root"}
    caps = [(c, int(c["t0"] * 1e6)) for c in doc["captures"]]
    results: list[Result] = []
    for path, spec in doc["expect"].items():
        seg = segs.get(path)
        if seg is None:
            results.append(Result(path, None, "segment", False, reason="no segment with this path in the log"))
            continue
        if _probe_section(seg):
            # opened and closed on the probe's clock: the part of every capture that holds it, cut to the section
            loaded, whys = [], set()
            for c in doc["captures"]:
                for cap in load_capture(run_dir, c):
                    w, why = _on_probe_clock(seg, cap)
                    if w is None:
                        whys.add(why)
                        continue
                    inner, outer, clipped = w
                    win = _window(cap, *outer)
                    win.meta["label"] = f"{cap.meta['label']}@{outer[0]}:{outer[1]}"
                    loaded.append((c, win, _window(cap, *inner) if inner else EMPTY, clipped))
            if not loaded:
                results.append(Result(path, None, "capture", False, reason="no capture holds the section on the "
                                      "probe's clock (" + "; ".join(sorted(whys)) + ")"))
                continue
        else:
            # a capture belongs to the section it was taken in (recorded path), or else to the one its start falls in
            inside = [c for c, t in caps if _in_segment(c, t, path, seg)]
            if not inside:
                results.append(Result(path, None, "capture", False, reason="no capture inside the segment"))
                continue
            loaded = [(c, cap, None, []) for c in inside for cap in load_capture(run_dir, c)]
        for c in dict.fromkeys(c["file"] for c, *_ in loaded):    # a loss fails the capture, whatever its parts say
            first = next(cap for cc, cap, *_ in loaded if cc["file"] == c)
            if not first.meta.get("complete", True):
                results.append(Result(path, c, "capture", False, reason=_lost(first)))
        for x in spec["checks"]:
            pins = _pins_of(x)
            hit = [(c, cap, alt, clipped) for c, cap, alt, clipped in loaded
                   if pins <= {ch.name for ch in cap.channels} | {a.name for a in cap.analog}
                   | {t.name for t in cap.events}] or (loaded if x["kind"] == "only_moving" else [])
            if not hit:
                skipped = {c["name"]: c["encoding"] for _, cap, *_ in loaded
                           for c in cap.meta.get("skipped_channels", [])}
                why = "pins not in any capture: " + ", ".join(sorted(pins))
                if pins & set(skipped):
                    why += " (" + ", ".join(f"{p} has encoding {skipped[p]!r}, not read by this version"
                                            for p in sorted(pins & set(skipped))) + ")"
                results.append(Result(path, None, x["kind"], None, x, reason=why))
                continue
            for c, cap, alt, clipped in hit:
                label = cap.meta.get("label", c["file"])
                ok, got, why = _judge(cap, x, pins)
                if alt is EMPTY:
                    ok, why = None, "the section is shorter than its bounds' uncertainty"
                elif alt is not None:
                    # the bounds are known within their uncertainty: the verdict must not depend on where
                    ok2, got2, why2 = _judge(alt, x, pins)
                    if ok2 != ok:
                        st = lambda v: Result("", None, "", v).status      # noqa: E731
                        got = {"outer": got, "inner": got2}
                        why = (f"the verdict depends on where the section's bounds fall: {st(ok)} on "
                               f"{cap.meta['window']} ({why or 'no reason'}), {st(ok2)} on {alt.meta['window']} "
                               f"({why2 or 'no reason'})")
                        ok = None
                if alt is not None:
                    got = {**got, "window": cap.meta["window"]}
                if clipped:
                    got = {**got, "clipped": clipped}
                    if ok is False:
                        why = (why + "; " if why else "") + "the section " + " and ".join(
                            {"begin": "began before", "end": "ended after"}[k] for k in clipped) + " the capture"
                if cap.meta.get("time_base_slipped"):
                    # the probe says some samples were taken late: the verdict
                    # stands (it may still read fine), a failure names it
                    got = {**got, "time_base_slipped": True}
                    if ok is False:
                        why = (why + "; " if why else "") + "the probe reported a time base slip in this capture"
                results.append(Result(path, label, x["kind"], ok, x, got, why))
    for e in doc["log"]:                 # what the probe dropped of its own lines: the sections are not known
        if e["src"] == "lost":
            results.append(Result("", None, "probe_log", False, reason=e["text"]))
    # marker problems anywhere in the tree fail the run: a wrong structure means
    # captures may be checked against the wrong expectations
    for path, seg in segs.items():
        for issue in seg.issues:
            results.append(Result(path, None, "markers", False, reason=json.dumps(issue)))
    for issue in tree.issues:
        results.append(Result("", None, "markers", False, reason=json.dumps(issue)))
    ev = [{k: e[k] for k in ("t", "src", "text", "probe_ns") if k in e} for e in doc["log"]]
    count = {k: sum(r.status == k for r in results) for k in STATUSES}
    return {"run": str(run_dir), "meta": doc.get("meta", {}), "results": [r.__dict__ for r in results],
            "summary": {**count,
                        "segments": len(segs), "captures": len(caps)},
            "tree_issues": tree.issues, "log": ev}


def dumps(report: dict) -> str:
    """The report as JSON (numpy values converted)."""
    from ._engine.export import dumps as _dumps
    return _dumps(report)


def failed(report: dict, allow_unchecked: bool = False) -> bool:
    """Whether the run fails: an NG, or (unless allowed) a check that could not be made."""
    s = report["summary"]
    return bool(s["ng"] or (s["unchecked"] and not allow_unchecked))


MARKS = {"ok": "OK", "ng": "NG", "unchecked": "--", "measured": "ME"}


def lines(report: dict, ok: bool = False) -> list[str]:
    """One line per result, as `wireskein verify` prints them, for people (the form is not promised:
    programs read the JSON report); only the ones that are not "ok" unless ok=True."""
    out = []
    for r in report["results"]:
        if r["status"] == "ok" and not ok:
            continue
        out.append(f"{MARKS[r['status']]}  {r['path']}  {r['check']}  {r['capture'] or ''}  {r['reason']}".rstrip())
    return out


def summary_line(report: dict) -> str:
    s = report["summary"]
    return (f"{s['ok']} ok, {s['ng']} ng, {s['unchecked']} unchecked, {s['measured']} measured "
            f"({s['segments']} segments, {s['captures']} captures)")


def junit(report: dict, allow_unchecked: bool = False) -> str:
    """JUnit XML: ng -> failure; unchecked -> failure (skipped when allowed); measured -> passed, with the values."""
    from xml.sax.saxutils import escape, quoteattr
    rs = report["results"]
    fail = [r for r in rs if r["status"] == "ng" or (r["status"] == "unchecked" and not allow_unchecked)]
    skip = [r for r in rs if r["status"] == "unchecked" and allow_unchecked]
    out = [f'<testsuite name={quoteattr("wireskein verify " + report["run"])} tests="{len(rs)}" '
           f'failures="{len(fail)}" skipped="{len(skip)}">']
    for r in rs:
        name = f'{r["check"]} [{r["capture"] or "-"}]'
        out.append(f'  <testcase classname={quoteattr(r["path"])} name={quoteattr(name)}>')
        values = escape(json.dumps(r["measured"], default=str))
        if r in fail:
            why = r["reason"] if r["status"] == "ng" else f"unchecked: {r['reason']}"
            out.append(f'    <failure message={quoteattr(why)}>{values}</failure>')
        elif r in skip:
            out.append(f'    <skipped message={quoteattr(r["reason"])}/>')
        elif r["status"] == "measured":
            out.append(f'    <system-out>{values}</system-out>')
        out.append("  </testcase>")
    out.append("</testsuite>")
    return "\n".join(out)
