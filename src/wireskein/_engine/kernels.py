"""Kernel API: the batch numeric primitives analyzers are built from.

These are the candidates for the fast core (Rust). Analyzers (future plugins)
call them with whole arrays, never per edge. Every call is timed and its input
and output sizes are counted, so the time spent inside the core can be
separated from the analyzer's own glue code.
"""

from __future__ import annotations

import time
from collections import defaultdict
from functools import wraps

import numpy as np

# name -> [calls, seconds, elements_in, elements_out]
STATS: dict[str, list] = defaultdict(lambda: [0, 0.0, 0, 0])
_clock = [0.0]  # total kernel seconds, read by the engine around analyzer calls


def _size(x) -> int:
    if isinstance(x, np.ndarray):
        return int(x.size)
    if isinstance(x, tuple):
        return sum(_size(v) for v in x)
    return 0


def kernel(fn):
    @wraps(fn)
    def wrapper(*args, **kw):
        t0 = time.perf_counter()
        out = fn(*args, **kw)
        dt = time.perf_counter() - t0
        st = STATS[fn.__name__]
        st[0] += 1
        st[1] += dt
        st[2] += sum(_size(a) for a in args)
        st[3] += _size(out)
        _clock[0] += dt
        return out
    return wrapper


def reset() -> None:
    STATS.clear()
    _clock[0] = 0.0


def kernel_seconds() -> float:
    return _clock[0]


@kernel
def level_at(edges: np.ndarray, initial: int, pos: np.ndarray) -> np.ndarray:
    n = np.searchsorted(edges, pos, side="right")
    return (initial ^ (n & 1)).astype(np.int8)


@kernel
def runs(edges: np.ndarray, initial: int, n_samples: int):
    bounds = np.concatenate(([0], edges, [n_samples]))
    start = bounds[:-1]
    length = np.diff(bounds)
    level = (initial ^ (np.arange(len(start)) & 1)).astype(np.int8)
    return start, length, level


@kernel
def edge_levels(edges: np.ndarray, initial: int) -> np.ndarray:
    """Level after each edge."""
    return (initial ^ ((np.arange(len(edges)) + 1) & 1)).astype(np.int8)


@kernel
def sample_grid(edges: np.ndarray, initial: int, starts: np.ndarray, offsets: np.ndarray, n_samples: int) -> np.ndarray:
    """Levels at starts[i] + offsets[j] -> matrix (len(starts), len(offsets))."""
    pos = np.minimum(np.floor(starts[:, None] + offsets[None, :]).astype(np.int64), n_samples - 1)
    n = np.searchsorted(edges, pos.ravel(), side="right")
    return (initial ^ (n & 1)).astype(np.int8).reshape(pos.shape)


@kernel
def chain(starts: np.ndarray, busy_until: np.ndarray) -> np.ndarray:
    """Greedy non-overlapping chain: take start i, then the first start >= busy_until[i]."""
    nxt = np.searchsorted(starts, busy_until, side="left").tolist()
    out, i, n = [], 0, len(starts)
    while i < n:
        out.append(i)
        i = nxt[i]
    return np.asarray(out, dtype=np.int64)


@kernel
def in_windows(win: np.ndarray, x: np.ndarray):
    """For each x: inside some [start, end) window, and the window index."""
    k = np.searchsorted(win[:, 0], x, side="right") - 1
    return (k >= 0) & (x < win[np.maximum(k, 0), 1]), k


@kernel
def nearest_distance(x: np.ndarray, ref: np.ndarray):
    j = np.searchsorted(ref, x)
    a = np.abs(x - ref[np.clip(j, 0, len(ref) - 1)])
    b = np.abs(x - ref[np.clip(j - 1, 0, len(ref) - 1)])
    return np.minimum(a, b), j


@kernel
def phase_concentration(x: np.ndarray, ref: np.ndarray, period: float) -> float:
    """Mean resultant length of the phase of x after the preceding ref."""
    j = np.searchsorted(ref, x)
    sel = j > 0
    if sel.sum() < 2:
        return 0.0
    ph = 2 * np.pi * (x[sel] - ref[j[sel] - 1]) / period
    return float(np.hypot(np.cos(ph).mean(), np.sin(ph).mean()))


@kernel
def pack_bits(bits: np.ndarray, seg_len: np.ndarray, order: str) -> np.ndarray:
    """Pack bits into bytes per segment, dropping incomplete trailing bits."""
    if len(bits) == 0:
        return np.zeros(0, np.int64)
    seg_start = np.concatenate(([0], np.cumsum(seg_len)[:-1]))
    seg_id = np.repeat(np.arange(len(seg_len)), seg_len)
    pos = np.arange(len(bits)) - seg_start[seg_id]
    keep = pos < (seg_len // 8 * 8)[seg_id]
    pos, b, sid = pos[keep], bits[keep].astype(np.int64), seg_id[keep]
    shift = 7 - (pos % 8) if order == "msb" else pos % 8
    byte_offset = np.concatenate(([0], np.cumsum(seg_len // 8)[:-1]))
    byte_id = byte_offset[sid] + pos // 8
    return np.bincount(byte_id, weights=b << shift, minlength=int((seg_len // 8).sum())).astype(np.int64)


@kernel
def merge_events(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """Time-ordered (sample, source) events of two edge lists; source 0 before 1 on ties."""
    ev = np.concatenate([np.stack([a, np.zeros(len(a), np.int64)], 1), np.stack([b, np.ones(len(b), np.int64)], 1)])
    return ev[np.lexsort((ev[:, 1], ev[:, 0]))] if len(ev) else ev


@kernel
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


@kernel
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


@kernel
def deglitch(edges: np.ndarray, k: int) -> np.ndarray:
    """Drop pulses shorter than k samples: an edge within k samples of the
    previous kept edge cancels it (both edges of the short pulse go)."""
    out: list[int] = []
    for x in edges.tolist():
        if out and x - out[-1] < k:
            out.pop()
        else:
            out.append(x)
    return np.asarray(out, dtype=np.int64)
