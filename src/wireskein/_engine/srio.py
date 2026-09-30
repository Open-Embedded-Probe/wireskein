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

from .model import Capture, Channel, edges_from_dense

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
        total = int(dev.get("total probes", unitsize * 8))
        chunks = sorted(
            (n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
            key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0,
        )
        raw = b"".join(z.read(n) for n in chunks)
        extra = json.loads(z.read(EXTRA)) if EXTRA in z.namelist() else {}
    dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
    data = np.frombuffer(raw, dtype=dtype)
    steps = extra.get("channels", {})
    meta = {"source": str(path), "unitsize": unitsize}
    if extra.get("tick_hz"):
        meta["tick_hz"] = Fraction(*extra["tick_hz"])
        rate = float(meta["tick_hz"])
    meta.update(extra.get("meta", {}))
    # Every probe the file declares becomes a channel, named or not: unnamed
    # probes are real (usually static) inputs and are useful negatives.
    channels = []
    for bit in range(min(total, unitsize * 8)):
        name = names.get(bit + 1, f"bit{bit}")
        st = steps.get(name, {})
        step, phase = int(st.get("step", 1)), int(st.get("phase", 0))
        bits = ((data[phase::step] >> bit) & 1).astype(np.uint8)      # the samples the channel really has
        initial, edges = edges_from_dense(bits, step, phase)
        if phase:
            initial = int((data[0] >> bit) & 1)
        channels.append(Channel(name, initial, edges, step, phase))
    return Capture(rate, len(data), channels, meta=meta)


EXTRA = "wireskein.json"
CHUNK = 4 << 20             # ticks per logic-1-N file


def write_sr(path: Path, cap: Capture, **meta) -> Path:
    """Channels in the order given, one bit each; slow channels repeated to the
    tick rate (wireskein.json keeps their real step)."""
    n_ch = len(cap.channels)
    unitsize = 1 if n_ch <= 8 else 2 if n_ch <= 16 else 4 if n_ch <= 32 else 0
    if not unitsize:
        raise ValueError(f"{n_ch} channels: a .sr holds up to 32 here")
    dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
    tick = Fraction(cap.meta.get("tick_hz", cap.rate)).limit_denominator(10**9)
    lines = ["[global]", "sigrok version=0.5.2", "", "[device 1]", "capturefile=logic-1",
             f"total probes={n_ch}", f"samplerate={round(tick)} Hz", "total analog=0"]
    lines += [f"probe{k + 1}={c.name}" for k, c in enumerate(cap.channels)] + [f"unitsize={unitsize}", ""]
    extra = {"format": "wireskein-sr-extra/0", "tick_hz": [tick.numerator, tick.denominator],
             "channels": {c.name: {"step": c.step, "phase": c.phase} for c in cap.channels if c.step != 1 or c.phase},
             "meta": meta}
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr("version", "2")
        z.writestr("metadata", "\n".join(lines))
        z.writestr(EXTRA, json.dumps(extra, indent=1))
        for part, s0 in enumerate(range(0, cap.n_samples, CHUNK), start=1):
            s1 = min(s0 + CHUNK, cap.n_samples)
            out = np.zeros(s1 - s0, dtype)
            for k, c in enumerate(cap.channels):
                out |= _levels(c, s0, s1).astype(dtype) << k
            z.writestr(f"logic-1-{part}", out.tobytes())
    return path


def _levels(ch: Channel, s0: int, s1: int) -> np.ndarray:
    """0/1 per tick in [s0, s1)."""
    lo = int(np.searchsorted(ch.edges, s0, side="right"))       # edges up to s0 set the level at s0
    hi = int(np.searchsorted(ch.edges, s1, side="left"))        # edges inside (s0, s1)
    toggles = np.zeros(s1 - s0, np.uint8)
    np.add.at(toggles, ch.edges[lo:hi] - s0, 1)
    return (int(ch.initial ^ (lo & 1)) ^ (np.cumsum(toggles, dtype=np.uint8) & 1)).astype(np.uint8)
