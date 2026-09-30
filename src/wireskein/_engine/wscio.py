"""wireskein.wsc files <-> the edge-list model (numpy side)."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from .. import wsc
from .model import Capture, Channel, edges_from_dense


def load(path: str | Path) -> Capture:
    head, chans = wsc.read(path)
    out = []
    for c in chans:
        bits = np.unpackbits(np.frombuffer(c.bits, np.uint8), bitorder="little")[: c.n]
        init, edges = edges_from_dense(bits, c.step, c.phase)
        out.append(Channel(c.name, init, edges, c.step, c.phase))
    tick = wsc.tick_hz(head)
    return Capture(float(tick), int(head["ticks"]), out, meta={**head.get("meta", {}), "file": str(path), "tick_hz": tick})


def to_channel(ch: Channel, n_ticks: int) -> wsc.Channel:
    """The samples a channel really has (at phase + k * step) back into a wsc.Channel."""
    n = max(0, (n_ticks - ch.phase + ch.step - 1) // ch.step)
    ticks = ch.phase + np.arange(n, dtype=np.int64) * ch.step
    levels = ch.level_at(ticks).astype(np.uint8)
    return wsc.Channel(ch.name, np.packbits(levels, bitorder="little").tobytes(), n, ch.step, ch.phase)


def save(path: str | Path, cap: Capture, **meta) -> Path:
    tick = cap.meta.get("tick_hz", cap.rate)
    return wsc.write(path, tick, [to_channel(c, cap.n_samples) for c in cap.channels], **meta)
