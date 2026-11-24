"""rust: WireSkein's core in Rust (crates/, bench/notes/2026-11-24-rust-plan.ja.md), built up beside the Python
engine a part at a time and judged here as the other engines are. Needs the module wireskein_core:
    uv run --with ./crates/wireskein-py --with-requirements bench/requirements.txt python bench/run.py ...
"""
from __future__ import annotations

import json

import numpy as np


def engine(cap, hints=None):
    import wireskein_core
    chans = [(c.name, int(c.initial), np.ascontiguousarray(c.edges, dtype=np.int64)) for c in cap.channels]
    return json.loads(wireskein_core.analyze(float(cap.rate), int(cap.n_samples), chans, json.dumps(hints)))
