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

__all__ = ["Capture", "Channel", "load", "save", "analyze", "export", "dumps"]


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
    raise ValueError(f"{path}: neither a WireSkein file nor a sigrok session (.sr)")


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """By the name: .sr (one rate: slow channels are repeated, their real rate
    kept in wireskein.json inside the zip), anything else a WireSkein file
    (each channel at its own rate; .wireskein is the usual name)."""
    path = Path(path)
    fileio.refuse_if_skipped(cap, path)
    if path.suffix == ".sr":
        return write_sr(path, cap, **meta)
    return fileio.save(path, cap, **meta)


def analyze(cap: Capture, hints: dict | None = None, declarative: bool = False):
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried."""
    staged.use_declarative(declarative)
    return staged.analyze(cap, hints)
