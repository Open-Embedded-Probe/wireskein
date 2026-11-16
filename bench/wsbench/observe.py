"""What a capture shows of itself before anything is decided: what a switch between algorithms could go by at run time
(select.py's dispatch:obs_*). Nothing here sees the truth.

    short_share   share of the whole runs (all active lines) of 2 samples or less
    short_rel     share of the whole runs shorter than a fifth of their line's unit (2 samples on a line whose unit the
                  comb cannot tell): how much short noise there is, the same in time whatever the rate
    samples_bit   the fewest samples per unit among the lines whose runs are whole units (the comb over each
                  line's runs, its score 0.5 or more): how finely the fastest line is taken
"""

from __future__ import annotations

import numpy as np

from .kit import comb_unit, runs


def observe(cap) -> dict:
    short = rel = total = 0
    units = []
    for ch in cap.channels:
        if len(ch.edges) < 16:
            continue
        r = runs(cap, ch)
        short += int(np.sum(r <= 2 * ch.step))
        total += len(r)
        u, score = comb_unit(r, 12)
        if u and score >= 0.5:      # a line whose runs are whole units (a random line's "unit" is no rate)
            units.append(u / ch.step)
            rel += int(np.sum(r < 0.2 * u))
        else:
            rel += int(np.sum(r <= 2 * ch.step))
    return {"short_share": short / total if total else 0.0, "short_rel": rel / total if total else 0.0,
            "samples_bit": min(units) if units else None}


def bucket_noise(x: float | None) -> str:
    if x is None:
        return "?"
    return "<1%" if x < 0.01 else "1-5%" if x < 0.05 else "5-20%" if x < 0.2 else ">=20%"


def bucket_os(x: float | None) -> str:
    if x is None:
        return "?"
    return "<4" if x < 4 else "4-8" if x < 8 else "8-16" if x < 16 else "16-64" if x < 64 else ">=64"
