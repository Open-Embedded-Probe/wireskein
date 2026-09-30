"""wireskein.fileformat files <-> the edge-list model (numpy side)."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from .. import fileformat
from .model import AnalogTrace, Capture, Channel, edges_from_dense


def load(path: str | Path) -> Capture:
    head, chans = fileformat.read(path)
    out, analog = [], []
    for c in chans:
        if isinstance(c, fileformat.AnalogChannel):
            dtype = np.float32 if c.encoding == "analog-f32" else {8: np.uint8, 16: "<u2", 32: "<u4"}[c.width]
            analog.append(AnalogTrace(c.name, np.frombuffer(c.data, dtype), c.rate_hz, c.t0_ticks, c.encoding, c.width,
                                      c.value_bits, c.zero, c.scale_nv, c.unit, dict(c.acquisition)))
            continue
        bits = np.unpackbits(np.frombuffer(c.bits, np.uint8), bitorder="little")[: c.n]
        init, edges = edges_from_dense(bits, c.step, c.phase)
        out.append(Channel(c.name, init, edges, c.step, c.phase, dict(c.acquisition)))
    tick = fileformat.tick_hz(head)
    return Capture(float(tick), int(head["ticks"]), out,
                   meta={**head.get("meta", {}), "file": str(path), "tick_hz": tick, "extras": fileformat.extras(path),
                         "capture_id": head.get("id"),
                         "skipped_channels": fileformat.skipped(head)}, analog=analog)


def to_channel(ch: Channel, n_ticks: int) -> fileformat.Channel:
    """The samples a channel really has (at phase + k * step) back into a fileformat.Channel."""
    n = max(0, (n_ticks - ch.phase + ch.step - 1) // ch.step)
    ticks = ch.phase + np.arange(n, dtype=np.int64) * ch.step
    levels = ch.level_at(ticks).astype(np.uint8)
    return fileformat.Channel(ch.name, np.packbits(levels, bitorder="little").tobytes(), n, ch.step, ch.phase,
                       dict(ch.acquisition))


def to_analog(a: AnalogTrace) -> fileformat.AnalogChannel:
    if a.encoding == "analog-f32":
        data = np.asarray(a.values, "<f4").tobytes()
    else:
        data = np.asarray(a.values, {8: np.uint8, 16: "<u2", 32: "<u4"}[a.width]).tobytes()
    return fileformat.AnalogChannel(a.name, data, len(a.values), a.rate_hz, a.t0_ticks, a.encoding, a.width, a.value_bits,
                             a.zero, a.scale_nv, a.unit, dict(a.acquisition))


def refuse_if_skipped(cap: Capture, dest) -> None:
    """Writing a capture read with skipped channels would drop them (wireskein-format §3.2)."""
    sk = cap.meta.get("skipped_channels")
    if sk:
        names = ", ".join(f"{c['name']} ({c['encoding']!r})" for c in sk)
        raise ValueError(f"not writing {dest}: it would drop the channels this version does not read: {names}")


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """meta: the capture file's metadata; attachments and notes carried in
    cap.meta["extras"] (as read from a .wireskein or .sr) are copied as they are."""
    import zipfile
    refuse_if_skipped(cap, path)
    tick = cap.meta.get("tick_hz", cap.rate)
    meta.pop("capture_id", None)
    path = fileformat.write(path, tick, [to_channel(c, cap.n_samples) for c in cap.channels] + [to_analog(a) for a in cap.analog],
                     capture_id=cap.meta.get("capture_id"),
                     **meta)
    if cap.meta.get("extras"):
        with zipfile.ZipFile(path, "a", zipfile.ZIP_DEFLATED) as z:
            for name, data in cap.meta["extras"].items():
                z.writestr(name, data)
    return path
