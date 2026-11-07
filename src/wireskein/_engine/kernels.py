"""Kernel API: the batch numeric primitives analyzers are built from.

Analyzers and plugins call them with whole arrays, never per edge: the
per-edge work stays in numpy (docs/design.ja.md §2), and these are the named
primitives a native engine would provide.
"""

from __future__ import annotations

import numpy as np


def level_at(edges: np.ndarray, initial: int, pos: np.ndarray) -> np.ndarray:
    n = np.searchsorted(edges, pos, side="right")
    return (initial ^ (n & 1)).astype(np.int8)


def runs(edges: np.ndarray, initial: int, n_samples: int):
    bounds = np.concatenate(([0], edges, [n_samples]))
    start = bounds[:-1]
    length = np.diff(bounds)
    level = (initial ^ (np.arange(len(start)) & 1)).astype(np.int8)
    return start, length, level


def edge_levels(edges: np.ndarray, initial: int) -> np.ndarray:
    """Level after each edge."""
    return (initial ^ ((np.arange(len(edges)) + 1) & 1)).astype(np.int8)


def sample_grid(edges: np.ndarray, initial: int, starts: np.ndarray, offsets: np.ndarray, n_samples: int) -> np.ndarray:
    """Levels at starts[i] + offsets[j] -> matrix (len(starts), len(offsets))."""
    pos = np.minimum(np.floor(starts[:, None] + offsets[None, :]).astype(np.int64), n_samples - 1)
    n = np.searchsorted(edges, pos.ravel(), side="right")
    return (initial ^ (n & 1)).astype(np.int8).reshape(pos.shape)


def chain(starts: np.ndarray, busy_until: np.ndarray) -> np.ndarray:
    """Greedy non-overlapping chain: take start i, then the first start >= busy_until[i]."""
    nxt = np.searchsorted(starts, busy_until, side="left").tolist()
    out, i, n = [], 0, len(starts)
    while i < n:
        out.append(i)
        i = nxt[i]
    return np.asarray(out, dtype=np.int64)


def in_windows(win: np.ndarray, x: np.ndarray):
    """For each x: inside some [start, end) window, and the window index."""
    k = np.searchsorted(win[:, 0], x, side="right") - 1
    if len(win) == 0:                      # no window (a select line that never went active): nothing is inside
        return np.zeros(len(x), dtype=bool), k
    return (k >= 0) & (x < win[np.maximum(k, 0), 1]), k


def nearest_distance(x: np.ndarray, ref: np.ndarray):
    j = np.searchsorted(ref, x)
    a = np.abs(x - ref[np.clip(j, 0, len(ref) - 1)])
    b = np.abs(x - ref[np.clip(j - 1, 0, len(ref) - 1)])
    return np.minimum(a, b), j


def burst_split(t: np.ndarray, ratio: float = 3.0) -> np.ndarray:
    """Indices where a new burst starts in a sorted edge list, decided only from
    neighbouring intervals (no global period): an interval longer than `ratio`
    times both the previous and the next interval is a gap."""
    if len(t) < 3:
        return np.zeros(1, np.int64)
    iv = np.diff(t).astype(np.float64)
    prev = np.concatenate(([np.inf], iv[:-1]))
    nxt = np.concatenate((iv[1:], [np.inf]))
    ref = np.minimum(prev, nxt)
    gap = iv > ratio * ref
    return np.concatenate(([0], np.flatnonzero(gap) + 1)).astype(np.int64)


def relative_phase_concentration(x: np.ndarray, ref: np.ndarray, gap_ratio: float = 3.0) -> float:
    """Phase of each x inside its surrounding ref interval, 0..1 = from the
    previous ref edge to the next one. Frequency independent: the interval
    itself is the unit. x inside gap intervals is ignored."""
    if len(ref) < 3 or len(x) < 2:
        return 0.0
    j = np.searchsorted(ref, x, side="right")
    sel = (j > 0) & (j < len(ref))
    if sel.sum() < 2:
        return 0.0
    iv = np.diff(ref).astype(np.float64)
    prev = np.concatenate(([np.inf], iv[:-1]))
    nxt = np.concatenate((iv[1:], [np.inf]))
    gap = iv > gap_ratio * np.minimum(prev, nxt)
    k = j[sel] - 1
    ok = ~gap[k]
    if ok.sum() < 2:
        return 0.0
    ph = 2 * np.pi * (x[sel][ok] - ref[k[ok]]) / iv[k[ok]]
    r1 = float(np.hypot(np.cos(ph).mean(), np.sin(ph).mean()))
    # Bidirectional lines (RVSWD/SWD DIO: host and target drive at different
    # phases) give two clusters; the doubled angle captures two opposite ones.
    r2 = float(np.hypot(np.cos(2 * ph).mean(), np.sin(2 * ph).mean()))
    return max(r1, r2)


def deglitch(edges: np.ndarray, k: int, allow: np.ndarray | None = None) -> np.ndarray:
    """Drop pulses shorter than k samples: an edge within k samples of the
    previous kept edge cancels it (both edges of the short pulse go).
    allow: per edge, whether it may cancel (None: every edge)."""
    out: list[int] = []
    ok = allow.tolist() if allow is not None else None
    for i, x in enumerate(edges.tolist()):
        if out and x - out[-1] < k and (ok is None or ok[i]):
            out.pop()
        else:
            out.append(x)
    return np.asarray(out, dtype=np.int64)


def local_period(ticks: np.ndarray, at: np.ndarray, w: int = 8, q: float = 25) -> np.ndarray:
    """The clock's period around each tick of `at`, the shorter of the lower quartiles of the `w` intervals of `ticks` (a
    clock's sampling edges) before the one it falls in and the `w` after: a capture whose clock changes speed (slow while connecting,
    fast reading) is fast at a fast frame's ends too, where what follows is slower or idle."""
    ticks, at = np.asarray(ticks, np.int64), np.asarray(at, np.int64)
    if len(ticks) < 2 or not len(at):
        return np.zeros(len(at))
    n = len(ticks) - 1                                       # intervals; interval j is ticks[j] .. ticks[j + 1]
    m = min(w, n)
    j = np.searchsorted(ticks, at, side="right") - 1         # the interval each falls in (-1: before the first)
    out = []
    # the w before the interval it falls in and the w after it (that one left out: a hold at a turnaround or a STOP)
    for j0 in (np.clip(j - m, 0, n - m), np.clip(j + 1, 0, n - m)):
        idx = j0[:, None] + np.arange(m)[None, :]
        # q: the lower quartile by default - a frame's last bits may come slower than its clock (a LinkE at its
        # fastest)
        out.append(np.percentile((ticks[idx + 1] - ticks[idx]).astype(np.float64), q, axis=1))
    return np.minimum(out[0], out[1])


def deglitch_slow(edges: np.ndarray, k: int, clock: np.ndarray) -> np.ndarray:
    """deglitch where the clock is slow enough that no bit is that short (its period around an edge 3 * k samples or
    more); where it is fast, a pulse of a sample or two is a bit and stays."""
    edges = np.asarray(edges, np.int64)
    if len(edges) < 2:
        return edges
    allow = np.zeros(len(edges), bool)
    near = np.flatnonzero(np.diff(edges) < k) + 1               # an edge that could cancel the one before it
    allow[near] = local_period(clock, edges[near]) >= 3 * k
    return deglitch(edges, k, allow)
