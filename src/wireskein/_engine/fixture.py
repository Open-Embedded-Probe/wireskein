"""Fixture directories: anonymized capture + separate ground truth.

    <dir>/capture.json   rate, n_samples, channels [{name, initial}]
    <dir>/edges.npz      one delta-encoded uint32 array per channel
    <dir>/truth.json     ground truth (only the evaluator reads this)

Channel names in capture.json are D0..Dn in a shuffled order, so analysis can
rely neither on names nor on the original channel order.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from .model import Capture, Channel


def save(path: Path, cap: Capture, truth: dict) -> None:
    path.mkdir(parents=True, exist_ok=True)
    meta = {
        "rate": cap.rate,
        "n_samples": cap.n_samples,
        "channels": [{"name": ch.name, "initial": ch.initial} for ch in cap.channels],
    }
    (path / "capture.json").write_text(json.dumps(meta, indent=1) + "\n")
    arrays = {}
    for ch in cap.channels:
        d = np.diff(ch.edges, prepend=0)
        if len(d) and d.max() >= 2**32:
            raise ValueError("edge gap too large for uint32")
        arrays[ch.name] = d.astype(np.uint32)
    np.savez_compressed(path / "edges.npz", **arrays)
    (path / "truth.json").write_text(json.dumps(truth, indent=1, ensure_ascii=False) + "\n")


def load_capture(path: Path) -> Capture:
    meta = json.loads((path / "capture.json").read_text())
    with np.load(path / "edges.npz") as z:
        channels = [
            Channel(c["name"], int(c["initial"]), np.cumsum(z[c["name"]].astype(np.int64)))
            for c in meta["channels"]
        ]
    return Capture(float(meta["rate"]), int(meta["n_samples"]), channels, meta={"fixture": str(path)})


def load_truth(path: Path) -> dict:
    return json.loads((path / "truth.json").read_text())


def anonymize(cap: Capture, seed: int) -> tuple[Capture, dict[str, str]]:
    """Rename channels to D0..Dn in a seeded random order. Returns the new
    capture and a map anonymous name -> original name."""
    rng = np.random.default_rng(seed)
    order = rng.permutation(len(cap.channels))
    channels, mapping = [], {}
    for new_idx, old_idx in enumerate(order):
        old = cap.channels[old_idx]
        name = f"D{new_idx}"
        channels.append(Channel(name, old.initial, old.edges, old.step, old.phase))
        mapping[name] = old.name
    return Capture(cap.rate, cap.n_samples, channels), mapping
