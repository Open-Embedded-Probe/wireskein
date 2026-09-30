"""Value Change Dump (.vcd) <-> the edge-list model.

A VCD holds only the changes, so channels at different rates need no
padding: GTKWave shows each one as it is. What VCD cannot say (the exact tick
clock, each channel's step and phase, the capture metadata, an analog
channel's rate and start) goes into a `$comment wireskein {...} $end` block
that other tools ignore and read_vcd uses to restore them. Attachments and
notes have no place in a VCD.

Reading a VCD from elsewhere (Saleae, DSView, simulators): the tick is the
greatest common divisor of the change times, so a 100 MHz export at a 1 ns
timescale gives 100 MHz ticks. 1-bit variables become logic channels,
vectors one channel per bit (NAME[k]); x and z read as 0 (counted in the
metadata). `real` variables become analog channels only when their changes
are evenly spaced (a sampled signal); others are skipped and named, since
anything else would need samples that were never taken.
"""

from __future__ import annotations

import json
import math
import re
from fractions import Fraction
from pathlib import Path

import numpy as np

from .model import AnalogTrace, Capture, Channel

MARK = "wireskein "
UNITS = [("s", Fraction(1)), ("ms", Fraction(1, 10**3)), ("us", Fraction(1, 10**6)), ("ns", Fraction(1, 10**9)),
         ("ps", Fraction(1, 10**12)), ("fs", Fraction(1, 10**15))]


def _timescale(tick_hz: Fraction) -> tuple[str, Fraction]:
    """The largest VCD unit (1/10/100 x s..fs) the tick period is a whole multiple of."""
    period = 1 / Fraction(tick_hz)
    for name, unit in UNITS:
        for m in (100, 10, 1):
            u = unit * m
            if (period / u).denominator == 1:
                return f"{m} {name}", u
    return "1 fs", Fraction(1, 10**15)               # not exact: times are rounded to fs


def _ids():
    """VCD identifiers: printable ASCII 33..126, then two characters, ..."""
    n = 0
    while True:
        k, s = n, ""
        while True:
            s = chr(33 + k % 94) + s
            k = k // 94 - 1
            if k < 0:
                break
        yield s
        n += 1


def write_vcd(path: Path, cap: Capture, **meta) -> Path:
    tick = Fraction(cap.meta.get("tick_hz", cap.rate)).limit_denominator(10**9)
    scale_name, unit = _timescale(tick)
    per_tick = (1 / tick) / unit                     # timescale units per tick (a whole number when exact)
    ids = _ids()
    logic = [(next(ids), c) for c in cap.channels]
    analog = []
    for a in cap.analog:
        v = a.volts()
        analog.append((next(ids), a, v if v is not None else a.values.astype(np.float64), v is not None))
    meta.pop("capture_id", None)
    extra = {"format": "wireskein-vcd-extra/1", "id": cap.meta.get("capture_id"),
             "tick_hz": [tick.numerator, tick.denominator], "ticks": cap.n_samples,
             "channels": {c.name: {"step": c.step, "phase": c.phase, **({"acquisition": c.acquisition} if c.acquisition else {})}
                          for _, c in logic},
             "analog": {a.name: {"rate_hz": [a.rate_hz.numerator, a.rate_hz.denominator],
                                 "t0_ticks": [a.t0_ticks.numerator, a.t0_ticks.denominator],
                                 "unit": a.unit if volts else "raw", "acquisition": a.acquisition}
                        for _, a, _, volts in analog},
             "meta": meta}
    # every change as (time in timescale units, order, text)
    times, texts = [], []
    for vid, c in logic:
        e = c.edges
        lv = c.initial ^ ((np.arange(len(e)) + 1) & 1)
        times.append(np.round(e.astype(object) * per_tick).astype(np.int64) if per_tick.denominator != 1
                     else e * int(per_tick))
        texts.append(np.array([f"{int(x)}{vid}" for x in lv], dtype=object))
    for vid, a, vals, _ in analog:
        t_ticks = float(a.t0_ticks) + np.arange(len(vals)) * float(Fraction(tick) / a.rate_hz)
        t = np.round(t_ticks * float(per_tick)).astype(np.int64)
        keep = t >= 0
        times.append(t[keep])
        texts.append(np.array([f"r{x:.9g} {vid}" for x in vals[keep]], dtype=object))
    allt = np.concatenate(times) if times else np.zeros(0, np.int64)
    allx = np.concatenate(texts) if texts else np.zeros(0, object)
    order = np.argsort(allt, kind="stable")
    with open(path, "w") as f:
        f.write("$version wireskein $end\n")
        f.write(f"$comment {MARK}{json.dumps(extra, default=str)} $end\n")
        f.write(f"$timescale {scale_name} $end\n$scope module capture $end\n")
        for vid, c in logic:
            f.write(f"$var wire 1 {vid} {c.name} $end\n")
        for vid, a, _, _ in analog:
            f.write(f"$var real 64 {vid} {a.name} $end\n")
        f.write("$upscope $end\n$enddefinitions $end\n#0\n$dumpvars\n")
        for vid, c in logic:
            f.write(f"{c.initial}{vid}\n")
        f.write("$end\n")
        last = 0
        for i in order:
            t = int(allt[i])
            if t != last:
                f.write(f"#{t}\n")
                last = t
            f.write(allx[i] + "\n")
        end = round(cap.n_samples * per_tick)
        if end > last:
            f.write(f"#{end}\n")
    return path


_TS = re.compile(r"\s*(\d+)\s*(s|ms|us|ns|ps|fs)\s*")


def read_vcd(path: Path) -> Capture:
    text = Path(path).read_text(errors="replace")
    head, sep, body = text.partition("$enddefinitions")
    if not sep:
        raise ValueError(f"{path}: no $enddefinitions: not a VCD")
    extra = {}
    for m in re.finditer(r"\$comment\s+(.*?)\$end", head, re.S):
        c = m.group(1).strip()
        if c.startswith(MARK):
            try:
                extra = json.loads(c[len(MARK):])
            except ValueError:
                extra = {}
    if extra.get("format") != "wireskein-vcd-extra/1":
        extra = {}                                       # another version's, or none: read as a plain VCD
    ts = re.search(r"\$timescale\s+(.*?)\$end", head, re.S)
    unit = Fraction(1, 10**9)
    if ts:
        m = _TS.fullmatch(ts.group(1).replace("\n", " "))
        if not m:
            raise ValueError(f"{path}: timescale {ts.group(1).strip()!r}")
        unit = int(m.group(1)) * dict(UNITS)[m.group(2)]
    vars_ = {}                                           # id -> (kind, width, name)
    order = []
    scope = []
    for tok in re.finditer(r"\$(scope|upscope|var)\b(.*?)\$end", head, re.S):
        kind, rest = tok.group(1), tok.group(2).split()
        if kind == "scope":
            scope.append(rest[1] if len(rest) > 1 else "")
        elif kind == "upscope":
            scope and scope.pop()
        else:
            vtype, width, vid, name = rest[0], int(rest[1]), rest[2], rest[3]
            if vid in vars_:
                continue                                 # the same signal under another name: keep the first
            vars_[vid] = (vtype, width, name)
            order.append(vid)
    changes = {vid: [] for vid in vars_}                 # id -> [(time, value text)]
    t = 0
    xz = 0
    words = body.split()
    i = 1 if words and words[0] == "$end" else 0
    while i < len(words):
        w = words[i]
        i += 1
        c = w[0]
        if c == "#":
            t = int(w[1:])
        elif c in "01xXzZ" and len(w) > 1:
            if w[1:] in changes:
                changes[w[1:]].append((t, c))
        elif c in "bB":
            vid = words[i]
            i += 1
            if vid in changes:
                changes[vid].append((t, w[1:]))
        elif c in "rR":
            vid = words[i]
            i += 1
            if vid in changes:
                changes[vid].append((t, w[1:]))
        # $dumpvars / $end / $comment ... and others: skipped
    all_t = [x for ch in changes.values() for x, _ in ch]
    if extra.get("tick_hz"):
        tick = Fraction(*extra["tick_hz"])
        per_tick = (1 / tick) / unit
    else:
        g = 0
        for x in all_t:
            g = math.gcd(g, x)
        per_tick = Fraction(max(g, 1))
        tick = 1 / (per_tick * unit)
    t_end = max(all_t, default=0)
    n_ticks = int(extra.get("ticks") or math.ceil(t_end / per_tick) + 1)
    if n_ticks > 1 << 40:
        raise ValueError(f"{path}: {n_ticks} ticks at {float(tick):g} Hz: too many")
    channels, analog, skipped = [], [], []

    def to_ticks(times):
        return np.array([int(Fraction(x) / per_tick) if (Fraction(x) / per_tick).denominator == 1
                         else round(Fraction(x) / per_tick) for x in times], dtype=np.int64)

    def logic(name, seq):
        nonlocal xz
        lv = []
        for x, v in seq:
            if v in "xXzZ":
                xz += 1
                v = "0"
            lv.append((x, int(v)))
        initial = lv[0][1] if lv and lv[0][0] == 0 else 0
        e, cur = [], initial
        for x, v in lv:
            if v != cur:
                e.append(x)
                cur = v
        own = extra.get("channels", {}).get(name, {})
        channels.append(Channel(name, initial, to_ticks(e), int(own.get("step", 1)), int(own.get("phase", 0)),
                                own.get("acquisition", {})))

    for vid in order:
        vtype, width, name = vars_[vid]
        seq = changes[vid]
        if vtype == "real":
            info = extra.get("analog", {}).get(name)
            ts_ = np.array([x for x, _ in seq], dtype=np.int64)
            vals = np.array([float(v) for _, v in seq])
            if info:
                rate, t0 = Fraction(*info["rate_hz"]), Fraction(*info["t0_ticks"])
            elif len(ts_) >= 2 and len(set(np.diff(ts_).tolist())) == 1:
                rate = 1 / (Fraction(int(ts_[1] - ts_[0])) * unit)
                t0 = Fraction(int(ts_[0])) / per_tick
            else:
                skipped.append({"name": name, "encoding": "vcd real, not evenly spaced"})
                continue
            unit_v = info.get("unit", "V") if info else "V"
            if unit_v == "raw":                          # written from raw values without a conversion
                analog.append(AnalogTrace(name, vals, rate, t0, "analog-f32", unit="raw",
                                          acquisition=info.get("acquisition", {})))
            else:
                analog.append(AnalogTrace(name, vals.astype(np.float32), rate, t0, "analog-f32", unit=unit_v,
                                          acquisition=(info or {}).get("acquisition", {})))
        elif width == 1:
            logic(name, seq)
        else:
            base = re.sub(r"\[.*\]$", "", name)
            for k in range(width):
                bits = []
                for x, v in seq:
                    v = v.rjust(width, "0" if v[:1] in "01" else v[:1])
                    bits.append((x, v[width - 1 - k]))
                logic(f"{base}[{k}]", bits)
    meta = {"vcd_file": str(path), "tick_hz": tick, **extra.get("meta", {})}
    if extra.get("id"):
        meta["capture_id"] = extra["id"]
    if xz:
        meta["vcd_x_or_z"] = xz
    if skipped:
        meta["vcd_not_read"] = skipped                   # named, not refused: the VCD itself keeps them
    return Capture(float(tick), n_ticks, channels, meta=meta, analog=analog)
