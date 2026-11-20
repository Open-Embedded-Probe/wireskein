"""L1: a line's views - its changes with one kind of noise taken out. Stages and protocols pick a view here by name
and take its widths from these constants; none makes its own (bench/notes/2026-11-19-structure-plan.ja.md, stage A).

The kinds of view (each a function of the changes alone: no protocol, no state):

- spikes_out: pulses shorter than k taken out, each change paired with the one before (kernels.deglitch). Only where
  `allow` says (glitch views: where the clock is slow enough for k).
- spikes_out_where_slow: the same where a clock's period around is 3 k or more (kernels.deglitch_slow): a reflection's
  ringing at a slow clock, where no bit is that short.
- pulses_out: the runs shorter than k taken out the shortest first, each with its own two changes
  (kernels.shortest_first): crosstalk's or a spike's pulse never takes a real edge a few ticks off.
- bounces_merged: changes within w of each other are one, at the first (kernels.debounce): where the line first
  crossed, an edge's bounces after it gone.
- gated: changes within w of each other are one, at the last (collapse): a clock's quarter period, for measuring
  how lines keep to it.

And what tells the kinds of noise apart on a line (bounces_paired, noise_runs).

Widths in ticks. Two windows merge a bounce today, as the stages had them: BOUNCE_MERGE (5 ticks: the survey's and
the pulse protocols') and BOUNCE_MERGE_WIDE (8: the async lines' views and the sample edge's second look); making
them one changes outputs (stage B).
"""
from __future__ import annotations

import numpy as np

from . import kernels as K

BOUNCE_TICKS = 4            # a bounce: pulses under this many ticks, within as many of an edge
BOUNCE_MERGE = BOUNCE_TICKS + 1      # ticks: an edge's changes and its bounces' are under this apart
BOUNCE_MERGE_WIDE = 2 * BOUNCE_TICKS  # the same, wider (async views, the sample edge's second look)
BOUNCE_PAIRED = 0.5         # bounces come as two short runs side by side: this share of the short runs in pairs
RING = 3                    # samples: at a clock slow enough, a data line's pulses this short are a reflection ringing
GATE = 4                    # a line's changes closer than a quarter of a clock's period are one, when its pairs are measured


def spikes_out(edges: np.ndarray, k: int, allow: np.ndarray | None = None) -> np.ndarray:
    """Pulses shorter than k taken out (kernels.deglitch); allow: per change, whether it may go."""
    return K.deglitch(edges, k, allow)


def spikes_out_where_slow(edges: np.ndarray, k: int, clock: np.ndarray) -> np.ndarray:
    """Pulses shorter than k taken out where the clock's period around is 3 k or more (kernels.deglitch_slow)."""
    return K.deglitch_slow(edges, k, clock)


def pulses_out(edges: np.ndarray, k: int, middle: bool = False) -> np.ndarray:
    """The runs shorter than k taken out, the shortest first, each with its own two changes (kernels.shortest_first;
    middle: a fast clock's line, three touching short runs a pulse inside one of its runs)."""
    return K.shortest_first(edges, k, middle)


def bounces_merged(edges: np.ndarray, w: int) -> np.ndarray:
    """Changes each within w ticks of the one before are one, at the first (kernels.debounce)."""
    return K.debounce(edges, w)


def gated(edges: np.ndarray, w: float) -> np.ndarray:
    """Changes each within w ticks of the one before are one: an odd number of them a change at the last, an even
    number none (a pulse)."""
    e = np.asarray(edges, np.int64)
    if len(e) < 2 or w < 2:
        return e
    first = np.concatenate(([True], np.diff(e) >= w))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


def bounces_paired(runs: np.ndarray, short: float) -> bool:
    """Whether a line's short runs (under `short`) are a bounce's: BOUNCE_PAIRED of them side by side (a gap after
    the edge, the pulse back), where a spike's stand alone amid long runs. False with none."""
    s = np.asarray(runs) < short
    return bool(s.any()) and float(np.count_nonzero(s[1:] & s[:-1])) * 2 >= BOUNCE_PAIRED * np.count_nonzero(s)


def noise_runs(clock: np.ndarray, short: float) -> np.ndarray:
    """The running count (from 0, one per run) of a clock's runs shorter than `short` beside one of 2 x `short` or
    longer: a noise's (a spike or crosstalk inside a run, a bounce at an edge). A clock too fast for the rate has its
    short highs between runs barely longer (a LinkE at 10 MHz: highs of a sample, lows of 2-3) - not counted."""
    runs = np.diff(np.asarray(clock, np.int64))
    long_ = runs >= 2 * short
    beside = np.zeros(len(runs), bool)
    beside[1:] |= long_[:-1]
    beside[:-1] |= long_[1:]
    return np.concatenate(([0], np.cumsum((runs < short) & beside)))


def broken_runs(clock: np.ndarray, short: float, off: float = 0.25, median: int = 33) -> np.ndarray:
    """The running count (from 0, one per run) of the clock's runs shorter than `short` that break its period: both
    periods through one (rise to rise, or fall to fall) off the median of the periods around by `off`, and not like
    the same period both before and after (two changes away: a clock changing speed keeps its frames). A clock's own
    short highs keep its period (a LinkE at 10 MHz: a sample high in sixteen); a spike or bounce breaks it."""
    ce = np.asarray(clock, np.int64)
    runs = np.diff(ce)
    per = (ce[2:] - ce[:-2]).astype(np.float64)
    if len(per) >= median:
        mid = K.rolling_median(per, median)
        mid = np.concatenate((np.full(median // 2, mid[0]), mid, np.full(median // 2, mid[-1])))
    else:
        mid = np.full(len(per), np.median(per) if len(per) else 0.0)
    pad = np.concatenate(([np.nan, np.nan], per, [np.nan, np.nan]))
    same = (np.abs(per - pad[:-4]) <= off * per) & (np.abs(per - pad[4:]) <= off * per)
    o = (np.abs(per - mid) > off * mid) & ~same
    broken = np.zeros(len(runs), bool)
    broken[1:-1] = o[:-1] & o[1:]
    return np.concatenate(([0], np.cumsum((runs < short) & broken)))


# ---- a clock rebuilt from its period (bounces as long as its half period: no width tells them from its own runs) ----

REBUILD_REFUSE = 0.8     # refuse: after a change it keeps, nothing within this share of the half period
REBUILD_TOL = 0.34       # follow: the next change within this share of a half period of where it is due
REBUILD_GRID = 0.6       # grid: a change within this many ticks of a point of the grid is the change there
REBUILD_BURST = 3        # half periods: a gap this long ends a burst (each burst is rebuilt on its own)


def _half(e: np.ndarray) -> float:
    """A clock's half period, from its longer runs (a bounce's are short)."""
    d = np.diff(e)
    big = d[d >= 0.5 * np.percentile(d, 75)] if len(d) > 4 else d
    return float(np.median(big)) if len(big) else 0.0


def _bursts(e: np.ndarray, h: float) -> list[np.ndarray]:
    cut = np.flatnonzero(np.diff(e) > REBUILD_BURST * h)
    return np.split(e, cut + 1)


def _odd(out: list) -> np.ndarray:
    """Changes in order, two at one tick cancelling (a pulse of no width)."""
    u, c = np.unique(np.asarray(out, np.int64), return_counts=True)
    return u[c % 2 == 1]


def _ends_as_raw(out: list, lv: int, raw: int) -> int:
    """A rebuilt burst ends on the level the line holds after it (the change too many or too few dropped)."""
    if lv != raw and out:
        out.pop()
        lv ^= 1
    return lv


def clock_refused(edges: np.ndarray, initial: int, n: int) -> np.ndarray:
    """In order: a change kept, nothing within REBUILD_REFUSE of a half period after it, and a change put back where a
    gap of 1.5-2.5 half periods says the noise cancelled one; levels alternate; each burst ends on the raw level."""
    e = np.asarray(edges, np.int64)
    h = _half(e) if len(e) >= 8 else 0.0
    if h < 2:
        return e
    out: list[int] = []
    lv = raw = initial
    for b in _bursts(e, h):
        raw ^= len(b) & 1
        last = -(1 << 62)
        for t in b:
            if t - last >= REBUILD_REFUSE * h:
                if 1.5 * h <= t - last < 2.5 * h:
                    out.append(int(last + round(h)))
                    lv ^= 1
                out.append(int(t))
                lv ^= 1
                last = t
        lv = _ends_as_raw(out, lv, raw)
    return _odd(out)


def clock_followed(edges: np.ndarray, initial: int, n: int) -> np.ndarray:
    """As a PLL: per burst, from its first change, each next the change nearest where a half period puts it (within
    REBUILD_TOL of one), or that place itself when none is there; the half period per burst."""
    e = np.asarray(edges, np.int64)
    hg = _half(e) if len(e) >= 8 else 0.0
    if hg < 2:
        return e
    out: list[int] = []
    lv = raw = initial
    for b in _bursts(e, hg):
        raw ^= len(b) & 1
        if len(b) < 4:
            for t in b:
                out.append(int(t))
                lv ^= 1
            lv = _ends_as_raw(out, lv, raw)
            continue
        bd = np.diff(b)
        h = float(np.median(bd[bd >= 0.5 * hg])) if np.any(bd >= 0.5 * hg) else hg
        bl = b.tolist()                  # the due place only moves on: one pointer through the burst, not a search a step
        t, i, end, tol = bl[0], 0, bl[-1], REBUILD_TOL * h
        out.append(t)
        lv ^= 1
        steps = 2 * len(bl) + 16         # a clock has a change at every half period: no more than its own, twice over
        while t + 0.5 * h < end and steps:
            steps -= 1
            due = t + h
            while i < len(bl) and bl[i] < due - tol:
                i += 1
            j, best = i, None
            while j < len(bl) and bl[j] <= due + tol:
                if best is None or abs(bl[j] - due) < abs(best - due):
                    best = bl[j]
                j += 1
            t = int(best) if best is not None else int(round(due))
            out.append(t)
            lv ^= 1
        lv = _ends_as_raw(out, lv, raw)
    return _odd(out)


def clock_gridded(edges: np.ndarray, initial: int, n: int) -> np.ndarray:
    """As a grid: per burst, from its first change, points every h (the fractional half period within 0.6-1.4 of the
    burst's that puts the most changes within REBUILD_GRID ticks of a point), each moved to the change there if any;
    an error at one point does not carry to the next."""
    e = np.asarray(edges, np.int64)
    hg = _half(e) if len(e) >= 8 else 0.0
    if hg < 2:
        return e
    out: list[int] = []
    lv = raw = initial
    for b in _bursts(e, hg):
        raw ^= len(b) & 1
        if len(b) < 6:
            for t in b:
                out.append(int(t))
                lv ^= 1
            lv = _ends_as_raw(out, lv, raw)
            continue
        bf = b.astype(np.float64)
        bd = np.diff(bf)
        h0 = float(np.median(bd[bd >= 0.5 * hg])) if np.any(bd >= 0.5 * hg) else hg
        best = None
        for h in np.arange(0.6 * h0, 1.4 * h0, max(0.01, 0.004 * h0)):
            g = bf[0] + h * np.arange(int((bf[-1] - bf[0]) / h + 0.5) + 1)
            i = np.clip(np.searchsorted(bf, g), 1, len(bf) - 1)
            dist = np.minimum(np.abs(bf[i] - g), np.abs(bf[i - 1] - g))
            sc = np.sum(dist <= REBUILD_GRID) - 0.5 * np.sum(dist > REBUILD_GRID)
            if best is None or sc > best[0]:
                best = (sc, g, i, dist)
        _, g, i, dist = best
        near = np.where(np.abs(bf[i] - g) <= np.abs(bf[i - 1] - g), bf[i], bf[i - 1])
        for t in np.unique(np.where(dist <= REBUILD_GRID, near, np.round(g)).astype(np.int64)):
            out.append(int(t))
            lv ^= 1
        lv = _ends_as_raw(out, lv, raw)
    return _odd(out)


REBUILDS = (("rebuilt_refused", clock_refused), ("rebuilt_followed", clock_followed),
            ("rebuilt_gridded", clock_gridded))
