"""sigrok session files (.sr) <-> the edge-list model.

A .sr has one sample rate for all channels. A channel sampled slower (step > 1)
is written repeated to the tick rate, and wireskein.json inside the zip keeps
each channel's step / phase and the exact tick clock; sigrok ignores the file,
read_sr uses it to give those channels their own step again.
"""

from __future__ import annotations

import configparser
import json
import re
import zipfile
from fractions import Fraction
from pathlib import Path

import numpy as np

from .model import AnalogTrace, Capture, Channel, edges_from_dense

_UNITS = {"hz": 1, "khz": 1e3, "mhz": 1e6, "ghz": 1e9}


def _parse_rate(text: str) -> float:
    m = re.fullmatch(r"\s*([\d.]+)\s*([kKmMgG]?[hH]z)\s*", text)
    if not m:
        raise ValueError(f"unknown samplerate {text!r}")
    return float(m.group(1)) * _UNITS[m.group(2).lower()]


def read_sr(path: Path) -> Capture:
    with zipfile.ZipFile(path) as z:
        cfg = configparser.ConfigParser()
        cfg.read_string(z.read("metadata").decode())
        dev = cfg["device 1"]
        rate = _parse_rate(dev["samplerate"])
        unitsize = int(dev.get("unitsize", "1"))
        prefix = dev.get("capturefile", "logic-1")
        names = {int(k[5:]): v for k, v in dev.items() if re.fullmatch(r"probe\d+", k)}
        anames = {int(k[6:]): v for k, v in dev.items() if re.fullmatch(r"analog\d+", k)}
        total = int(dev.get("total probes", unitsize * 8))
        chunks = sorted(
            (n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
            key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0,
        )
        raw = b"".join(z.read(n) for n in chunks)
        avals = {}
        for idx in anames:
            parts = sorted((n for n in z.namelist() if n.startswith(f"analog-1-{idx}-")), key=lambda n: int(n.rsplit("-", 1)[1]))
            avals[idx] = np.frombuffer(b"".join(z.read(n) for n in parts), "<f4")
        extra = json.loads(z.read(EXTRA)) if EXTRA in z.namelist() else {}
        if extra.get("format") != "wireskein-sr-extra/1":
            extra = {}
        rawfiles = {a["file"]: z.read(a["file"]) for a in extra.get("analog", {}).values() if a.get("file") in z.namelist()}
        listed = set(extra.get("extras", []))
        carried = {n: z.read(n) for n in z.namelist() if n.startswith(("attach/", "notes/")) or n in listed}
    dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
    data = np.frombuffer(raw, dtype=dtype)
    n_ticks = len(data) if len(data) else max((len(v) for v in avals.values()), default=0)
    steps = extra.get("channels", {})
    meta = {"sr_file": str(path), "unitsize": unitsize}
    sr_rate = Fraction(round(rate))
    if extra.get("tick_hz"):
        meta["tick_hz"] = Fraction(*extra["tick_hz"])
        rate = float(meta["tick_hz"])
    meta.update(extra.get("meta", {}))
    if extra.get("id"):
        meta["capture_id"] = extra["id"]
    if carried:
        meta["extras"] = carried
    # Every declared probe becomes a channel, named or not, when none is named:
    # unnamed probes are real (usually static) inputs and useful negatives. When
    # some are named (sigrok names the ones it captured), only those.
    bits = sorted(b - 1 for b in names) if names else range(min(total, unitsize * 8))
    channels = []
    for bit in bits:
        if bit >= unitsize * 8:
            continue
        name = names.get(bit + 1, f"bit{bit}")
        st = steps.get(name, {})
        step, phase = int(st.get("step", 1)), int(st.get("phase", 0))
        lv = ((data[phase::step] >> bit) & 1).astype(np.uint8)      # the samples the channel really has
        initial, edges = edges_from_dense(lv, step, phase)
        if phase:
            initial = int((data[0] >> bit) & 1)
        channels.append(Channel(name, initial, edges, step, phase, st.get("acquisition", {})))
    analog = []
    for idx, name in sorted(anames.items()):
        info = extra.get("analog", {}).get(name)
        if info and info.get("file") in rawfiles:           # written by write_sr: the raw values come back
            dt = {8: np.uint8, 16: "<u2", 32: "<u4"}[info["width"]]
            analog.append(AnalogTrace(name, np.frombuffer(rawfiles[info["file"]], dt), Fraction(*info["rate_hz"]),
                                      Fraction(*info["t0_ticks"]), "analog", info["width"], info.get("value_bits"),
                                      info.get("zero"), info.get("scale_nv"), "V", info.get("acquisition", {})))
        elif info:                                          # analog-f32 written by write_sr: its own rate
            per = int(Fraction(meta.get("tick_hz", sr_rate)) / Fraction(*info["rate_hz"]))
            t0 = int(Fraction(*info["t0_ticks"]))
            v = avals[idx][t0::per][: info["n"]]
            analog.append(AnalogTrace(name, v, Fraction(*info["rate_hz"]), Fraction(*info["t0_ticks"]), "analog-f32",
                                      unit=info.get("unit", "V"), acquisition=info.get("acquisition", {})))
        else:
            analog.append(AnalogTrace(name, avals[idx], Fraction(meta.get("tick_hz", sr_rate)), Fraction(0), "analog-f32"))
    return Capture(rate, n_ticks, channels, meta=meta, analog=analog)


EXTRA = "wireskein/sr-extra.json"          # wireskein-format §6
CHUNK = 4 << 20             # ticks per logic-1-N / analog-1-K-N file


def write_sr(path: Path, cap: Capture, **meta) -> Path:
    """Channels in the order given, one bit each; slow channels repeated to the
    tick rate (wireskein.json keeps their real step). Analog channels are
    written as volts when their samples land on ticks (repeated to the tick
    rate, NaN outside them); raw values and settings go into wireskein.json and
    wireskein/analog-K.raw, so reading the .sr back restores them."""
    n_ch = len(cap.channels)
    unitsize = 1 if n_ch <= 8 else 2 if n_ch <= 16 else 4 if n_ch <= 32 else 0
    if not unitsize:
        raise ValueError(f"{n_ch} channels: a .sr holds up to 32 here")
    dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
    tick = Fraction(cap.meta.get("tick_hz", cap.rate)).limit_denominator(10**9)
    held = []                                               # (index, trace, ticks per sample, t0, volts)
    for j, a in enumerate(cap.analog):
        per, t0 = tick / a.rate_hz, a.t0_ticks
        if per.denominator != 1 or t0.denominator != 1 or t0 < 0:
            raise ValueError(f"not writing {path}: analog {a.name} ({float(a.rate_hz):g} Hz from tick {float(t0):g}) "
                             f"does not land on the {float(tick):g} Hz ticks a .sr has")
        v = a.volts()
        if v is None:
            raise ValueError(f"not writing {path}: analog {a.name} has no conversion to volts (zero / scale_nv)")
        held.append((n_ch + j + 1, a, int(per), int(t0), v.astype("<f4")))
    lines = ["[global]", "sigrok version=0.5.2", "", "[device 1]", "capturefile=logic-1",
             f"total probes={n_ch}", f"samplerate={round(tick)} Hz", f"total analog={len(held)}"]
    lines += [f"probe{k + 1}={c.name}" for k, c in enumerate(cap.channels)]
    lines += [f"analog{idx}={a.name}" for idx, a, *_ in held] + [f"unitsize={unitsize}", ""]
    meta.pop("capture_id", None)
    extra = {"format": "wireskein-sr-extra/1", "id": cap.meta.get("capture_id"), "tick_hz": [tick.numerator, tick.denominator],
             "channels": {c.name: {"step": c.step, "phase": c.phase, **({"acquisition": c.acquisition} if c.acquisition else {})}
                          for c in cap.channels if c.step != 1 or c.phase or c.acquisition},
             "analog": {}, "meta": meta, "extras": sorted(cap.meta.get("extras", {}))}
    for name in extra["extras"]:
        if name in ("version", "metadata", EXTRA) or name.startswith(("logic-1", "analog-1-", "wireskein/")):
            raise ValueError(f"not writing {path}: the entry {name!r} would clash with the .sr's own")
    for idx, a, per, t0, _ in held:
        info = {"encoding": a.encoding, "n": len(a.values), "rate_hz": [a.rate_hz.numerator, a.rate_hz.denominator],
                "t0_ticks": [a.t0_ticks.numerator, a.t0_ticks.denominator], "acquisition": a.acquisition}
        if a.encoding == "analog":
            info.update(width=a.width, value_bits=a.value_bits, zero=a.zero, scale_nv=a.scale_nv,
                        file=f"wireskein/analog-{idx}.raw")
        else:
            info["unit"] = a.unit
        extra["analog"][a.name] = info
    n = cap.n_samples
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr("version", "2")
        z.writestr("metadata", "\n".join(lines))
        z.writestr(EXTRA, json.dumps(extra, indent=1, default=str))
        for name, data in cap.meta.get("extras", {}).items():      # attachments, notes and other parts of a WireSkein file
            z.writestr(name, data)
        for idx, a, *_ in held:
            if a.encoding == "analog":
                dt = {8: np.uint8, 16: "<u2", 32: "<u4"}[a.width]
                z.writestr(f"wireskein/analog-{idx}.raw", np.asarray(a.values, dt).tobytes())
        for part, s0 in enumerate(range(0, n, CHUNK), start=1):
            s1 = min(s0 + CHUNK, n)
            if n_ch:
                out = np.zeros(s1 - s0, dtype)
                for k, c in enumerate(cap.channels):
                    out |= _levels(c, s0, s1).astype(dtype) << k
                z.writestr(f"logic-1-{part}", out.tobytes())
            for idx, a, per, t0, v in held:
                k = (np.arange(s0, s1) - t0) // per             # the sample each tick shows (held)
                ok = (k >= 0) & (k < len(v))
                seg = np.full(s1 - s0, np.nan, "<f4")
                seg[ok] = v[k[ok]]
                z.writestr(f"analog-1-{idx}-{part}", seg.tobytes())
    return path


def _levels(ch: Channel, s0: int, s1: int) -> np.ndarray:
    """0/1 per tick in [s0, s1)."""
    lo = int(np.searchsorted(ch.edges, s0, side="right"))       # edges up to s0 set the level at s0
    hi = int(np.searchsorted(ch.edges, s1, side="left"))        # edges inside (s0, s1)
    toggles = np.zeros(s1 - s0, np.uint8)
    np.add.at(toggles, ch.edges[lo:hi] - s0, 1)
    return (int(ch.initial ^ (lo & 1)) ^ (np.cumsum(toggles, dtype=np.uint8) & 1)).astype(np.uint8)
