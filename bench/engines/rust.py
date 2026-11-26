"""rust: WireSkein's core in Rust (crates/, bench/notes/2026-11-24-rust-plan.ja.md), built up beside the Python
engine and the package's default since §51, judged here as the other engines are, called directly (the bench keeps
wireskein's own calls Python's: bench/wsbench/__init__.py). Needs the Rust core built (wireskein._core: uv sync, or
the wheel the bench's workflow builds).
"""
from __future__ import annotations

import json

import numpy as np


def engine(cap, hints=None):
    from wireskein import _core
    chans = [(c.name, int(c.initial), np.ascontiguousarray(c.edges, dtype=np.int64)) for c in cap.channels]
    return json.loads(_core.analyze(float(cap.rate), int(cap.n_samples), chans, json.dumps(hints)))
