"""Shapes that explain a line without naming its protocol (an open world for engines of few protocols): a line whose
runs of each level take one or two widths (a pulse code: LEDs, 1-Wire, IR) is explained as such, and that explanation
competes with the protocols' readings in the best set like any claim (ahead of a reading as sure: a pulse code is
an explanation no reader here can give); the lines it takes are left out, and it is not shown. A UART's
runs take many widths (one to nine bits), so it is not one.

engine "open": the views engine with these shapes among the candidates."""
from __future__ import annotations

import numpy as np

from . import claims_of, views_candidates

TOL = 0.2          # runs within 20% of a width are that width


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


def pulse_code(ch, need_two=False):
    """A line whose runs of each level take one or two widths (90% of them): a pulse code, a carrier's bursts, a
    clock. None, or the shape's score."""
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 24:
        return None
    runs = np.diff(e)
    lv = (int(ch.initial) + 1 + np.arange(len(runs))) & 1           # the level of each run between changes
    ks = [widths(runs[lv == v])[0] for v in (0, 1)]
    if not all(1 <= k <= 2 for k in ks) or (need_two and max(ks) < 2):
        return None
    return 1.0


def candidates(cap, need_two=False):
    return [("other", {"data": x}, 1.0, {"shape": "pulse_code"}, {}, {}) for x in sorted(shaped_lines(cap, need_two))]


def engine(cap, need_two=False):
    """Two passes: the best set with the shapes among the candidates says which lines a shape takes (a clock inside
    an SPI is not one: the bus explains more lines and goes first); the readings on the other lines then make the
    claims, their margins over readings only (a shape is no rival to a bus whose line it fits)."""
    cands = views_candidates(cap)
    taken = {x for c in claims_of(candidates(cap, need_two) + cands) if c["protocol"] == "other"
             for x in c["roles"].values()}
    return claims_of([c for c in cands if not set(c[1].values()) & taken])


def shaped_lines(cap, need_two=False) -> set:
    """The lines a shape fits on any of the views engine's views (as captured, pulses of a sample out, bounces
    merged), as the readers are tried on them: on the lines as captured only, a spike or a bounce adds widths and
    the shape is lost (light_open, noise added: 134 false claims against 20 on the views)."""
    from . import _views
    return {ch.name for _, v in _views(cap) for ch in v.channels if pulse_code(ch, need_two)}


def opened(claims: list[dict], cap) -> list[dict]:
    """The open world after any engine (no candidates needed): a claim on a line a shape fits is left out unless it
    explains more than the shape (its score times its lines over 1, the shape's): the two-pass engine's choice
    read off the claims."""
    shaped = shaped_lines(cap)
    return [c for c in claims if not (set(c["roles"].values()) & shaped
                                      and float(c["score"]) * len(set(c["roles"].values())) <= 1.0)]
