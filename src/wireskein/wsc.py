"""WireSkein capture files (.wsc): each channel at its own sample rate.

Standard library only, like wireskein.runlog, so anything holding samples can
save them without numpy. A .wsc is a zip:

    capture.json    format, tick clock, channels, capture metadata
    ch/0.bits       channel 0: one bit per sample, least significant bit first
    ch/1.bits       ...
    attach/<name>   free-form files: acquisition settings, analysis results,
                    anything (text, JSON, bytes); attach() adds or replaces one
    notes/<n>.json  an append-only log: note() adds one entry, with its time

Time is counted in ticks of one clock (tick_hz, a fraction). Channel k has a
sample every `step` ticks, the first at tick `phase`: a probe that decimates
some channels (keeps every 32nd sample to fit its link) stores those with
step=32 and only the samples it really took. Nothing is repeated to fill the
gaps, so a viewer can show exactly the samples that exist.

    from wireskein import wsc
    wsc.write("c.wsc", 100_000_000,
              [wsc.Channel("PA5", wsc.pack(samples_pa5), n),                   # samples: 0/1 per byte
               wsc.Channel("PB0", wsc.pack(samples_pb0), n // 32, step=32)],
              start_us=seg_start_us)
    wsc.write("c.wsc", 20_000_000, wsc.from_interleaved(data, ["PA5", "PA7"]))  # 1 byte per sample, bit k = channel k

    wsc.attach("c.wsc", "probe.json", {"fw": "1.2", "plan": {...}})   # later, to an existing file
    wsc.note("c.wsc", "PA5 looked noisy; re-captured with a shorter wire")
    wsc.note("c.wsc", {"kind": "analysis", "i2c": [...]})
"""

from __future__ import annotations

import datetime
import json
import os
import zipfile
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path

FORMAT = "wireskein-capture/0"
SUFFIX = ".wsc"
ENCODINGS = {"bits"}     # what this version reads; others (analog, ...) are refused, see docs/wsc-format.ja.md


@dataclass
class Channel:
    name: str
    bits: bytes          # n samples, one bit each, sample i at bit i % 8 of byte i // 8
    n: int               # number of samples
    step: int = 1        # ticks per sample
    phase: int = 0       # tick of the first sample

    def __post_init__(self):
        if self.step < 1 or not 0 <= self.phase:
            raise ValueError(f"{self.name}: step must be >= 1 and phase >= 0")
        if len(self.bits) != (self.n + 7) // 8:
            raise ValueError(f"{self.name}: {len(self.bits)} bytes for {self.n} samples, want {(self.n + 7) // 8}")

    @property
    def end(self) -> int:
        """The tick just after the last sample's step."""
        return self.phase + self.n * self.step


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

def write(path: str | Path, tick_hz: int | float | Fraction | tuple[int, int], channels: list[Channel],
          attachments: dict[str, str | bytes | dict | list] | None = None, **meta) -> Path:
    """Write a .wsc. tick_hz is the tick clock (a Fraction keeps an exact rate
    such as 160 MHz / 3). meta: small JSON-able facts about the capture, e.g.
    start_us (probe clock of tick 0), time_base_slipped, probe, trigger.
    attachments: free-form files stored as attach/<name> (see attach())."""
    path = Path(path)
    tick = Fraction(*tick_hz) if isinstance(tick_hz, tuple) else Fraction(tick_hz).limit_denominator(10**9)
    names = [c.name for c in channels]
    if len(set(names)) != len(names):
        raise ValueError(f"channel names repeat: {names}")
    head = {"format": FORMAT, "tick_hz": [tick.numerator, tick.denominator],
            "ticks": max((c.end for c in channels), default=0),
            "channels": [{"name": c.name, "file": f"ch/{k}.bits", "encoding": "bits", "n": c.n, "step": c.step,
                          "phase": c.phase} for k, c in enumerate(channels)],
            "meta": meta}
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr("capture.json", json.dumps(head, indent=1))
        for k, c in enumerate(channels):
            z.writestr(f"ch/{k}.bits", c.bits)
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
    """Store a free-form file in an existing .wsc as attach/<name>: str as
    UTF-8 text, dict / list as JSON, bytes as they are. The channel data is
    not rewritten unless an existing attachment is replaced (replace=True)."""
    entry = _attach_name(name)
    with zipfile.ZipFile(path) as z:
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
    """attach/ and notes/ entries as they are stored (to carry them into another file)."""
    with zipfile.ZipFile(path) as z:
        return {x: z.read(x) for x in z.namelist() if x.startswith((ATTACH, NOTES))}


def _rewrite(path: str | Path, drop: str) -> None:
    path = Path(path)
    tmp = path.with_name(path.name + ".tmp")
    with zipfile.ZipFile(path) as src, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as dst:
        for info in src.infolist():
            if info.filename != drop:
                dst.writestr(info, src.read(info))
    os.replace(tmp, path)


def read_header(path: str | Path) -> dict:
    with zipfile.ZipFile(path) as z:
        head = json.loads(z.read("capture.json"))
    if head.get("format") != FORMAT:
        raise ValueError(f"{path}: format {head.get('format')!r}, expected {FORMAT!r}")
    unknown = [f"{c.get('name')} ({c.get('encoding')!r})" for c in head.get("channels", [])
               if c.get("encoding") not in ENCODINGS]
    if unknown:
        raise ValueError(f"{path}: channel encodings this wireskein does not read: {', '.join(unknown)} "
                         f"(it reads {', '.join(sorted(ENCODINGS))}; a newer wireskein may)")
    return head


def read(path: str | Path) -> tuple[dict, list[Channel]]:
    """(header, channels). header["tick_hz"] is [numerator, denominator]."""
    head = read_header(path)
    with zipfile.ZipFile(path) as z:
        chans = [Channel(c["name"], z.read(c["file"]), c["n"], c["step"], c["phase"]) for c in head["channels"]]
    return head, chans


def tick_hz(head: dict) -> Fraction:
    return Fraction(*head["tick_hz"])
