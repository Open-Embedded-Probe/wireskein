"""Decoding a capture (beta: the result format may change).

    cap = load("capture.wsc")                # .wsc, .sr or a fixture directory
    res = analyze(cap, {"protocols": ["i2c"]})
    doc = export(res, cap)                   # JSON-ready dict, as `wireskein analyze` prints
"""

from __future__ import annotations

from pathlib import Path

from ._engine import fixture, staged, wscio
from ._engine.export import dumps, export
from ._engine.model import Capture, Channel
from ._engine.srio import read_sr, write_sr

__all__ = ["Capture", "Channel", "load", "save", "analyze", "export", "dumps"]


def load(path: str | Path) -> Capture:
    """A .wsc (wireskein.wsc), a sigrok .sr, or a fixture directory (capture.json + edges)."""
    path = Path(path)
    if path.is_dir():
        return fixture.load_capture(path)
    if path.suffix == ".wsc":
        return wscio.load(path)
    return read_sr(path)


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """.wsc (each channel at its own rate) or .sr (one rate: slow channels are
    repeated, their real rate kept in wireskein.json inside the zip)."""
    path = Path(path)
    wscio.refuse_if_skipped(cap, path)
    if path.suffix == ".wsc":
        return wscio.save(path, cap, **meta)
    if path.suffix == ".sr":
        return write_sr(path, cap, **meta)
    raise ValueError(f"{path}: unknown format (use .wsc or .sr)")


def analyze(cap: Capture, hints: dict | None = None, declarative: bool = False):
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried."""
    staged.use_declarative(declarative)
    return staged.analyze(cap, hints)
