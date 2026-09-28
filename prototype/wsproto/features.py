"""L1-L3: per-channel statistics, timing structure and soft role scores.

Nothing here decides a protocol. Each channel gets candidate time units and
role scores that later layers use as one input among several.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from .model import Capture, Channel


@dataclass
class UnitCandidate:
    samples: float      # estimated unit length in samples
    fit: float          # 0..1, how well runs are integer multiples of the unit
    support: int        # runs used


@dataclass
class ChannelFeatures:
    name: str
    n_edges: int
    static: bool
    idle_level: int | None          # level holding the longest runs
    duty_high: float                # fraction of time high
    bursts: list[tuple[int, int]]   # active spans (start, end) in samples
    run_hist: dict                  # log2-binned histogram of run lengths per level
    units: list[UnitCandidate]
    period: float | None            # dominant rising-to-rising interval inside bursts
    period_conc: float              # fraction of those intervals within ±10% of period
    distinct_runs: float            # normalized count of distinct run-length clusters
    scores: dict = field(default_factory=dict)  # soft role scores


def _bursts(ch: Channel, gap: float) -> list[tuple[int, int]]:
    e = ch.edges
    if len(e) == 0:
        return []
    split = np.flatnonzero(np.diff(e) > gap)
    starts = np.concatenate(([e[0]], e[split + 1]))
    ends = np.concatenate((e[split], [e[-1]]))
    return list(zip(starts.tolist(), ends.tolist()))


def _circ_fit(runs: np.ndarray, u: float) -> float:
    """Mean resultant length of run/u phases: 1 when every run is an integer multiple of u."""
    ph = 2 * np.pi * runs / u
    return float(np.hypot(np.cos(ph).mean(), np.sin(ph).mean()))


def estimate_units(runs: np.ndarray, max_mult: int = 12, top: int = 6) -> list[UnitCandidate]:
    """Candidate base units from interior run lengths (excluding the first/last partial runs).

    Candidates come from histogram peaks and their integer fractions. A unit is
    scored on runs no longer than max_mult units; tiny units that fit everything
    trivially are penalized because they need large multiples.
    """
    runs = runs[runs > 0].astype(np.float64)
    if len(runs) < 4:
        return []
    hist_vals, counts = np.unique(np.round(runs), return_counts=True)
    order = np.argsort(counts)[::-1][:12]
    peaks = hist_vals[order]
    cands = set()
    for p in peaks:
        for k in (1, 2, 3, 4, 5, 8):
            if p / k >= 1.5:
                cands.add(float(p / k))
    out = []
    for u in sorted(cands):
        sel = runs[runs <= max_mult * u * 1.05]
        if len(sel) < max(4, 0.2 * len(runs)):
            continue
        # refine: least squares on k = round(r/u)
        k = np.maximum(1, np.round(sel / u))
        u2 = float((sel * k).sum() / (k * k).sum())
        k = np.maximum(1, np.round(sel / u2))
        resid = np.abs(sel / u2 - k)
        fit = float(np.mean(resid < 0.2)) * min(1.0, len(sel) / len(runs) / 0.5)
        # quantization: when a unit is only a few samples, ±1 sample is a large fraction
        out.append(UnitCandidate(u2, fit, int(len(sel))))
    if not out:
        return []
    # Every integer fraction of the true unit fits too, so rank by "largest unit
    # whose fit is close to the best", then the rest by fit.
    best = max(c.fit for c in out)
    good = sorted((c for c in out if c.fit >= best - 0.03), key=lambda c: -c.samples)
    rest = sorted((c for c in out if c.fit < best - 0.03), key=lambda c: (-c.fit, -c.samples))
    kept: list[UnitCandidate] = []
    for c in good + rest:
        if all(abs(c.samples - k.samples) / k.samples > 0.03 for k in kept):
            kept.append(c)
        if len(kept) >= top:
            break
    return kept


def channel_features(cap: Capture, ch: Channel) -> ChannelFeatures:
    n = cap.n_samples
    e = ch.edges
    if len(e) == 0:
        return ChannelFeatures(ch.name, 0, True, ch.initial, float(ch.initial), [], {}, [], None, 0.0, 0.0,
                               {"static": 1.0})
    start, length, level = ch.runs(n)
    duty = float(length[level == 1].sum() / n)
    inner = slice(1, len(length) - 1)
    ilen, ilev = length[inner], level[inner]
    # Idle level: the level holding most of the time spent in the longest runs
    # (the 20 longest, which covers inter-frame gaps even on busy lines).
    pool_len, pool_lev = (ilen, ilev) if len(ilen) else (length, level)
    top = np.argsort(pool_len)[-20:]
    t_hi = pool_len[top][pool_lev[top] == 1].sum()
    t_lo = pool_len[top][pool_lev[top] == 0].sum()
    idle = 1 if t_hi >= t_lo else 0
    # A line at rest at both ends of the capture shows its idle level there;
    # the longest runs can be breaks (LIN, DMX512) at the active level.
    final = ch.initial ^ (len(e) & 1)
    if ch.initial == final:
        idle = int(ch.initial)
    med = float(np.median(ilen)) if len(ilen) else float(n)
    bursts = _bursts(ch, gap=max(64 * med, 64))
    hist = {}
    for lv in (0, 1):
        r = ilen[ilev == lv]
        if len(r):
            b = np.floor(np.log2(r)).astype(int)
            vals, cnt = np.unique(b, return_counts=True)
            hist[lv] = {int(v): int(c) for v, c in zip(vals, cnt)}
    # Runs at the active level (start bit + zero bits) are always whole bits;
    # idle-level runs include the gaps between characters, which need not be.
    act = ilen[ilev != idle]
    units = estimate_units(act) if len(act) >= 20 else estimate_units(ilen)
    if not units:
        units = estimate_units(ilen)

    # after edge k the level is initial ^ ((k + 1) & 1)
    rising = e[(ch.initial ^ ((np.arange(len(e)) + 1) & 1)) == 1]
    period, conc = None, 0.0
    if len(rising) >= 4:
        iv = np.diff(rising).astype(np.float64)
        iv = iv[iv < 16 * np.median(iv)]
        if len(iv) >= 3:
            vals, cnt = np.unique(np.round(iv), return_counts=True)
            p = float(vals[np.argmax(cnt)])
            near = iv[np.abs(iv - p) <= max(1.0, 0.1 * p)]
            period = float(near.mean())
            conc = float(len(near) / len(iv))
    # Distinct run-length clusters relative to the smallest unit: a clock has ~2
    # (high and low half-periods), UART/data have many multiples.
    distinct = 0.0
    if units:
        u = units[0].samples
        k = np.round(ilen[ilen <= 12 * u] / u)
        distinct = float(len(np.unique(k)))

    f = ChannelFeatures(ch.name, len(e), False, idle, duty, bursts, hist, units, period, conc, distinct)
    f._rise_iv = np.diff(rising).astype(np.float64) if len(rising) > 1 else None
    f.scores = role_scores(f, ilen, ilev)
    return f


def local_clock(f: "ChannelFeatures") -> float:
    """Frequency-independent clock-likeness: inside bursts, each rising-to-rising
    interval should be close to its neighbour (ratio within 10 %). The period may
    change between bursts or slowly within one (multi-speed clocks such as
    RVSWD's attach vs. transfer speed). Gaps (> 3x both neighbours) are left out."""
    iv = getattr(f, "_rise_iv", None)
    if iv is None or len(iv) < 16:
        return 0.0
    prev = np.concatenate(([np.inf], iv[:-1]))
    nxt = np.concatenate((iv[1:], [np.inf]))
    inb = ~(iv > 3 * np.minimum(prev, nxt))
    r = iv[1:] / iv[:-1]
    both = inb[1:] & inb[:-1]
    if both.sum() < 8:
        return 0.0
    return float(np.mean(np.abs(np.log(r[both])) < np.log(1.1) + 1.0 / np.maximum(iv[1:][both], 1)))


def role_scores(f: ChannelFeatures, ilen: np.ndarray, ilev: np.ndarray) -> dict:
    """Soft, independent scores in 0..1. These are evidence, not a classification."""
    s = {"static": 0.0}
    # clock-like: concentrated rising period and high/low runs close to fixed halves
    clock = 0.0
    if f.period:
        hi = ilen[(ilev == 1) & (ilen < 2 * f.period)]
        lo = ilen[(ilev == 0) & (ilen < 2 * f.period)]
        if len(hi) > 2 and len(lo) > 2:
            mh, ml = np.median(hi), np.median(lo)
            spread = (np.mean(np.abs(hi - mh) <= max(1, 0.15 * mh)) + np.mean(np.abs(lo - ml) <= max(1, 0.15 * ml))) / 2
            clock = f.period_conc * spread
    s["clock"] = float(clock)
    s["clock_local"] = local_clock(f)
    # async-like: many distinct integer multiples of one unit, idle level holds long gaps
    asy = 0.0
    if f.units:
        u = f.units[0]
        variety = min(1.0, max(0.0, (f.distinct_runs - 2) / 4))
        asy = u.fit * variety
    s["async"] = float(asy)
    s["irregular"] = float(1.0 - max((f.units[0].fit if f.units else 0.0), clock))
    return s


def features(cap: Capture) -> dict[str, ChannelFeatures]:
    return {ch.name: channel_features(cap, ch) for ch in cap.channels}
