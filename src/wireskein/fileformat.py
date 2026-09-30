"""WireSkein files (.wireskein, docs/wireskein-format.ja.md): a capture with
each channel at its own sample rate, and what goes with it.

Standard library only, like wireskein.runlog, so anything holding samples can
save them without numpy. A .wireskein is a zip:

    wireskein.json  {"format": "wireskein/0"}: what the file is (first entry, stored)
    capture.json    tick clock, channels, capture metadata
    ch/0.bits       channel 0: one bit per sample, least significant bit first
    ch/1.bits       ...
    attach/<name>   free-form files: acquisition settings, analysis results,
                    anything (text, JSON, bytes); attach() adds or replaces one
    notes/<n>.json  an append-only log: note() adds one entry, with its time

Readers go by the content, not the name (sniff()). Entries this version does
not know are left alone, and carried over when the file is rewritten.

Time is counted in ticks of one clock (tick_hz, a fraction). Channel k has a
sample every `step` ticks, the first at tick `phase`: a probe that decimates
some channels (keeps every 32nd sample to fit its link) stores those with
step=32 and only the samples it really took. Nothing is repeated to fill the
gaps, so a viewer can show exactly the samples that exist.

    from wireskein import fileformat as wf
    wf.write("c.wireskein", 100_000_000,
             [wf.Channel("PA5", wf.pack(samples_pa5), n),                   # samples: 0/1 per byte
              wf.Channel("PB0", wf.pack(samples_pb0), n // 32, step=32)],
             start_us=seg_start_us)
    wf.write("c.wireskein", 20_000_000, wf.from_interleaved(data, ["PA5", "PA7"]))  # 1 byte per sample, bit k = channel k

    wf.attach("c.wireskein", "probe.json", {"fw": "1.2", "plan": {...}})   # later, to an existing file
    wf.note("c.wireskein", "PA5 looked noisy; re-captured with a shorter wire")
    wf.note("c.wireskein", {"kind": "analysis", "i2c": [...]})
"""

from __future__ import annotations

import array
import datetime
import json
import math
import os
import sys
import zipfile
from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

FORMAT = "wireskein/0"
SUFFIX = ".wireskein"
IDENT = "wireskein.json"          # what the file is; the first entry, stored (wireskein-format §2.1)
CAPTURE = "capture.json"
ENCODINGS = {"bits", "analog", "analog-f32"}    # what this version reads; others are skipped (docs/wireskein-format.ja.md §3.2)


@dataclass
class Channel:
    name: str
    bits: bytes          # n samples, one bit each, sample i at bit i % 8 of byte i // 8
    n: int               # number of samples
    step: int = 1        # ticks per sample
    phase: int = 0       # tick of the first sample
    acquisition: dict = field(default_factory=dict)   # how it was taken (pin, ...), wireskein-format §3.1.1

    encoding = "bits"

    def __post_init__(self):
        if self.step < 1 or not 0 <= self.phase:
            raise ValueError(f"{self.name}: step must be >= 1 and phase >= 0")
        if len(self.bits) != (self.n + 7) // 8:
            raise ValueError(f"{self.name}: {len(self.bits)} bytes for {self.n} samples, want {(self.n + 7) // 8}")

    @property
    def end(self) -> int:
        """The tick just after the last sample's step."""
        return self.phase + self.n * self.step


_TYPECODE = {8: "B", 16: "H", 32: "I"}


@dataclass
class AnalogChannel:
    """An analog channel (wireskein-format §4.2, §4.3). encoding "analog": raw
    unsigned values of `width` bits (little endian), volts = (raw - zero) *
    scale_nv * 1e-9 when zero / scale_nv are known; "analog-f32": float32
    volts. Sample k is at tick t0_ticks + k * tick_hz / rate_hz."""
    name: str
    data: bytes
    n: int
    rate_hz: Fraction
    t0_ticks: Fraction = Fraction(0)
    encoding: str = "analog"
    width: int = 16
    value_bits: int | None = None
    zero: float | None = None
    scale_nv: float | None = None
    unit: str = "V"
    acquisition: dict = field(default_factory=dict)

    def __post_init__(self):
        self.rate_hz, self.t0_ticks = _fraction(self.rate_hz), _fraction(self.t0_ticks)
        if self.encoding not in ("analog", "analog-f32"):
            raise ValueError(f"{self.name}: encoding {self.encoding!r} is not analog")
        if self.rate_hz <= 0:
            raise ValueError(f"{self.name}: rate_hz must be > 0")
        size = self.n * (4 if self.encoding == "analog-f32" else self.width // 8)
        if self.encoding == "analog" and self.width not in _TYPECODE:
            raise ValueError(f"{self.name}: width {self.width}: must be 8, 16 or 32")
        if len(self.data) != size:
            raise ValueError(f"{self.name}: {len(self.data)} bytes for {self.n} samples, want {size}")

    def end(self, tick: Fraction) -> int:
        """The tick just after the last sample (rounded up)."""
        return math.ceil(self.t0_ticks + self.n * tick / self.rate_hz)

    def values(self) -> list:
        """Raw integers ("analog") or volts ("analog-f32")."""
        a = array.array("f" if self.encoding == "analog-f32" else _TYPECODE[self.width])
        a.frombytes(self.data)
        if sys.byteorder == "big":
            a.byteswap()
        return a.tolist()

    def volts(self) -> list[float] | None:
        """Volts by the stored linear conversion; None when it is not known."""
        if self.encoding == "analog-f32":
            return self.values()
        if self.zero is None or self.scale_nv is None:
            return None
        return [(v - self.zero) * self.scale_nv * 1e-9 for v in self.values()]


def _fraction(x) -> Fraction:
    if isinstance(x, (list, tuple)):
        return Fraction(*x)
    return Fraction(x).limit_denominator(10**12) if isinstance(x, float) else Fraction(x)


def _packed(values, code: str) -> bytes:
    a = array.array(code, values)
    if sys.byteorder == "big":
        a.byteswap()
    return a.tobytes()


def analog_raw(name: str, values, rate_hz, width: int = 16, t0_ticks=0, value_bits: int | None = None,
               zero: float | None = None, scale_nv: float | None = None, **acquisition) -> AnalogChannel:
    """Raw ADC values (unsigned integers) -> an "analog" channel. acquisition:
    pin, attenuation_db, reference={"source": "vdd", "mv": 3300}, vrefint_raw, ..."""
    data = _packed(values, _TYPECODE[width])
    return AnalogChannel(name, data, len(data) // (width // 8), rate_hz, t0_ticks, "analog", width, value_bits,
                         zero, scale_nv, acquisition=acquisition)


def analog_volts(name: str, values, rate_hz, t0_ticks=0, unit: str = "V", **acquisition) -> AnalogChannel:
    """Volts (floats) -> an "analog-f32" channel."""
    data = _packed(values, "f")
    return AnalogChannel(name, data, len(data) // 4, rate_hz, t0_ticks, "analog-f32", unit=unit,
                         acquisition=acquisition)


# ---------------- packing (C-speed paths of the standard library) ----------------

_TO_ASCII01 = bytes(0x31 if b & 1 else 0x30 for b in range(256))


def pack(samples: bytes | bytearray | memoryview) -> bytes:
    """One sample per byte (its bit 0 is the level) -> one bit per sample."""
    n = len(samples)
    if n == 0:
        return b""
    text = bytes(samples).translate(_TO_ASCII01)[::-1]         # sample 0 becomes the least significant digit
    return int(text, 2).to_bytes((n + 7) // 8, "little")


def unpack(ch: Channel) -> bytes:
    """One bit per sample -> one sample per byte (0 or 1)."""
    if ch.n == 0:
        return b""
    text = format(int.from_bytes(ch.bits, "little"), f"0{len(ch.bits) * 8}b")[::-1][: ch.n]
    return text.encode().translate(bytes(b - 0x30 if 0x30 <= b <= 0x31 else 0 for b in range(256)))


def _bit(data: bytes, b: int) -> bytes:
    return data.translate(bytes((x >> b) & 1 for x in range(256)))


def from_interleaved(data: bytes | bytearray | memoryview, names: list[str], width: int = 8,
                     positions: list[int] | None = None, n: int | None = None, step: int = 1, phase: int = 0) -> list[Channel]:
    """Channels from a probe's sample stream (oep-if-capture §1.1): each sample
    is `width` bits (1, 2, 4, 8, 16 or 32), channel k is bit positions[k] of it
    (default k), samples below 8 bits share a byte with the earliest sample in
    the low bits, wider ones are little-endian. n: number of samples (default:
    all that fit). All channels get the same step and phase."""
    data = bytes(data)
    if width not in (1, 2, 4, 8, 16, 32):
        raise ValueError(f"width {width}: must be 1, 2, 4, 8, 16 or 32")
    positions = list(range(len(names))) if positions is None else list(positions)
    if len(positions) != len(names) or any(not 0 <= q < width for q in positions):
        raise ValueError(f"positions {positions} do not fit {len(names)} channels of a {width}-bit sample")
    total = len(data) * 8 // width
    n = total if n is None else n
    if n > total:
        raise ValueError(f"{n} samples of {width} bits need {(n * width + 7) // 8} bytes, got {len(data)}")
    out = []
    for name, q in zip(names, positions):
        if width >= 8:
            size = width // 8
            col = _bit(data[q // 8::size], q % 8)
        else:
            per = 8 // width
            buf = bytearray(len(data) * per)
            for j in range(per):
                buf[j::per] = _bit(data, j * width + q)
            col = bytes(buf)
        out.append(Channel(name, pack(col[:n]), n, step, phase))
    return out


# ---------------- files ----------------

def write(path: str | Path, tick_hz: int | float | Fraction | tuple[int, int], channels: list[Channel | AnalogChannel],
          attachments: dict[str, str | bytes | dict | list] | None = None, **meta) -> Path:
    """Write a .wireskein file (any name; SUFFIX is the usual one). tick_hz is the tick clock (a Fraction keeps an exact rate
    such as 160 MHz / 3). meta: small JSON-able facts about the capture, e.g.
    start_us (probe clock of tick 0), time_base_slipped, probe, trigger.
    attachments: free-form files stored as attach/<name> (see attach())."""
    path = Path(path)
    tick = Fraction(*tick_hz) if isinstance(tick_hz, tuple) else Fraction(tick_hz).limit_denominator(10**9)
    names = [c.name for c in channels]
    if len(set(names)) != len(names):
        raise ValueError(f"channel names repeat: {names}")
    entries, files = [], []
    for k, c in enumerate(channels):
        if isinstance(c, AnalogChannel):
            e = {"name": c.name, "file": f"ch/{k}.{'f32' if c.encoding == 'analog-f32' else 'raw'}",
                 "encoding": c.encoding, "n": c.n, "rate_hz": [c.rate_hz.numerator, c.rate_hz.denominator],
                 "t0_ticks": [c.t0_ticks.numerator, c.t0_ticks.denominator]}
            if c.encoding == "analog":
                e["width"] = c.width
                e.update({k2: v for k2, v in (("value_bits", c.value_bits), ("zero", c.zero), ("scale_nv", c.scale_nv))
                          if v is not None})
            elif c.unit != "V":
                e["unit"] = c.unit
            files.append((e["file"], c.data))
        else:
            e = {"name": c.name, "file": f"ch/{k}.bits", "encoding": "bits", "n": c.n, "step": c.step, "phase": c.phase}
            files.append((e["file"], c.bits))
        if c.acquisition:
            e["acquisition"] = c.acquisition
        entries.append(e)
    ends = [c.end(tick) if isinstance(c, AnalogChannel) else c.end for c in channels]
    head = {"tick_hz": [tick.numerator, tick.denominator], "ticks": max(ends, default=0),
            "channels": entries, "meta": meta}
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr(zipfile.ZipInfo(IDENT, (1980, 1, 1, 0, 0, 0)), json.dumps({"format": FORMAT}), zipfile.ZIP_STORED)
        z.writestr(CAPTURE, json.dumps(head, indent=1, default=str))
        for name, data in files:
            z.writestr(name, data)
        for name, data in (attachments or {}).items():
            z.writestr(_attach_name(name), _encode(data))
    return path


# ---------------- attachments and notes ----------------

ATTACH, NOTES = "attach/", "notes/"


def _attach_name(name: str) -> str:
    if not name or name.startswith("/") or ".." in name.split("/"):
        raise ValueError(f"bad attachment name {name!r}")
    return ATTACH + name


def _encode(data) -> bytes:
    if isinstance(data, (bytes, bytearray, memoryview)):
        return bytes(data)
    if isinstance(data, str):
        return data.encode()
    return json.dumps(data, indent=1, ensure_ascii=False, default=str).encode()


def attach(path: str | Path, name: str, data: str | bytes | dict | list, replace: bool = False) -> None:
    """Store a free-form file in an existing .wireskein as attach/<name>: str as
    UTF-8 text, dict / list as JSON, bytes as they are. The channel data is
    not rewritten unless an existing attachment is replaced (replace=True)."""
    entry = _attach_name(name)
    with zipfile.ZipFile(path) as z:
        _check(z, path)
        exists = entry in z.namelist()
    if exists and not replace:
        raise FileExistsError(f"{path}: {entry} exists (replace=True to overwrite)")
    if exists:
        _rewrite(path, drop=entry)
    with zipfile.ZipFile(path, "a", zipfile.ZIP_DEFLATED) as z:
        z.writestr(entry, _encode(data))


def note(path: str | Path, content: str | dict | list, **fields) -> int:
    """Append one entry to the capture's log (never rewrites the others):
    {"time": ISO 8601, "content": ..., **fields}. Returns its number."""
    with zipfile.ZipFile(path) as z:
        _check(z, path)
        n = sum(1 for x in z.namelist() if x.startswith(NOTES)) + 1
    entry = {"time": datetime.datetime.now().astimezone().isoformat(timespec="seconds"), "content": content, **fields}
    with zipfile.ZipFile(path, "a", zipfile.ZIP_DEFLATED) as z:
        z.writestr(f"{NOTES}{n:04d}.json", json.dumps(entry, ensure_ascii=False, default=str))
    return n


def attachments(path: str | Path) -> dict[str, bytes]:
    with zipfile.ZipFile(path) as z:
        return {x[len(ATTACH):]: z.read(x) for x in z.namelist() if x.startswith(ATTACH)}


def notes(path: str | Path) -> list[dict]:
    with zipfile.ZipFile(path) as z:
        return [json.loads(z.read(x)) for x in sorted(x for x in z.namelist() if x.startswith(NOTES))]


def extras(path: str | Path) -> dict[str, bytes]:
    """Every entry but the capture itself (attach/, notes/ and parts this
    version does not know), as stored: to carry them into another file."""
    with zipfile.ZipFile(path) as z:
        head = json.loads(z.read(CAPTURE)) if CAPTURE in z.namelist() else {}
        own = {IDENT, CAPTURE} | {c.get("file") for c in head.get("channels", [])}
        return {x: z.read(x) for x in z.namelist() if x not in own and not x.endswith("/")}


def _rewrite(path: str | Path, drop: str) -> None:
    path = Path(path)
    tmp = path.with_name(path.name + ".tmp")
    with zipfile.ZipFile(path) as src, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as dst:
        for info in src.infolist():
            if info.filename != drop:
                dst.writestr(info, src.read(info))
    os.replace(tmp, path)


def sniff(path: str | Path) -> str | None:
    """What a file is, by its content (wireskein-format §2.3): "wireskein",
    "sr" (a sigrok session), "vcd" (a Value Change Dump), or None. A WireSkein file of a version this one
    does not know is "wireskein" too; read_header() says why it cannot read it."""
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
        if head[:4] != b"PK\x03\x04":                 # not a zip: a VCD (text) or nothing we read
            text = head.decode("utf-8", "replace").lstrip()
            import re
            return "vcd" if re.match(r"\$(date|version|timescale|comment|scope|var)\b", text) else None
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())
            if IDENT in names:          # (a .sr written by wireskein has one too, "wireskein-sr-extra/0")
                fmt = json.loads(z.read(IDENT)).get("format", "")
                if isinstance(fmt, str) and fmt.startswith("wireskein/"):
                    return "wireskein"
            return "sr" if {"version", "metadata"} <= names else None
    except (OSError, zipfile.BadZipFile, ValueError, AttributeError):
        return None


def _check(z: zipfile.ZipFile, path) -> None:
    """ValueError unless z is a WireSkein file of this version."""
    if IDENT not in z.namelist():
        raise ValueError(f"{path}: not a WireSkein file (no {IDENT})")
    fmt = json.loads(z.read(IDENT)).get("format")
    if fmt != FORMAT:
        raise ValueError(f"{path}: format {fmt!r}, this version reads {FORMAT!r} (a newer wireskein may read it)")


def read_header(path: str | Path) -> dict:
    """capture.json of a WireSkein file (ValueError: not one, a newer version, or no capture in it)."""
    with zipfile.ZipFile(path) as z:
        _check(z, path)
        if CAPTURE not in z.namelist():
            raise ValueError(f"{path}: holds no capture")
        return json.loads(z.read(CAPTURE))


def skipped(head: dict) -> list[dict]:
    """Channels whose encoding this version does not read: [{"name", "encoding"}]."""
    return [{"name": c.get("name"), "encoding": c.get("encoding")} for c in head.get("channels", [])
            if c.get("encoding") not in ENCODINGS]


def read(path: str | Path, analog: bool = True) -> tuple[dict, list[Channel | AnalogChannel]]:
    """(header, channels): Channel for logic, AnalogChannel for analog (left
    out with analog=False). header["tick_hz"] is [numerator, denominator].
    Channels of an encoding this version does not read are left out (never
    read as something else); skipped(header) names them."""
    head = read_header(path)
    chans = []
    with zipfile.ZipFile(path) as z:
        for c in head["channels"]:
            enc = c.get("encoding")
            if enc == "bits":
                chans.append(Channel(c["name"], z.read(c["file"]), c["n"], c["step"], c["phase"],
                                     c.get("acquisition", {})))
            elif enc in ("analog", "analog-f32") and analog:
                chans.append(AnalogChannel(c["name"], z.read(c["file"]), c["n"], Fraction(*c["rate_hz"]),
                                           Fraction(*c["t0_ticks"]), enc, c.get("width", 16), c.get("value_bits"),
                                           c.get("zero"), c.get("scale_nv"), c.get("unit", "V"),
                                           c.get("acquisition", {})))
    return head, chans


def tick_hz(head: dict) -> Fraction:
    return Fraction(*head["tick_hz"])
