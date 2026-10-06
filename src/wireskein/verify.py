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
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

from ._engine import analog, markers
from ._engine.model import AnalogTrace, Capture, Channel
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


def load_capture(run_dir: Path, c: dict) -> Capture:
    from ._engine import fileio
    cap = fileio.load(run_dir / c["file"])
    cap.meta.update(file=c["file"], t0=c["t0"])
    return cap


def segment_tree(doc: dict):
    lines = [(e["text"], int(e["t"] * 1e6), int(e["t"] * 1e6)) for e in doc["log"] if e["src"] == "marker"]
    end = int(max([e["t"] for e in doc["log"]] + [c["t0"] for c in doc["captures"]] + [0]) * 1e6) + 1
    return markers.build(lines, markers.load_dialects()["heading"], end)


# ---------------- checks ----------------

def _ch(cap: Capture, pin: str) -> Channel | None:
    try:
        return cap.channel(pin)
    except KeyError:
        return None


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
    if ch is None:
        return None, {}, "pin not captured"
    got = {"initial": ch.initial, "edges": int(len(ch.edges)),
           "high_fraction": float(np.mean(ch.level_at(np.arange(0, cap.n_samples, max(1, cap.n_samples // 4096)))))}
    ok = len(ch.edges) == 0 and ch.initial == x["value"]
    return ok, got, "" if ok else f"not constant {x['value']}"


def _levels(cap, want: dict, at_end: bool):
    got, bad, missing = {}, [], []
    for pin, v in want.items():
        ch = _ch(cap, pin)
        if ch is None:
            missing.append(pin)
            continue
        got[pin] = _level_at(ch, cap.n_samples - 1) if at_end else ch.initial
        if got[pin] != v:
            bad.append(f"{pin}={got[pin]} (want {v})")
    if missing and not got:
        return None, got, "pins not captured: " + ", ".join(missing)
    return not bad, got, "; ".join(bad)


def check_ends(cap, x):
    return _levels(cap, x["levels"], True)


def check_starts(cap, x):
    return _levels(cap, x["levels"], False)


def check_only_moving(cap, x):
    moved = {c.name: int(len(c.edges)) for c in cap.channels if len(c.edges) and c.name not in x["pins"]}
    return not moved, {"unexpected": moved}, "" if not moved else "other pins moved: " + ", ".join(moved)


def check_pulses(cap, x):
    ch = _ch(cap, x["pin"])
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


LOGIC = {"square", "level", "ends", "starts", "pulses", "i2c", "spi", "uart"}   # checks that read logic levels


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
    return Capture(cap.rate, cap.n_samples, cap.channels + extra, cap.meta, cap.analog), ""


CHECKS = {"square": check_square, "level": check_level, "ends": check_ends, "starts": check_starts,
          "only_moving": check_only_moving, "pulses": check_pulses, "i2c": check_i2c, "spi": check_spi,
          "uart": check_uart, "voltage": check_voltage}


def _pins_of(x: dict) -> set[str]:
    out = set()
    for k in ("pin", "scl", "sda", "clk", "mosi", "miso", "cs"):
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
        # a capture belongs to the section it was taken in (recorded path), or else to the one its start falls in
        inside = [c for c, t in caps if _in_segment(c, t, path, seg)]
        if not inside:
            results.append(Result(path, None, "capture", False, reason="no capture inside the segment"))
            continue
        loaded = [(c, load_capture(run_dir, c)) for c in inside]
        for x in spec["checks"]:
            pins = _pins_of(x)
            hit = [(c, cap) for c, cap in loaded
                   if pins <= {ch.name for ch in cap.channels} | {a.name for a in cap.analog}] or \
                  ([(c, cap) for c, cap in loaded] if x["kind"] == "only_moving" else [])
            if not hit:
                skipped = {c["name"]: c["encoding"] for _, cap in loaded for c in cap.meta.get("skipped_channels", [])}
                why = "pins not in any capture: " + ", ".join(sorted(pins))
                if pins & set(skipped):
                    why += " (" + ", ".join(f"{p} has encoding {skipped[p]!r}, not read by this version"
                                            for p in sorted(pins & set(skipped))) + ")"
                results.append(Result(path, None, x["kind"], None, x, reason=why))
                continue
            for c, cap in hit:
                cap, why = _as_logic(cap, x)
                if cap is None:
                    results.append(Result(path, c["file"], x["kind"], None, x, reason=why))
                    continue
                ok, got, why = CHECKS[x["kind"]](cap, x)
                if cap.meta.get("time_base_slipped"):
                    # the probe says some samples were taken late: the verdict
                    # stands (it may still read fine), a failure names it
                    got = {**got, "time_base_slipped": True}
                    if ok is False:
                        why = (why + "; " if why else "") + "the probe reported a time base slip in this capture"
                results.append(Result(path, c["file"], x["kind"], ok, x, got, why))
    # marker problems anywhere in the tree fail the run: a wrong structure means
    # captures may be checked against the wrong expectations
    for path, seg in segs.items():
        for issue in seg.issues:
            results.append(Result(path, None, "markers", False, reason=json.dumps(issue)))
    for issue in tree.issues:
        results.append(Result("", None, "markers", False, reason=json.dumps(issue)))
    ev = [{"t": e["t"], "src": e["src"], "text": e["text"]} for e in doc["log"]]
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
