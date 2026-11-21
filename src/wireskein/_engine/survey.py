"""Common pre-analysis (共通の下調べ), independent of any protocol decoder.

Estimates that several protocols need are computed once here: clock periods and
bits per clock burst (word length), async bit time and character length, and
pin-to-pin correlation against every clock-like channel. Decoders and exclusion
rules consume these instead of re-deriving them.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace

import numpy as np

from . import kernels as K
from . import views as V
from .views import GATE, RING, gated as collapse   # (callers' names; the views are L1's)
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
    selects: np.ndarray | None = None   # the clock's bursts (of 2 bits or more, by index) the line frames at its active level
    n_real: int = 0                     # the clock's bursts of 2 bits or more
    selects_idle: np.ndarray | None = None   # those it frames at the other level (with other targets' transfers more
                                             # numerous than its own, its own may be there: taxonomy._select_set)

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
    gated: dict[str, tuple] = field(default_factory=dict)   # clock -> (its gated view of the capture, the clock there)
    select_ambiguous: set = field(default_factory=set)      # clocks several sets of select lines frame (taxonomy)

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
    # the bursts from the clock with its spikes in the idle out: a run of a sample between two SPIKE_IDLE times its
    # median run or longer (a select framing the bursts frames none of those: spikes in the gaps cut its framing in
    # five). A clock's own short highs inside its frames stay (a LinkE at 10 MHz: lows of 2-3 beside them)
    e = np.asarray(ch.edges, np.int64)
    if len(e) > 4:
        runs = np.diff(e)
        idle = runs >= SPIKE_IDLE * float(np.median(runs))
        spike = np.zeros(len(runs), bool)
        spike[1:-1] = (runs[1:-1] <= ch.step) & idle[:-2] & idle[2:]
        if spike.any():
            k = np.flatnonzero(spike)
            d = np.delete(e, np.concatenate((k, k + 1)))
            rd = d[K.edge_levels(d, ch.initial) == 1]
            if len(rd) >= 4:
                r = rd
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


SPIKE_PERIOD = 4        # samples: a clock's period this long or more has halves of two samples, longer than a spike
SPIKE_BROKEN = 0.35     # a clock's pulses of a sample this share or more breaking its period: a noise's
SPIKE_IDLE = 4          # a clock's run this many times its median or longer: idle (a sample's pulse between two: a spike)
GATE_KEEPS = 3          # a line keeping under a third of its changes through a clock's gate is measured without it
USED_SELECTIONS = 0.8   # a select line's windows at its active level holding a transfer: this share or more


def gated_view(cap: Capture, clk: ClockInfo, w: float = GATE) -> tuple[Capture, ClockInfo]:
    """The capture as a clock sees it: every line's changes closer than T / w merged (T: the clock's period), so a
    bouncing edge crossing the threshold several times, or a spike near a clock edge, no longer scatters the phase a
    data line keeps to the clock. Only for measuring pairs: the lines themselves are not changed (a real I2C bus's
    clocks clipped short are real). The capture and the clock as they are where the view has no clock."""
    if not clk.period:
        return cap, clk
    if clk.period / w < 2:
        # a clock too fast for a quarter of it to reach two samples: the pulses of a single sample taken out (a spike,
        # crosstalk), each with its own two changes, where the clock's halves are two samples or more
        cch = cap.channel(clk.name)
        step = cch.step
        if clk.period < SPIKE_PERIOD * step:
            return cap, clk
        # and where the clock's own pulses of a sample are a noise's: most of them break its period (a spike); a
        # clock of uneven duty has highs of a sample that keep it (a LinkE's V203 at 50 MS/s: 0.24 of its runs)
        ce = np.asarray(cch.edges, np.int64)
        one = int(np.count_nonzero(np.diff(ce) <= step))
        if one and V.broken_runs(ce, 2 * step)[-1] < SPIKE_BROKEN * one:
            return cap, clk
        chans = [replace(x, edges=V.pulses_out(x.edges, 2 * x.step)) for x in cap.channels]
        if all(len(a.edges) == len(b.edges) for a, b in zip(chans, cap.channels)):
            return cap, clk
        view = replace(cap, channels=chans)
    else:
        view = replace(cap, channels=[replace(x, edges=collapse(x.edges, clk.period / w)) for x in cap.channels])
    vch = view.channel(clk.name)
    if len(vch.edges) < 8:
        return cap, clk
    from .features import channel_features
    vci = clock_info(view, channel_features(view, vch))
    return (view, vci) if vci is not None else (cap, clk)


def despiked_relation(cap: Capture, clk_name: str, other: str, k: int) -> tuple[PairRelation, ClockInfo] | None:
    """The relation of a clock and a line with both lines' pulses shorter than k samples taken out (bounces at their
    edges), and the clock there: how the line keeps to the clock under that noise. None where it is no clock there."""
    from .features import channel_features
    chans = [replace(x, edges=V.pulses_out(x.edges, k)) if x.name in (clk_name, other) else x for x in cap.channels]
    view = replace(cap, channels=chans)
    vch = view.channel(clk_name)
    if len(vch.edges) < 8:
        return None
    vci = clock_info(view, channel_features(view, vch))
    return (pair_relation(view, vci, other), vci) if vci is not None else None


DATA_PER_PERIOD = 4        # a data line's changes in one clock period at most (its change, spikes): more, another bus's
DATA_DENSE = 1.5           # and on average over the clock's periods: more, a pulse train of its own
DATA_DENSE_PERIODS = 16    # periods of the clock's own length, at least, for that average


def pair_relation(cap: Capture, clk: ClockInfo, other: str) -> PairRelation:
    c, d = cap.channel(clk.name), cap.channel(other)
    de = d.edges
    if d.step == 1 and len(de):
        # a reflection ringing at the clock's edges scatters the line's changes over the period: read as it settles
        # where the clock is slow enough that no bit is that short
        de = V.spikes_out_where_slow(np.asarray(de), RING, _rising(c))
        # and a pulse of a single sample where the clock's period is 6 samples or more (too fast for RING's 9):
        # crosstalk from the clock beside it puts one at each of its edges, in phase with it by that alone (2026-11-21,
        # synth-mixed-171 with crosstalk at 8 samples a period: another bus's CS and a PWM taken as data)
        de = V.spikes_out_where_slow(de, 2, _rising(c))
    if len(de) == 0:
        return PairRelation(clk.name, other, 0, 0, 0, 0, 0, 0)
    inside, k = K.in_windows(clk.bursts.astype(np.int64), de)
    rise = _rising(c)
    fall = c.edges[K.edge_levels(c.edges, c.initial) == 0]
    di = de[inside]
    # phase inside the surrounding clock interval (frequency independent)
    pr = K.relative_phase_concentration(di, rise) if len(di) >= 2 and len(rise) > 2 else 0.0
    pf = K.relative_phase_concentration(di, fall) if len(di) >= 2 and len(fall) > 2 else 0.0
    # a data line changes about once a clock period (a spike adds two): changes crowded in one period are another,
    # faster bus's (an SWD packet at 10 MHz inside a PS/2 clock's 80 us: its edges all at about one phase of it,
    # concentrated by that alone, 2026-11-21 synth-protomix-12)
    # Only in periods of the clock's own length: one held long (RVSWD's start and stop, DIO moving while CLK
    # stays high; a wait) holds several changes of its own data
    if len(di) and len(rise) > 2:
        j = np.searchsorted(rise, di)
        per_len = np.diff(rise)
        ok = (j > 0) & (j < len(rise))
        jj = j[ok]
        normal = per_len[jj - 1] <= 2 * float(np.median(per_len))
        per = np.bincount(jj[normal], minlength=len(rise) + 1)
        crowded = float(per[per > DATA_PER_PERIOD].sum()) / len(di)
        pr *= 1 - crowded
        pf *= 1 - crowded
        # and a data line changes at most once a period (NRZ: half the periods for random bits); one changing twice a
        # period on average is a pulse train of its own beside the clock (four WS281x lines driven together:
        # sigrok-dumps' ws281x_4ch, each rising with the others at every bit, each tried as every other's data)
        periods = int(np.count_nonzero(per_len <= 2 * float(np.median(per_len))))
        if periods >= DATA_DENSE_PERIODS and np.count_nonzero(normal) > DATA_DENSE * periods:
            pr = pf = 0.0
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
    lv0 = d.initial ^ (j0 & 1)
    # a line every burst of which lies inside one of its windows at one level, two windows or more holding them, is
    # a selection holding several transfers (a session), however far apart: a line unrelated to the bus has bursts
    # at both its levels
    whole = framed.all() and len(np.unique(lv0)) == 1 and len(np.unique(w)) >= 2
    if whole:
        # and its selections are used: nearly every whole window at that level holds a transfer (a PWM's low windows
        # may each fall around a few bursts, most of them empty)
        held = len(np.unique(w))
        at = int(np.count_nonzero(np.diff(de) > 0)) // 2 if len(de) > 2 else 0     # windows at one level, about
        whole = held >= USED_SELECTIONS * max(held, at)
    for k in (() if whole else np.unique(w[w >= 0])):
        sel = w == k
        if sel.sum() > 1:
            f, l = first[sel], last[sel]
            lead, trail = f.min() - de[k - 1], de[k] - l.max()
            if lead + trail > l.max() - f.min() or (f[1:] - l[:-1]).max() > 2 * (l - f).max():
                framed &= ~sel
    lv = d.initial ^ (j0 & 1)
    active = int(np.bincount(lv[framed], minlength=2).argmax()) if framed.any() else 0
    # the one burst in the window after the line's last change, at the active level, is framed too when the capture
    # ends in that window (a selection the capture's end cut: its closing edge was never recorded)
    cut = (j0 == j1) & (j0 == len(de)) & (j0 > 0) & (lv == active) & framed.any()
    if np.count_nonzero(cut) == 1 and len(de) and de[-1] < cap.n_samples:
        framed = framed | cut
    # and so is the one burst before the line's first change, the line at the active level from the capture's start
    # (a selection the capture's start cut: the capture began inside it)
    cut = (j0 == j1) & (j0 == 0) & (lv == active) & framed.any()
    if np.count_nonzero(cut) == 1 and len(de):
        framed = framed | cut
    boundary = float(np.mean(framed & (lv == active)))
    return PairRelation(clk.name, other, float(inside.mean()), pr, pf, coincide, boundary,
                        float(inside.sum() / max(1, len(clk.bursts))), int(len(clk.bursts)),
                        np.flatnonzero(framed & (lv == active)), int(len(framed)),
                        np.flatnonzero(framed & (lv != active)))


def _despiked_select(cap: Capture, clk: ClockInfo, other: str, r: PairRelation) -> PairRelation:
    """A select's pulses of a single sample (the shortest a line shows: a spike, crosstalk) cut none of its windows:
    its framing measured with them taken out too, where that frames more (a clock too fast for the gate leaves them)."""
    e = cap.channel(other).edges
    d = collapse(e, 2)
    if len(d) == len(e) or r.boundary >= 1.0:
        return r
    s = pair_relation(replace(cap, channels=[x if x.name != other else replace(x, edges=d) for x in cap.channels]),
                      clk, other)
    if s.boundary <= r.boundary:
        return r
    return replace(r, boundary=s.boundary, selects=s.selects, n_real=s.n_real, selects_idle=s.selects_idle)


def better_relation(cap: Capture, clk: ClockInfo, other: str, gated: tuple | None = None) -> PairRelation:
    """The pair measured on the lines as they are and on the clock's gated view (gated_view; given when made
    already), the better kept: the gate takes a noise's scatter out of a data line's phase and a select's spikes out
    of its framing; the lines as they are keep a clock's real short pulses (an I2C clock a slow rise clipped short,
    a clock running four times as fast in some frames), which the gate would merge."""
    r = _despiked_select(cap, clk, other, pair_relation(cap, clk, other))
    view, vc = gated or gated_view(cap, clk)
    # a line the gate takes most of the changes out of moves faster than this clock (it is no data of it: the real
    # clock of a bus, seen from its data line tried as a clock): measured as it is only. Counted against its changes
    # but pulses of a couple of ticks (a spike, crosstalk: what the gate is for, not a faster line)
    if view is cap:
        return r
    n_raw, n_view = len(cap.channel(other).edges), len(view.channel(other).edges)
    if n_view == n_raw and len(view.channel(clk.name).edges) == len(cap.channel(clk.name).edges):
        return r                    # the gate changed neither line: the same two lines, the same relation
    spikes_out = clk.period / GATE < 2      # gated_view's view for a fast clock: its pulses of a sample out
    if spikes_out or n_view * GATE_KEEPS >= len(collapse(cap.channel(other).edges, 3)):
        g = pair_relation(view, vc, other)
        # the line as data of the clock and as its select are two measures: each the better of the two views (a
        # select's spikes cut its framing on the lines as they are, while its few changes may keep to the clock no
        # worse there). With the spikes out, as data that view's only: a line whose changes in the clock's bursts
        # are pulses of a sample (crosstalk from the bus's data line) is no data of it
        data = g if spikes_out or g.data_score > r.data_score else r
        sel = g if g.boundary > r.boundary else r
        if data is sel:
            return data
        return replace(data, boundary=sel.boundary, selects=sel.selects, n_real=sel.n_real,
                       selects_idle=sel.selects_idle)
    return r


BOUNCED_CLOCK = 0.8       # a line this clock-like with its bounces out, and DEBOUNCE_GAIN more than as it is
DEBOUNCE_GAIN = 0.2
BOUNCED_SHARE = 0.05      # a line's changes this share or more within BOUNCE_TICKS of the one before: it bounces


def _debounced_clock(cap: Capture, name: str, f: ChannelFeatures):
    """(features, clock info) of the line with its bounces taken out, when that is a clear clock and as it is it was
    not; else (None, None)."""
    ch = cap.channel(name)
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 32 or f.scores.get("clock", 0) >= BOUNCED_CLOCK:
        return None, None
    gaps = np.diff(e)
    tries = []
    # a bounce's mark: a share of the line's changes come within BOUNCE_TICKS of the one before; each edge's
    # bounces merged at its first change, where the runs left are twice the window
    # (wider windows too: a fast capture's bounces last more ticks, 200 MS/s sees 60 ns as 12; each window kept only
    # where the runs left are twice it)
    if float(np.mean(gaps < V.BOUNCE_TICKS * ch.step)) >= BOUNCED_SHARE:
        for k in (w * ch.step for w in BOUNCE_WINDOWS if w == V.BOUNCE_MERGE or w * ch.step <= BOUNCE_MAX_S * cap.rate):
            d = V.bounces_merged(e, k)
            if len(d) != len(e) and len(d) >= 16 and float(np.median(np.diff(d))) >= 2 * k:
                tries.append(d)
    # crosstalk's or a spike's mark: pulses of a single sample; those runs taken out with their own two edges
    if float(np.mean(gaps <= ch.step)) >= BOUNCED_SHARE:
        d = V.pulses_out(e, 2 * ch.step)
        if len(d) != len(e) and len(d) >= 16 and float(np.median(np.diff(d))) >= 3 * ch.step:
            tries.append(d)
    from .features import channel_features
    best = None
    for d in tries:
        view = replace(cap, channels=[x if x.name != name else replace(x, edges=d) for x in cap.channels])
        fb = channel_features(view, view.channel(name))
        sc = fb.scores.get("clock", 0)
        if sc >= BOUNCED_CLOCK and sc >= f.scores.get("clock", 0) + DEBOUNCE_GAIN and (best is None or sc > best[0]):
            best = (sc, fb, clock_info(view, fb))
    return (best[1], best[2]) if best else (None, None)


BOUNCE_WINDOWS = (V.BOUNCE_MERGE, 8, 12)    # ticks: the windows a clock's bounces are merged in, tried in turn
BOUNCE_MAX_S = 60e-9   # ... the wider ones only as long as a bounce lasts (200 MS/s: 12 ticks); slower, a fast clock's
                       # half period is as short (a clock that changes speed lost its fast stretch's edges)


def survey(cap: Capture, fs: dict[str, ChannelFeatures] | None = None, asyncs: bool = True) -> Survey:
    """asyncs: False leaves the asynchronous reading out (character lengths, bit times): no protocol tried needs it."""
    given = fs is not None
    fs = fs or features(cap)
    active = [n for n, f in fs.items() if not f.static]
    clocks = {}
    for n in active:
        ci = clock_info(cap, fs[n])
        if not given:
            # a clock whose edges bounce (pulses back of a few ticks at half of them) reads as no clock: its runs
            # and period are the bounces'. Its features from the line with the pulses under BOUNCE_TICKS taken out
            # where that is a clear clock and the line's runs are long enough to keep
            fb, cb = _debounced_clock(cap, n, fs[n])
            if fb is not None:
                fs[n], ci = fb, cb
        if ci is not None:
            clocks[n] = ci
    asyncs = {n: async_info(cap, fs[n]) for n in active} if asyncs else {}
    s = Survey(fs, active, clocks, asyncs)
    from . import methods
    swapped = methods.get("pair_relation", None)
    for c in clocks.values():
        # each pair measured on the lines as they are and on the clock's gated view (made once a clock), the better
        # kept: the gate takes a noise's scatter out of a data line's phase; a clock's real short pulses (an I2C clock
        # a slow rise clipped short, a clock running four times as fast in some frames) are kept where it would merge
        # them; a swapped step on the capture as it is
        view, vc = (cap, c) if swapped else gated_view(cap, c)
        s.gated[c.name] = (view, vc)
        for o in active:
            if o != c.name and not same_changes(cap, c.name, o):
                s.pairs[(c.name, o)] = swapped(cap, c, o) if swapped else better_relation(cap, c, o, (view, vc))
    return s


SAME_CHANGES = 0.95     # of each line's changes, this share at the other's (within a tick): one signal on two lines


def same_changes(cap: Capture, a: str, b: str) -> bool:
    """Two lines changing together, change for change: a differential pair (RS-485's A and B: DMX512, a UART over
    it; USB's D+ and D-) or one net probed twice. Neither is a clock and its data (the "data" carries nothing the
    clock does not), whatever the clock's regularity makes of it."""
    ea = np.asarray(cap.channel(a).edges, np.int64)
    eb = np.asarray(cap.channel(b).edges, np.int64)
    if len(ea) < 8 or len(eb) < 8 or not 0.9 <= len(ea) / len(eb) <= 1.1:
        return False
    tick = max(cap.channel(a).step, cap.channel(b).step, 1)

    def near(x, y):
        i = np.clip(np.searchsorted(y, x), 1, len(y) - 1)
        return float(np.mean(np.minimum(np.abs(x - y[i - 1]), np.abs(y[i] - x)) <= tick))
    return near(ea, eb) >= SAME_CHANGES and near(eb, ea) >= SAME_CHANGES
