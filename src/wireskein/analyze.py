"""Decoding a capture (beta: the result format may change).

    cap = load("capture.sr")                 # or a fixture directory
    res = analyze(cap, {"protocols": ["i2c"]})
    doc = export(res, cap)                   # JSON-ready dict, as `wireskein analyze` prints
"""

from __future__ import annotations

from pathlib import Path

from ._engine import fixture, staged
from ._engine.export import dumps, export
from ._engine.model import Capture, Channel
from ._engine.srio import read_sr

__all__ = ["Capture", "Channel", "load", "analyze", "export", "dumps"]


def load(path: str | Path) -> Capture:
    """A sigrok .sr file or a fixture directory (capture.json + edges)."""
    path = Path(path)
    return fixture.load_capture(path) if path.is_dir() else read_sr(path)


def analyze(cap: Capture, hints: dict | None = None, declarative: bool = False):
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried."""
    staged.use_declarative(declarative)
    return staged.analyze(cap, hints)
