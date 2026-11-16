"""Pairs measured through the clock's own rate: once a clock's period T is known, a line's changes closer than T / w
are one change (a bouncing edge crossing the threshold several times), so noise near the edges no longer scatters
the phase a data line keeps to the clock. Everything else as taxonomy.groups does: only the pair measure differs.

The user's hypothesis (2026-11-16): with the fundamental frequency known, noise is easy to gate."""

from dataclasses import replace

import numpy as np

from wsbench import algorithm
from wireskein._engine.features import channel_features
from wireskein._engine.model import Capture
from wireskein._engine.survey import clock_info, pair_relation, survey
from wireskein._engine.taxonomy import groups, pin_classes


def collapse(edges: np.ndarray, w: float) -> np.ndarray:
    """Changes each within w ticks of the one before are one: an odd number of them a change at the last, an even
    number none (a pulse)."""
    e = np.asarray(edges, np.int64)
    if len(e) < 2 or w < 2:
        return e
    first = np.concatenate(([True], np.diff(e) >= w))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


@algorithm("grouping", "gated", status="trial", about="pairs measured with changes closer than T/w merged (T: the clock's period)",
           variants={"w8": {"w": 8}, "w4": {"w": 4}})
def gated(cap, w):
    sv = survey(cap, asyncs=False)
    pins = pin_classes(sv, cap)
    for c, ci in list(sv.clocks.items()):
        if not ci.period:
            continue
        k = ci.period / w
        view = Capture(cap.rate, cap.n_samples, [replace(x, edges=collapse(x.edges, k)) for x in cap.channels])
        vch = view.channel(c)
        vci = clock_info(view, channel_features(view, vch)) if len(vch.edges) >= 8 else None
        if vci is None:
            continue
        for o in sv.active:
            if o != c:
                sv.pairs[(c, o)] = pair_relation(view, vci, o)
    return [{"clock": g.clock, "data": list(g.data), "select": g.select, "score": g.score}
            for g in groups(sv, pins) if g.kind == "sync"]
