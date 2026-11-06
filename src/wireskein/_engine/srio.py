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
        want = [b for b in (sorted(b - 1 for b in names) if names else range(min(total, unitsize * 8)))
                if b < unitsize * 8]
        stepped = any(int(c.get("step", 1)) != 1 for c in extra.get("channels", {}).values())
        if stepped:          # channels of their own step: their samples picked from the whole
            data = np.frombuffer(b"".join(z.read(n) for n in chunks), dtype=dtype)
            n_logic = len(data)
        else:                # a block at a time, the changes kept: a capture of any length
            data = None
            initial0, edges0, n_logic = _changes(z, chunks, unitsize, dtype, want)
    n_ticks = n_logic if n_logic else max((len(v) for v in avals.values()), default=0)
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
        if data is None:
            initial, edges = initial0.get(bit, 0), edges0[bit]
        else:
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


BLOCK = 1 << 24          # samples read at a time: a chunk of a .sr may be the whole capture


def _blocks(z, chunks: list[str], unitsize: int, dtype, start: int, end: int | None):
    """The samples of ticks [start, end) of the chunks, BLOCK at a time, decompressed as they are read:
    (the block's first tick from start, its samples)."""
    pos = 0
    for name in chunks:
        n = z.getinfo(name).file_size // unitsize
        if end is not None and pos >= end:
            return
        if pos + n <= start:
            pos += n
            continue
        with z.open(name) as f:
            k = pos
            while True:
                if end is not None and k >= end:
                    break
                raw = f.read(BLOCK * unitsize)
                if not raw:
                    break
                data = np.frombuffer(raw, dtype=dtype)
                lo, hi = max(0, start - k), len(data) if end is None else min(len(data), end - k)
                if hi > lo:
                    yield k + lo - start, data[lo:hi]
                k += len(data)
        pos += n


def read_sr_changes(path: Path, start: int = 0, end: int | None = None) -> Capture | None:
    """A logic-only .sr read a chunk at a time, keeping each channel's changes only: a capture far larger than memory
    as samples (a long stream at a high rate) is read in the memory its changes take. Ticks [start, end) only, tick 0
    at start (an excerpt). None when the file is not one this reads (analog channels, channels of their own step:
    read_sr reads those)."""
    with zipfile.ZipFile(path) as z:
        cfg = configparser.ConfigParser()
        cfg.read_string(z.read("metadata").decode())
        dev = cfg["device 1"]
        if any(re.fullmatch(r"analog\d+", k) for k in dev):
            return None
        if EXTRA in z.namelist():
            extra = json.loads(z.read(EXTRA))
            if extra.get("analog") or any(int(c.get("step", 1)) != 1 for c in extra.get("channels", {}).values()):
                return None
        rate = _parse_rate(dev["samplerate"])
        unitsize = int(dev.get("unitsize", "1"))
        prefix = dev.get("capturefile", "logic-1")
        names = {int(k[5:]): v for k, v in dev.items() if re.fullmatch(r"probe\d+", k)}
        total = int(dev.get("total probes", unitsize * 8))
        chunks = sorted((n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
                        key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0)
        dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
        bits = [b for b in (sorted(b - 1 for b in names) if names else range(min(total, unitsize * 8)))
                if b < unitsize * 8]
        initial, edges, n_ticks = _changes(z, chunks, unitsize, dtype, bits, start, end)
    channels = [Channel(names.get(b + 1, f"bit{b}"), initial.get(b, 0), edges[b]) for b in bits]
    meta = {"sr_file": str(path), "unitsize": unitsize}
    return Capture(rate, max(0, n_ticks), channels, meta=meta)


def _changes(z, chunks, unitsize, dtype, bits, start=0, end=None):
    """Each bit's (initial level, changes) and the ticks, read a block at a time: the samples are never all held."""
    edges: dict[int, list[np.ndarray]] = {b: [] for b in bits}
    initial: dict[int, int] = {}
    last: dict[int, int] = {}
    n_ticks = 0
    for first, data in _blocks(z, chunks, unitsize, dtype, start, end):
        n_ticks = first + len(data)
        for b in bits:
            lv = ((data >> b) & 1).astype(np.int8)
            if b not in initial:
                initial[b] = last[b] = int(lv[0])
            ch = np.flatnonzero(np.diff(lv)) + 1
            if lv[0] != last[b]:                     # a change across the blocks' seam
                ch = np.concatenate(([0], ch))
            if len(ch):
                edges[b].append(ch.astype(np.int64) + first)
            last[b] = int(lv[-1])
    return initial, {b: (np.concatenate(e) if e else np.zeros(0, np.int64)) for b, e in edges.items()}, n_ticks


def convert_sr(path: Path, out: Path, start: int = 0, end: int | None = None, excerpt: bool = False, **meta) -> tuple[Path, int, list[str], float] | None:
    """A logic-only .sr written as a WireSkein file without holding it: a chunk of the .sr at a time, each line's bits
    packed and appended to a file of its own, then copied into the capture (fileformat.OnDisk). Memory stays a chunk's,
    whatever the length (a long stream at a high rate). Ticks [start, end) only, tick 0 at start (an excerpt).
    (path written, ticks, channel names, rate), or None when the file is not one this writes (analog channels,
    channels of their own step, attachments: the whole read does those)."""
    import shutil
    import tempfile
    from .. import _fileformat as fileformat
    with zipfile.ZipFile(path) as z:
        cfg = configparser.ConfigParser()
        cfg.read_string(z.read("metadata").decode())
        dev = cfg["device 1"]
        if any(re.fullmatch(r"analog\d+", k) for k in dev):
            return None
        prefix = dev.get("capturefile", "logic-1")
        # only what this writes: anything else in the file (wireskein's own data, attachments, markers, parts it
        # does not know) is carried by the whole read
        if any(n not in ("version", "metadata") and n != prefix and not n.startswith(prefix + "-")
               for n in z.namelist()):
            return None
        rate = _parse_rate(dev["samplerate"])
        unitsize = int(dev.get("unitsize", "1"))
        names = {int(k[5:]): v for k, v in dev.items() if re.fullmatch(r"probe\d+", k)}
        total = int(dev.get("total probes", unitsize * 8))
        chunks = sorted((n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
                        key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0)
        dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
        bits = [b for b in (sorted(b - 1 for b in names) if names else range(min(total, unitsize * 8)))
                if b < unitsize * 8]
        if excerpt:                          # where the excerpt was in the source, from the chunks' sizes
            whole = sum(z.getinfo(n).file_size for n in chunks) // unitsize
            meta["excerpt"] = {"from_tick": start, "to_tick": whole if end is None else min(end, whole)}
        tmp = Path(tempfile.mkdtemp(prefix=".wireskein-", dir=Path(out).parent))
        try:
            files = {b: open(tmp / f"bit{b}", "wb") for b in bits}
            carry = {b: np.zeros(0, np.uint8) for b in bits}   # bits not yet a whole byte
            n_ticks = 0
            for first, data in _blocks(z, chunks, unitsize, dtype, start, end):
                n_ticks = first + len(data)
                for b in bits:
                    lv = np.concatenate((carry[b], ((data >> b) & 1).astype(np.uint8)))
                    whole = len(lv) // 8 * 8
                    files[b].write(np.packbits(lv[:whole], bitorder="little").tobytes())
                    carry[b] = lv[whole:]
            for b in bits:
                if len(carry[b]):
                    files[b].write(np.packbits(carry[b], bitorder="little").tobytes())
                files[b].close()
            chans = [fileformat.Channel(names.get(b + 1, f"bit{b}"), fileformat.OnDisk(tmp / f"bit{b}"), n_ticks)
                     for b in bits]
            written = fileformat.write(out, Fraction(round(rate)), chans, **meta)
        finally:
            for f in locals().get("files", {}).values():
                f.close()
            shutil.rmtree(tmp, ignore_errors=True)
    return written, n_ticks, [c.name for c in chans], rate
