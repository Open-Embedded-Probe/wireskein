"""Shapes that explain a line without naming its protocol (an open world for engines of few protocols): a line whose
runs of each level take one or two widths (a pulse code: LEDs, 1-Wire, IR) is explained as such, and that explanation
competes with the protocols' readings in the best set like any claim (ahead of a reading as sure: a pulse code is
an explanation no reader here can give); it takes the line and is not shown. A UART's
runs take many widths (one to nine bits), so it is not one.

engine "open": the views engine with these shapes among the candidates."""
from __future__ import annotations

import numpy as np

from . import claims_of, views_candidates

TOL = 0.2          # runs within 20% of a width are that width
GAP = 8            # a run of more than 8 times the shortest is a gap between bursts, not a symbol


def widths(runs):
    """Clusters of the run widths: the number of widths that hold 90% of the runs."""
    if len(runs) == 0:
        return 0, 0.0
    r = np.sort(np.asarray(runs, float))
    cl, start = [], 0
    for i in range(1, len(r) + 1):
        if i == len(r) or r[i] > r[start] * (1 + TOL) + 1:
            cl.append(i - start)
            start = i
    cl.sort(reverse=True)
    k, held = 0, 0
    while held < 0.9 * len(r):
        held += cl[k]
        k += 1
    return k, held / len(r)


def pulse_code(ch):
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 24:
        return None
    runs = np.diff(e)
    lv = (int(ch.initial) + 1 + np.arange(len(runs))) & 1           # the level of each run between changes
    short = max(1, int(np.percentile(runs, 5)))
    sym = runs <= GAP * short
    ks = []
    for v in (0, 1):
        k, _ = widths(runs[sym & (lv == v)])
        ks.append(k)
    if not all(1 <= k <= 2 for k in ks) or max(ks) < 2:
        return None
    return 1.0


def candidates(cap):
    out = []
    for ch in cap.channels:
        s = pulse_code(ch)
        if s:
            out.append(("other", {"data": ch.name}, s, {"shape": "pulse_code"}, {}, {}))
    return out


def engine(cap):
    return [c for c in claims_of(candidates(cap) + views_candidates(cap)) if c["protocol"] != "other"]
