"""The engine's pin classes, but a line's sparseness (the select candidate: SPI's CS) counted on its changes gated by
the fastest clock's period: changes closer than T / w are one (data.collapse, as grouping/gated measures pairs).

Why (2026-11-16, the calibration run 20261116T112652Z): of main's 49 wrong confirmed claims, 40 were SPI buses read
right (clock, data, mode, bytes) but without their CS line. taxonomy.pin_classes calls a line sparse from its raw
edge count against the busiest line's; spikes or crosstalk on CS add edges, CS is no longer sparse, and grouping
(taxonomy.groups: select only among sparse lines) never tries it. grouping/gated measures the pair relation through
the clock's period but not this count, so combo still left CS out 17 times (xtalk).
"""

from wsbench import algorithm
from wireskein._engine.survey import survey
from wireskein._engine.taxonomy import pin_classes

import numpy as np

# as lineclass/current.py (an algorithm file imports no other: the registry loads each once, by path)
TO_LABEL = {"clock": "clock", "data": "data", "sparse": "select", "pulse": "data"}


def collapse(edges, w: float) -> np.ndarray:
    """grouping/gated.py's: changes each within w ticks of the one before are one (an odd number a change at the
    last, an even number none)."""
    e = np.asarray(edges, np.int64)
    if len(e) < 2 or w < 2:
        return e
    first = np.concatenate(([True], np.diff(e) >= w))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


def _sparse(ratio: float) -> float:
    """taxonomy.pin_classes' sparseness from an edge ratio (the same curve)."""
    return float(np.clip(1 - np.log10(max(ratio, 1e-6) / 0.01) / 2, 0, 1)) if ratio < 0.2 else 0.0


def classes(sv, cap, w: float):
    own = pin_classes(sv, cap)
    periods = [ci.period for ci in sv.clocks.values() if ci.period]
    if not periods:
        return own
    gate = min(periods) / w
    counts = {n: len(collapse(cap.channel(n).edges, gate)) for n in own}
    top = max(counts.values(), default=1) or 1
    for n, pc in own.items():
        s = _sparse(counts[n] / top)
        if s <= pc.scores["sparse"]:
            continue
        pc.scores["sparse"] = s
        best = max(pc.scores.values())
        pc.candidates[:] = [k for k, v in pc.scores.items() if v >= 0.2 and v >= 0.5 * best] or [max(pc.scores, key=pc.scores.get)]
    return own


def engine(w):
    return {"pin_classes": lambda sv, cap: classes(sv, cap, w)}


@algorithm("lineclass", "gated_sparse", status="trial", engine=engine,
           about="pin_classes with a line's sparseness counted on its changes gated by the fastest clock (T / w)",
           variants={"w4": {"w": 4}})
def gated_sparse(cap, w):
    sv = survey(cap, asyncs=False)
    pcs = classes(sv, cap, w)
    out = {}
    for ch in cap.channels:
        pc = pcs.get(ch.name)
        if pc is None:
            out[ch.name] = {"label": "static"}
            continue
        best = max(pc.scores, key=pc.scores.get)
        out[ch.name] = {"label": TO_LABEL[best], "scores": pc.scores}
    return out
