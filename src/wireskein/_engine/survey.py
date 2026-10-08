"""Common pre-analysis (共通の下調べ), independent of any protocol decoder.

Estimates that several protocols need are computed once here: clock periods and
bits per clock burst (word length), async bit time and character length, and
pin-to-pin correlation against every clock-like channel. Decoders and exclusion
rules consume these instead of re-deriving them.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from . import kernels as K
from .features import ChannelFeatures, features
from .model import Capture


@dataclass
class ClockInfo:
    name: str
    period: float                   # samples, dominant rising-to-rising interval
    clock_score: float
    idle_level: int
    bursts: np.ndarray              # (start, end) samples of clock bursts
    bits: np.ndarray                # rising edges per burst
    mod8: float                     # share of bursts with bits % 8 == 0
    mod9: float                     # share with bits % 9 in (0, 1)  (I2C: 9n, +1 for the STOP rise)
    word_candidates: list[int]      # likely word lengths from the burst lengths


@dataclass
class AsyncInfo:
    name: str
    unit: float | None              # samples per bit
    char_bits: list[tuple[int, float]]  # (bits per character incl. start/stop, stop-bit pass rate), best first
    idle_level: int | None


@dataclass
class PairRelation:
    clock: str
    other: str
    co_activity: float    # share of other's edges inside clock bursts
    phase_rise: float     # phase concentration of other's edges vs the clock's rising edges
    phase_fall: float
    coincide: float       # share of other's edges within 1 sample of any clock edge
    boundary: float       # share of clock bursts with an edge of other near both ends (CS-like)
    other_edges_per_burst: float
    n_bursts: int = 1

    @property
    def data_score(self) -> float:
        # a phase concentration from a handful of edges is weak evidence
        n_inside = self.other_edges_per_burst * max(1, self.n_bursts)
        quantity = 1.0 - np.exp(-n_inside / 8.0)
        return self.co_activity * max(self.phase_rise, self.phase_fall) * quantity

    @property
    def select_score(self) -> float:
        return self.boundary


@dataclass
class Survey:
    features: dict[str, ChannelFeatures]
    active: list[str]
    clocks: dict[str, ClockInfo]
    asyncs: dict[str, AsyncInfo]
    pairs: dict[tuple[str, str], PairRelation] = field(default_factory=dict)

    def partners(self, clock: str) -> list[PairRelation]:
        return sorted((r for (c, _), r in self.pairs.items() if c == clock), key=lambda r: -r.data_score)


def _rising(ch) -> np.ndarray:
    return ch.edges[K.edge_levels(ch.edges, ch.initial) == 1]


def clock_info(cap: Capture, f: ChannelFeatures) -> ClockInfo | None:
    """Clock bursts are split from neighbouring intervals only, so a clock whose
    frequency changes between (or within) bursts is handled the same way."""
    ch = cap.channel(f.name)
    r = _rising(ch)
    if len(r) < 4:
        return None
    starts = K.burst_split(r)
    ends = np.concatenate((starts[1:] - 1, [len(r) - 1]))
    bits = (ends - starts + 1).astype(np.int64)
    iv = np.diff(r).astype(np.float64)
    # half a local interval of margin around each burst
    m0 = np.array([iv[s] if s < len(iv) else iv[-1] for s in starts]) / 2
    m1 = np.array([iv[e - 1] if e > 0 else iv[0] for e in ends]) / 2
    bursts = np.stack([r[starts] - m0, r[ends] + m1], 1)
    mod8 = float(np.mean(bits % 8 == 0))
    mod9 = float(np.mean(np.isin(bits % 9, (0, 1))))
    words = []
    for w in range(4, 65):
        share = float(np.mean(bits % w == 0))
        if share >= 0.8 and bits.min() >= w:
            words.append(w)
    score = max(f.scores.get("clock", 0.0), f.scores.get("clock_local", 0.0))
    period = float(np.median(iv)) if len(iv) else float(f.period or 0)
    return ClockInfo(f.name, period, score, f.idle_level or 0, bursts, bits, mod8, mod9, words[:6])


def char_length(cap: Capture, pin: str, idle: int, unit: float, s0: int = 0, s1: int | None = None) -> list[tuple[int, float]]:
    """Character length L (bits incl. start and stop) from the stop-bit test
    alone: chain frames of L bits from each start edge in [s0, s1) and check that
    the start sample is active and the sample at L-0.5 bits is idle. A wrong L
    lands the stop sample in a data bit (random) or in the next start bit (fails
    on back-to-back characters). Shared by the whole-line survey and by every
    rate block. Returns [(L, pass rate)] best first."""
    ch = cap.channel(pin)
    s1 = cap.n_samples if s1 is None else s1
    lv = K.edge_levels(ch.edges, ch.initial)
    e = ch.edges
    starts = e[(lv != idle) & (e >= s0) & (e < s1)].astype(np.float64)
    if len(starts) < 4:
        return []
    grid = K.sample_grid(e, ch.initial, starts, (np.arange(14) + 0.5) * unit, cap.n_samples)
    if idle == 0:
        grid = 1 - grid
    rates = []
    for L in range(8, 14):
        idx = K.chain(starts, starts + (L - 0.5) * unit)
        ok = (grid[idx, 0] == 0) & (grid[idx, L - 1] == 1)
        rates.append((L, float(ok.mean()) if len(idx) else 0.0))
    rates.sort(key=lambda x: (-round(x[1], 2), x[0]))
    return rates[:4]


def async_info(cap: Capture, f: ChannelFeatures) -> AsyncInfo:
    if not f.units or f.idle_level is None:
        return AsyncInfo(f.name, None, [], f.idle_level)
    u = f.units[0].samples
    return AsyncInfo(f.name, u, char_length(cap, f.name, f.idle_level, u), f.idle_level)


def pair_relation(cap: Capture, clk: ClockInfo, other: str) -> PairRelation:
    c, d = cap.channel(clk.name), cap.channel(other)
    de = d.edges
    if len(de) == 0:
        return PairRelation(clk.name, other, 0, 0, 0, 0, 0, 0)
    inside, k = K.in_windows(clk.bursts.astype(np.int64), de)
    rise = _rising(c)
    fall = c.edges[K.edge_levels(c.edges, c.initial) == 0]
    di = de[inside]
    # phase inside the surrounding clock interval (frequency independent)
    pr = K.relative_phase_concentration(di, rise) if len(di) >= 2 and len(rise) > 2 else 0.0
    pf = K.relative_phase_concentration(di, fall) if len(di) >= 2 and len(fall) > 2 else 0.0
    dist, _ = K.nearest_distance(de, c.edges)
    coincide = float(np.mean(dist <= max(c.step, d.step)))   # within one sample of the coarser line
    # CS-like: every burst lies between two edges of `other`, with none inside it,
    # and at the same (active) level. Several bursts may share one window (bytes
    # with gaps under one CS), so a single CS frame counts. A lone rising edge is
    # not a transfer (the clock going to its idle level before the first one).
    # The span is from the burst's first clock edge to its last (not the margin
    # around it: a CS edge may sit within half a clock of the first clock).
    real = clk.bits >= 2
    b = clk.bursts[real] if real.any() else clk.bursts
    ce = c.edges
    first = ce[np.minimum(np.searchsorted(ce, b[:, 0], side="left"), len(ce) - 1)]
    last = ce[np.maximum(np.searchsorted(ce, b[:, 1], side="right") - 1, 0)]
    j0 = np.searchsorted(de, first, side="left")
    j1 = np.searchsorted(de, last, side="right")
    framed = (j0 == j1) & (j0 > 0) & (j0 < len(de))
    # a window holding several bursts must look like one transfer, or any slow
    # line would frame them all: tight around them (lead + trail no longer than
    # the clocked span) and gaps between them like those between bytes (no
    # longer than twice the longest burst)
    w = np.where(framed, j0, -1)
    for k in np.unique(w[w >= 0]):
        sel = w == k
        if sel.sum() > 1:
            f, l = first[sel], last[sel]
            lead, trail = f.min() - de[k - 1], de[k] - l.max()
            if lead + trail > l.max() - f.min() or (f[1:] - l[:-1]).max() > 2 * (l - f).max():
                framed &= ~sel
    lv = d.initial ^ (j0 & 1)
    active = int(np.bincount(lv[framed], minlength=2).argmax()) if framed.any() else 0
    boundary = float(np.mean(framed & (lv == active)))
    return PairRelation(clk.name, other, float(inside.mean()), pr, pf, coincide, boundary,
                        float(inside.sum() / max(1, len(clk.bursts))), int(len(clk.bursts)))


def survey(cap: Capture, fs: dict[str, ChannelFeatures] | None = None) -> Survey:
    fs = fs or features(cap)
    active = [n for n, f in fs.items() if not f.static]
    clocks = {}
    for n in active:
        ci = clock_info(cap, fs[n])
        if ci is not None:
            clocks[n] = ci
    asyncs = {n: async_info(cap, fs[n]) for n in active}
    s = Survey(fs, active, clocks, asyncs)
    for c in clocks.values():
        for o in active:
            if o != c.name:
                s.pairs[(c.name, o)] = pair_relation(cap, c, o)
    return s
