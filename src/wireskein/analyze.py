"""Decoding a capture (beta: the result format may change).

    cap = load("capture.wireskein")          # a WireSkein file, a .sr or a fixture directory
    res = analyze(cap, {"protocols": ["i2c"]})
    doc = export(res, cap)                   # JSON-ready dict, as `wireskein analyze` prints
"""

from __future__ import annotations

from pathlib import Path

from . import fileformat
from ._engine import fileio, fixture, staged
from ._engine.export import dumps, export
from ._engine.model import Capture, Channel
from ._engine.srio import read_sr, write_sr

__all__ = ["Capture", "Channel", "load", "save", "as_logic", "analyze", "export", "dumps"]


def load(path: str | Path) -> Capture:
    """A WireSkein file (wireskein.fileformat), a sigrok .sr, or a fixture
    directory (capture.json + edges). Files are told apart by their content."""
    path = Path(path)
    if path.is_dir():
        return fixture.load_capture(path)
    kind = fileformat.sniff(path)
    if kind == "wireskein":
        return fileio.load(path)
    if kind == "sr":
        return read_sr(path)
    if kind == "vcd":
        from ._engine.vcdio import read_vcd
        return read_vcd(path)
    raise ValueError(f"{path}: not a WireSkein file, a sigrok session (.sr) or a VCD")


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """By the name: .sr (one rate: slow channels are repeated, their real rate
    kept in wireskein.json inside the zip), .vcd (the changes; wireskein's own
    data in a $comment), anything else a WireSkein file
    (each channel at its own rate; .wireskein is the usual name)."""
    path = Path(path)
    fileio.refuse_if_skipped(cap, path)
    if path.suffix in (".sr", ".vcd"):
        fileio.refuse_if_no_intervals(cap, path)
    if path.suffix == ".sr":
        return write_sr(path, cap, **meta)
    if path.suffix == ".vcd":
        from ._engine.vcdio import write_vcd
        return write_vcd(path, cap, **meta)
    return fileio.save(path, cap, **meta)


def as_logic(cap: Capture, thresholds: dict) -> Capture:
    """The capture with these analog channels also read as logic (name -> volts,
    or (low, high) for hysteresis), so the decoders see them: a UART or I2C line
    captured by an ADC. The analog channels stay as they are."""
    from ._engine import analog
    names = {c.name for c in cap.channels}
    extra = []
    for name, th in thresholds.items():
        trace = next((a for a in cap.analog if a.name == name), None)
        if trace is None:
            raise ValueError(f"{name}: not an analog channel in this capture")
        if name in names:
            raise ValueError(f"{name}: already a logic channel")
        extra.append(analog.to_logic(trace, cap.meta.get("tick_hz", cap.rate), th))
    return Capture(cap.rate, cap.n_samples, cap.channels + extra, cap.meta, cap.analog, cap.intervals)


def analyze(cap: Capture, hints: dict | None = None):
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried."""
    return staged.analyze(cap, hints)
