"""Fine-grained staged narrowing (T1..T5), built on the common survey.

T1 pin class      static / clock / data / sparse / pulse  (static pins leave here)
T2 grouping       single pins, and clock + correlated pins (+ select) sets
T3 family         sync.select / sync.startstop / sync.gap / async / pulse / lone clock
T4 framing        bits per delimited unit -> spi8, i2c9, other-N; UART character length
T5 hypotheses     decoder parameter sets implied by T1..T4

Every stage keeps several candidates with scores; nothing is decided by a hard
top-1 except where the evaluation shows it is safe. Stages are deliberately
small so they can be measured one by one and merged later.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field

import numpy as np

from . import kernels as K
from .survey import Survey, survey

CLASSES = ("clock", "data", "sparse", "pulse")


@dataclass
class PinClass:
    name: str
    scores: dict[str, float]
    candidates: list[str]


@dataclass
class Group:
    kind: str                       # "sync" or "single"
    clock: str | None = None
    data: tuple[str, ...] = ()
    select: str | None = None
    pin: str | None = None          # for single
    score: float = 0.0


@dataclass
class Family:
    group: Group
    family: str                     # sync.select / sync.startstop / sync.gap / async / pulse / lone_clock / irregular
    score: float
    evidence: dict = field(default_factory=dict)


@dataclass
class Framing:
    family: Family
    variant: str                    # spi8 / i2c9 / other / uart-L / pulse-other ...
    score: float
    evidence: dict = field(default_factory=dict)


@dataclass
class Taxonomy:
    survey: Survey
    pins: dict[str, PinClass]
    groups: list[Group]
    families: list[Family]
    framings: list[Framing]
    seconds: dict[str, float]


# ---------------- T1 ----------------

def _two_valued(runs: np.ndarray) -> float:
    """1 when runs fall into two well-separated tight clusters."""
    if len(runs) < 8:
        return 0.0
    lr = np.log(runs.astype(np.float64))
    lo, hi = np.quantile(lr, 0.1), np.quantile(lr, 0.9)
    if hi - lo < np.log(1.6):
        return 0.0
    mid = (lo + hi) / 2
    a, b = lr[lr < mid], lr[lr >= mid]
    if min(len(a), len(b)) < 0.1 * len(lr):
        return 0.0
    tight = np.mean(np.abs(a - np.median(a)) < 0.2) * len(a) / len(lr) + np.mean(np.abs(b - np.median(b)) < 0.2) * len(b) / len(lr)
    return float(tight)


def _constant(runs: np.ndarray) -> float:
    if len(runs) < 8:
        return 0.0
    m = np.median(runs)
    return float(np.mean(np.abs(runs - m) <= max(1.0, 0.25 * m)))


def pin_classes(sv: Survey, cap) -> dict[str, PinClass]:
    max_edges = max((f.n_edges for f in sv.features.values()), default=1)
    out = {}
    for n in sv.active:
        f = sv.features[n]
        ch = cap.channel(n)
        _, length, level = ch.runs(cap.n_samples)
        inner, ilev = length[1:-1], level[1:-1]
        clock = max(f.scores.get("clock", 0.0), f.scores.get("clock_local", 0.0))
        data = max(f.scores.get("async", 0.0), f.scores.get("irregular", 0.0) * 0.6)
        ratio = f.n_edges / max_edges
        sparse = float(np.clip(1 - np.log10(max(ratio, 1e-6) / 0.01) / 2, 0, 1)) if ratio < 0.2 else 0.0
        pulse = 0.0
        if len(inner) >= 16:
            short_hi, short_lo = inner[ilev == 1], inner[ilev == 0]
            # restrict to runs inside activity (drop the long idle gaps)
            cap_len = np.quantile(inner, 0.9) * 3
            short_hi, short_lo = short_hi[short_hi < cap_len], short_lo[short_lo < cap_len]
            pulse = max(_two_valued(short_lo) * _constant(short_hi), _two_valued(short_hi) * _constant(short_lo))
            pulse *= 1 - clock  # a clock is two-valued trivially only if duty != 50%; keep them apart
        sc = {"clock": clock, "data": data, "sparse": sparse, "pulse": pulse}
        best = max(sc.values())
        cands = [k for k, v in sc.items() if v >= 0.2 and v >= 0.5 * best] or [max(sc, key=sc.get)]
        out[n] = PinClass(n, sc, cands)
    return out


# ---------------- T2 ----------------

DATA_PAIR = 0.3
SELECT_BOUNDARY = 0.25  # candidates only; plugins re-measure on the filtered view


def groups(sv: Survey, pins: dict[str, PinClass]) -> list[Group]:
    out = []
    for c in sv.active:
        if "clock" not in pins[c].candidates or c not in sv.clocks:
            continue
        rels = [r for r in sv.partners(c)]
        data = tuple(sorted(r.other for r in rels if r.data_score >= DATA_PAIR))
        sel = [r.other for r in rels if r.boundary >= SELECT_BOUNDARY and "sparse" in pins[r.other].candidates]
        clk_score = sv.clocks[c].clock_score
        for s in [None, *sel]:
            d_all = tuple(x for x in data if x != s)
            if not d_all:
                continue
            # data-pin subsets: all partners, the strong ones, and each alone
            strong = tuple(x for x in d_all if sv.pairs[(c, x)].data_score >= 0.6)
            variants = {d_all, strong, *[(x,) for x in d_all]} - {()}
            for d in variants:
                sc = clk_score * float(np.mean([sv.pairs[(c, x)].data_score for x in d]))
                if s:
                    sc *= sv.pairs[(c, s)].boundary
                out.append(Group("sync", clock=c, data=d, select=s, score=sc))
    for n in sv.active:
        pc = pins[n]
        out.append(Group("single", pin=n, score=max(pc.scores.values())))
    return out


# ---------------- T3 ----------------

def _startstop(cap, sv: Survey, clk: str, data: tuple[str, ...]) -> float:
    """Share of clock bursts that open with a data edge while the clock is high
    (START-like) and close with one after the last clock (STOP-like)."""
    ci = sv.clocks[clk]
    c = cap.channel(clk)
    best = 0.0
    for d in data:
        de = cap.channel(d).edges
        if len(de) == 0:
            continue
        clk_hi = c.level_at(de) == 1
        dh = de[clk_hi]
        if len(dh) == 0:
            continue
        b0, b1 = ci.bursts[:, 0], ci.bursts[:, 1]
        # START-like: a data edge while the clock is high in the idle gap before the
        # burst's first clock; STOP-like: one after its last clock (frequency independent)
        gap_before = np.concatenate(([0.0], b1[:-1]))
        gap_after = np.concatenate((b0[1:], [float(cap.n_samples)]))
        j = np.searchsorted(dh, gap_before)
        start = (j < len(dh)) & (dh[np.minimum(j, len(dh) - 1)] <= b0 + (b1 - b0) * 0.05 + 1)
        j = np.searchsorted(dh, b1 - (b1 - b0) * 0.05)
        stop = (j < len(dh)) & (dh[np.minimum(j, len(dh) - 1)] <= gap_after)
        best = max(best, float(np.mean(start & stop)))
    return best


def families(sv: Survey, cap, pins: dict[str, PinClass], gs: list[Group]) -> list[Family]:
    out = []
    for g in gs:
        if g.kind == "sync":
            if g.select:
                out.append(Family(g, "sync.select", g.score))
                continue
            ss = _startstop(cap, sv, g.clock, g.data)
            out.append(Family(g, "sync.startstop", g.score * ss, {"startstop": ss}))
            out.append(Family(g, "sync.gap", g.score * (1 - ss), {"startstop": ss}))
        else:
            sc = pins[g.pin].scores
            out.append(Family(g, "async", sc["data"] * (1 - 0.5 * sc["clock"])))
            out.append(Family(g, "pulse", sc["pulse"]))
            out.append(Family(g, "lone_clock", sc["clock"]))
    return out


# ---------------- T4 ----------------

def _window_bits(cap, clk: str, sel: str, sv: Survey) -> np.ndarray:
    c, s = cap.channel(clk), cap.channel(sel)
    rising = c.edges[K.edge_levels(c.edges, c.initial) == 1]
    start, length, level = s.runs(cap.n_samples)
    idle = sv.features[sel].idle_level
    act = level != idle
    win = np.stack([start[act], start[act] + length[act]], 1)
    inside, k = K.in_windows(win, rising)
    counts = np.bincount(k[inside], minlength=len(win)) if inside.any() else np.zeros(len(win), np.int64)
    return counts[counts > 0]


def framings(sv: Survey, cap, fams: list[Family]) -> list[Framing]:
    out = []
    for fm in fams:
        g = fm.group
        if fm.family in ("sync.startstop", "sync.gap"):
            bits = sv.clocks[g.clock].bits
            m9 = float(np.mean(np.isin(bits % 9, (0, 1))))
            m8 = float(np.mean(bits % 8 == 0))
            if fm.family == "sync.startstop":
                out.append(Framing(fm, "i2c9", fm.score * m9, {"mod9": m9}))
                vals, cnt = np.unique(bits, return_counts=True)
                top = vals[np.argsort(cnt)[::-1][:3]].tolist()
                out.append(Framing(fm, "startstop-other", fm.score * (1 - m9), {"top_bits": top}))
            else:
                out.append(Framing(fm, "spi8", fm.score * m8, {"mod8": m8}))
                out.append(Framing(fm, "gap-other", fm.score * (1 - m8), {"mod8": m8}))
        elif fm.family == "sync.select":
            bits = _window_bits(cap, g.clock, g.select, sv)
            m8 = float(np.mean(bits % 8 == 0)) if len(bits) else 0.0
            out.append(Framing(fm, "spi8", fm.score * m8, {"mod8": m8, "windows": int(len(bits))}))
            out.append(Framing(fm, "select-other", fm.score * (1 - m8), {"mod8": m8}))
        elif fm.family == "async":
            a = sv.asyncs.get(g.pin)
            if a and a.char_bits:
                best = a.char_bits[0][1]
                for L, r in a.char_bits:
                    if r >= best - 0.02:
                        out.append(Framing(fm, f"uart-L{L}", fm.score * r, {"char_bits": L}))
            else:
                out.append(Framing(fm, "uart-L?", fm.score * 0.5))
        else:
            out.append(Framing(fm, fm.family + "-other", fm.score))
    return out


def classify(cap, sv: Survey | None = None) -> Taxonomy:
    t = {}
    t0 = time.perf_counter()
    sv = sv or survey(cap)
    t["survey"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    pins = pin_classes(sv, cap)
    t["T1"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    gs = groups(sv, pins)
    t["T2"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    fams = families(sv, cap, pins, gs)
    t["T3"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    frs = framings(sv, cap, fams)
    t["T4"] = time.perf_counter() - t0
    return Taxonomy(sv, pins, gs, fams, frs, t)
