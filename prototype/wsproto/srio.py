"""Read sigrok session (.sr) logic captures into the edge-list model."""

from __future__ import annotations

import configparser
import re
import zipfile
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
        chunks = sorted(
            (n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
            key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0,
        )
        raw = b"".join(z.read(n) for n in chunks)
    dtype = {1: np.uint8, 2: np.uint16, 4: np.uint32}[unitsize]
    data = np.frombuffer(raw, dtype=dtype)
    # Every stored bit becomes a channel, named or not: unnamed bits are real
    # (usually static) inputs and are useful negatives.
    channels = []
    for bit in range(unitsize * 8):
        bits = ((data >> bit) & 1).astype(np.uint8)
        initial, edges = edges_from_dense(bits)
        channels.append(Channel(names.get(bit + 1, f"bit{bit}"), initial, edges))
    return Capture(rate, len(data), channels, meta={"source": str(path), "unitsize": unitsize})
