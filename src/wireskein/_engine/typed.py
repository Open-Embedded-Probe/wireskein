"""Typed streams handed from general stages to specific protocols.

Each stage turns the previous representation into a smaller, better organized
one, and a protocol-specific analyzer receives that typed data instead of raw
edges. `nbytes()` is the size of the representation, the measure used to decide
whether a stage really reduces information (parent/child) or not (sibling /
merge candidate).

    edges -> SyncBits / AsyncSymbols / PulseSymbols -> Frames -> Words -> protocol
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from . import kernels as K
from . import views as V
from .survey import Survey
from .views import RING


def _nb(*arrs) -> int:
    return int(sum(a.nbytes for a in arrs if a is not None))


# ---------------- T2 outputs ----------------

@dataclass
class SyncBits:
    """Bits of every data pin sampled at the sampling edge of one clock."""
    clock: str
    data: tuple[str, ...]
    select: str | None
    sample_edge: str            # "rise" / "fall"
    t: np.ndarray               # int64 sample time of each sampling edge
    bits: np.ndarray            # uint8 (n_data, n_edges)
    burst_start: np.ndarray     # index into t where each clock burst begins

    def nbytes(self) -> int:
        # bits packed, one timestamp per burst start + the burst index table;
        # per-bit times are kept too (needed for delimiters) but counted as int32 offsets
        return int(np.ceil(self.bits.size / 8)) + _nb(self.burst_start) + 4 * len(self.t)


@dataclass
class AsyncSymbols:
    """Runs of one pin quantized to a bit time."""
    pin: str
    unit: float
    idle: int
    level: np.ndarray           # int8 level of each run
    units: np.ndarray           # int16 run length in bit times (clipped)
    start: np.ndarray           # int64 start sample of each run

    def nbytes(self) -> int:
        return _nb(self.level.astype(np.int8), self.units.astype(np.int16)) + 4 * len(self.start)


@dataclass
class RateBlocks:
    """An async line split into blocks of one bit time each (a layer of its
    own: everything after it is the same per-block processing)."""
    pin: str
    idle: int
    blocks: list[tuple[int, int, float]]   # (start_sample, end_sample, unit)

    def nbytes(self) -> int:
        return 20 * len(self.blocks)


@dataclass
class RateSegments:
    """Annotation for a clocked stream: where the clock rate changes. The
    stream itself is not split, because bits are taken at clock edges anyway."""
    clock: str
    segments: list[tuple[int, int, float]]  # (first edge index, last+1, median interval)


# ---------------- T3 / T4 outputs ----------------

@dataclass
class Frames:
    """Delimited units over a SyncBits stream."""
    source: SyncBits
    delimiter: str              # "select" / "startstop" / "gap"
    bounds: np.ndarray          # (n_frames, 2) [first, last+1) index into source.t
    events: list = field(default_factory=list)  # startstop: ("S"/"Sr"/"P", time)
    open_tail: bool = False     # startstop: the last frame has no STOP/Sr (the capture ended inside it)
    conds: list = field(default_factory=list)   # startstop: per frame (how it began "S"/"Sr", how it ended "P"/"Sr"/None)

    def closed(self) -> "Frames":
        """Without the open last frame (for consumers that only take whole frames)."""
        if not self.open_tail:
            return self
        return Frames(self.source, self.delimiter, self.bounds[:-1], self.events, conds=self.conds[:-1])

    def nbytes(self) -> int:
        # the bits inside frames (packed, all data pins) + the frame table + events
        inside = int(np.diff(self.bounds, axis=1).sum()) if len(self.bounds) else 0
        return int(np.ceil(inside * self.source.bits.shape[0] / 8)) + _nb(self.bounds.astype(np.int32)) + 9 * len(self.events)


@dataclass
class Words:
    frames: Frames | None
    size: int                   # bits per word (8, 9 for I2C byte+ack, ...)
    values: np.ndarray          # int64 word values (per data pin: row)
    frame_of: np.ndarray        # frame index of each word
    leftover_bits: np.ndarray   # bits per frame not forming a whole word

    def nbytes(self) -> int:
        per = 1 if self.size <= 8 else 2
        # values + words-per-frame (int16) + leftover bits per frame (int8)
        return int(self.values.size * per) + 3 * len(self.leftover_bits)


@dataclass
class Chars:
    """UART characters from AsyncSymbols."""
    pin: str
    bits: int                   # character length incl. start/stop
    values: np.ndarray          # int64 raw data+parity bits
    ok: np.ndarray              # bool stop/start ok
    start: np.ndarray           # int64
    breaks: np.ndarray = field(default_factory=lambda: np.zeros(0, np.int64))  # start of active runs >= 1 character

    def nbytes(self) -> int:
        return int(len(self.values) * 2) + 4 * len(self.start) + 4 * len(self.breaks)


# ---------------- builders ----------------

def sample_edge_for(cap, clock: str, data: tuple[str, ...], _debounced: bool = False) -> list[str]:
    """Sample on the clock edge farther from where the data changes. When the
    data changes about midway between both edges there is no evidence here, and
    both edges are returned as sibling candidates for a later stage to resolve:
    within 1.3 times, or a sample apart (the distances are whole samples, so a
    change midway in a half of 5 samples reads 2 from one edge and 3 from the
    other, either way round by the grid's phase)."""
    c = cap.channel(clock)
    lv = K.edge_levels(c.edges, c.initial)
    rise, fall = c.edges[lv == 1], c.edges[lv == 0]
    dr, df = [], []
    for d in data:
        de = cap.channel(d).edges
        if len(de) and len(rise) > 1 and len(fall):
            # where the clock runs too fast for its edges to be told apart (a half of a sample or two: a clock that
            # changes speed, taken down) a change is a sample from either: those left out where the slower stretches
            # hold enough (a clock fast throughout keeps them all)
            slow = K.local_period(rise, de) >= TELLS_EDGES * c.step
            if not slow.all() and slow.sum() >= max(8, 0.1 * len(de)):
                de = de[slow]
        if len(de) and len(rise) and len(fall):
            dr.append(np.median(K.nearest_distance(de, rise)[0]))
            df.append(np.median(K.nearest_distance(de, fall)[0]))
    a, b = float(np.mean(dr or [1])), float(np.mean(df or [0]))
    if min(a, b) > 0 and (max(a, b) / min(a, b) < 1.3 or abs(a - b) <= 1):
        if not _debounced:
            from dataclasses import replace
            # bounces at the clock's edges put a fall beside a rise, and the data's changes look near both: measured
            # again with each edge's bounces merged at its first change, where that leaves the clock's runs long
            k = V.BOUNCE_MERGE_WIDE * c.step
            ce = V.bounces_merged(np.asarray(c.edges, np.int64), k)
            if len(ce) != len(c.edges) and len(ce) > 16 and float(np.median(np.diff(ce))) >= 2 * k:
                view = replace(cap, channels=[replace(x, edges=V.bounces_merged(np.asarray(x.edges, np.int64), k))
                                              if x.name in (clock, *data) else x for x in cap.channels])
                got = sample_edge_for(view, clock, data, _debounced=True)
                if len(got) == 1:
                    return got
            # crosstalk's pulses of a sample (none of them an edge's bounce) do the same: measured again with those
            # runs taken out, each with its own two edges
            pins = (clock, *data)
            cleaned = {x.name: V.pulses_out(x.edges, 2 * x.step) for x in cap.channels if x.name in pins}
            # (not where the clock's runs of a sample are its own, its period kept to a sample: a clock of 5 samples a
            # period with lows of one or two, an I2C at 400 kHz at 2 MS/s, lost half its edges and read on the fall
            # alone, sigrok-dumps' ssd1306-i2c: the clock is taken as it is, its data lines cleaned)
            if V.own_short_runs(c.edges, c.step):
                cleaned[clock] = np.asarray(c.edges, np.int64)
            if any(len(cleaned[n]) != len(cap.channel(n).edges) for n in pins):
                view = replace(cap, channels=[replace(x, edges=cleaned[x.name]) if x.name in pins else x
                                              for x in cap.channels])
                return sample_edge_for(view, clock, data, _debounced=True)
        return ["rise", "fall"]
    return ["rise" if a >= b else "fall"]


TELLS_EDGES = 4          # a clock's period this many samples or more: a data change's distance to each edge tells them apart
ON_EDGE_SHARE = 0.1      # a data line's changes on the sampling edge's sample, this share: a driver clocked by it ...
ON_EDGE_OVER = 3          # ... and this many times its changes on the other edge's (another clock in step with it,
                          # drifting, meets both edges alike)


def sync_bits(cap, sv: Survey, clock: str, data: tuple[str, ...], select: str | None, edge: str | None = None) -> SyncBits:
    c = cap.channel(clock)
    edge = edge or sample_edge_for(cap, clock, data)[0]
    lv = K.edge_levels(c.edges, c.initial)
    t = c.edges[lv == (1 if edge == "rise" else 0)]
    t_other = c.edges[lv == (0 if edge == "rise" else 1)]
    # a data line ringing at the sampling edge (a reflection: pulses of a sample or two) is read as it settles, its
    # pulses shorter than RING samples dropped where the clock is slow enough that no bit is that short (the period
    # around each: a clock may change speed on the way)
    # A data change on the sampling edge's own sample: where such changes are a share of the line's (a target driving
    # on the edge it is clocked by does so every bit it drives: an STLink's SWD target its ACK and read data on
    # SWCLK's rise, 28% of SWDIO's changes), the edge's doing, and the value latched is the one before it (read at
    # the edge itself, 136 of 183 requests were lost); a few now and then (2%: a probe setting its data and clock in
    # one write) are set up for the edge and read after, and so is a line meeting the other edge as often (another
    # clock of the same rate, drifting, which read before the edges gave bits an SWD scan took for packets)
    rows = []
    for d in data:
        ch = cap.channel(d)
        if len(t) > 1 and ch.step == 1:
            e = V.spikes_out_where_slow(np.asarray(ch.edges), RING, t)
            at = K.in_sorted(e, t)
            inside = e[(e >= t[0]) & (e <= t[-1])]
            share = at.sum() / len(inside) if len(inside) else 0.0
            other = (K.in_sorted(inside, t_other).sum() / len(inside)) if len(inside) else 0.0
            side = "left" if share >= ON_EDGE_SHARE and other <= share / ON_EDGE_OVER else "right"
            rows.append(((ch.initial + np.searchsorted(e, t, side=side)) & 1).astype(np.uint8))
        else:
            rows.append(ch.level_at(t))
    bits = np.stack(rows).astype(np.uint8)
    return SyncBits(clock, data, select, edge, t, bits, K.burst_split(t))


def async_symbols(cap, sv: Survey, pin: str) -> AsyncSymbols | None:
    f = sv.features[pin]
    if not f.units or f.idle_level is None:
        return None
    u = f.units[0].samples
    start, length, level = cap.channel(pin).runs(cap.n_samples)
    units = np.clip(np.round(length / u), 0, 32767).astype(np.int16)
    return AsyncSymbols(pin, u, f.idle_level, level.astype(np.int8), units, start)


def select_level(start: np.ndarray, length: np.ndarray, level: np.ndarray, t: np.ndarray, n: int,
                 shortest: float = 0.0):
    """The select line's active level and its windows: (active, inside, k, windows). Decided from how the line frames
    the clock, not from its own idle estimate (a sparse line with long selections looks "idle" at its active level).
    A level's windows frame the clock when they hold it, each one (a selection is a transfer) and tightly (from just
    before its first clock to just after its last): the share of its whole windows (not cut by the capture's ends)
    holding clock edges, times how much of those windows the clocked span fills. Holding more of the clock alone is
    not it: with two targets on one bus, the other's transfers fall in this select's idle level, often more of them.
    Windows shorter than `shortest` (a spike on the line: no selection) count for neither."""
    best = None
    for active in (0, 1):
        act = level == active
        win = np.stack([start[act], start[act] + length[act]], 1)
        inside, k = K.in_windows(win, t)
        whole = (win[:, 0] > 0) & (win[:, 1] < n) & (win[:, 1] - win[:, 0] >= shortest)
        held = np.zeros(len(win), bool)
        lo = np.full(len(win), np.iinfo(np.int64).max)
        hi = np.full(len(win), -1, np.int64)
        if inside.any():
            ki, ti = k[inside], np.asarray(t, np.int64)[inside]
            held[ki] = True
            np.minimum.at(lo, ki, ti)
            np.maximum.at(hi, ki, ti)
        hw = held & whole
        share = float(hw.sum() / whole.sum()) if whole.any() else float(held.any())
        span = (hi - lo)[hw].sum() / max(1, (win[hw, 1] - win[hw, 0]).sum()) if hw.any() else 0.0
        score = (share * float(span), int(inside.sum()))
        if best is None or score > best[0]:
            best = (score, active, inside, k, win)
    return best[1:]


def frames_select(cap, sv: Survey, sb: SyncBits) -> Frames:
    s = cap.channel(sb.select)
    ci = sv.clocks.get(sb.clock)
    if ci is not None and ci.period:
        # read with its changes closer than a quarter of the bus clock's period merged: a spike or a crosstalk
        # pulse on the select no longer ends a frame mid-word (the line itself is not changed); a pulse of a single
        # sample at least, where the clock is too fast for a quarter of it to reach two
        from dataclasses import replace
        s = replace(s, edges=V.gated(s.edges, max(2.0, ci.period / V.GATE)))
    start, length, level = s.runs(cap.n_samples)
    inside, k, win = select_level(start, length, level, sb.t, cap.n_samples)[1:]
    bounds = []
    if inside.any():
        ks = k[inside]
        idx = np.flatnonzero(inside)
        cut = np.flatnonzero(np.diff(ks)) + 1
        for seg in np.split(idx, cut):
            # a window around a single sampling edge is no transfer: a select toggling with the clock (sigrok-dumps'
            # AT25SF041 at its end: 16 such "frames" took its length fit to 0.4)
            if seg[-1] + 1 - seg[0] >= SELECT_MIN_BITS:
                bounds.append((seg[0], seg[-1] + 1))
    return Frames(sb, "select", np.asarray(bounds, dtype=np.int64).reshape(-1, 2))


SELECT_MIN_BITS = 2     # bits a select's window holds at least for a frame


def frames_gap(sb: SyncBits, ratio: float | None = None) -> Frames:
    """Frames at idle gaps of the sampling clock. `ratio`: an interval longer
    than ratio x both neighbours is a gap (default: the burst split of SyncBits)."""
    starts = sb.burst_start if ratio is None else K.burst_split(sb.t, ratio)
    b = np.concatenate((starts, [len(sb.t)]))
    fr = Frames(sb, "gap", np.stack([b[:-1], b[1:]], 1))
    fr.ratio = ratio or 3.0
    return fr


def frames_startstop(cap, sb: SyncBits) -> Frames:
    """START: data falls while clock high; STOP: data rises while clock high.
    Uses the first data pin (the I2C SDA / RVSWD DIO candidate)."""
    c, d = cap.channel(sb.clock), cap.channel(sb.data[0])
    de = d.edges
    hi = c.level_at(de) == 1
    after = K.edge_levels(de, d.initial)
    ev_t = de[hi]
    ev_k = np.where(after[hi] == 0, 1, 2)  # 1 START-like, 2 STOP-like
    pos = np.searchsorted(sb.t, ev_t)
    # a frame is open after a START-like event and closed by the next event of either kind (the state is the kind of
    # the event before: no walk needed). A START-like event inside an open frame is a repeated START (Sr)
    start = ev_k == 1
    was_open = np.concatenate(([False], start[:-1]))
    names = np.where(start, np.where(was_open, "Sr", "S"), "P")
    events = list(zip(names.tolist(), ev_t.tolist()))
    close = np.flatnonzero(was_open)
    bounds = list(zip(pos[close - 1].tolist(), pos[close].tolist()))
    conds = list(zip(names[close - 1].tolist(), np.where(start[close], "Sr", "P").tolist()))
    # a START never closed (a bus left stuck, a window too short) is kept: where
    # the traffic stopped is often what a fault test asks for
    open_tail = bool(len(start)) and bool(start[-1]) and int(pos[-1]) < len(sb.t)
    if open_tail:
        bounds.append((int(pos[-1]), len(sb.t)))
        conds.append((str(names[-1]), None))
    return Frames(sb, "startstop", np.asarray(bounds, dtype=np.int64).reshape(-1, 2), events, open_tail, conds)


def words(fr: Frames, size: int, row: int = 0, drop_tail: int = 0) -> Words:
    """Group each frame's bits into words of `size` bits (MSB first).
    drop_tail: bits at the end of each frame that are not data (e.g. none)."""
    bits = fr.source.bits[row]
    bounds = np.asarray(fr.bounds, np.int64).reshape(-1, 2)
    n = (bounds[:, 1] - bounds[:, 0] - drop_tail) // size
    if (n >= 0).all():
        # every frame's words at once: the word starts, then their bits read together
        off = np.concatenate(([0], np.cumsum(n)))
        start = np.repeat(bounds[:, 0], n) + (np.arange(off[-1]) - np.repeat(off[:-1], n)) * size
        seg = bits[start[:, None] + np.arange(size)] if len(start) else np.zeros((0, size), np.uint8)
        w = (seg.astype(np.int64) << np.arange(size - 1, -1, -1)).sum(1) if len(start) else np.zeros(0, np.int64)
        return Words(fr, size, w.astype(np.int64), np.repeat(np.arange(len(n), dtype=np.int64), n),
                     (bounds[:, 1] - bounds[:, 0] - n * size).astype(np.int64))
    vals, fof, left = [], [], []
    for i, (a, b) in enumerate(fr.bounds.tolist()):
        n = (b - a - drop_tail) // size
        seg = bits[a:a + n * size].reshape(n, size) if n > 0 else np.zeros((0, size), np.uint8)
        w = (seg.astype(np.int64) << np.arange(size - 1, -1, -1)).sum(1) if n else np.zeros(0, np.int64)
        vals.append(w)
        fof.append(np.full(n, i, np.int64))
        left.append(b - a - n * size)
    cat = lambda xs: np.concatenate(xs) if xs else np.zeros(0, np.int64)  # noqa: E731
    return Words(fr, size, cat(vals), cat(fof), np.asarray(left, np.int64))


def chars(cap, sym: AsyncSymbols, L: int) -> Chars:
    """UART characters from the symbol stream: a character starts at a run that
    leaves the idle level; bits are read from the quantized runs."""
    # A run at the active level longer than a whole character is a break
    # (LIN, DMX512, "send break"): record it and treat it as idle for framing.
    level = sym.level.copy()
    brk = (level != sym.idle) & (sym.units >= L)
    level[brk] = sym.idle
    # expand runs into a bit string (bounded) and walk it
    lv = np.repeat(level, np.minimum(sym.units, L + 2).astype(np.int64))
    if sym.idle == 0:
        lv = 1 - lv
    t_bit = np.repeat(sym.start, np.minimum(sym.units, L + 2).astype(np.int64))
    # the walk only finds where each character starts; the bits of all of them are read at once
    at = _char_starts(lv, L, _first_start(lv, L))
    fr = lv[at[:, None] + np.arange(L)]
    ok = (fr[:, 0] == 0) & (fr[:, L - 1] == 1)
    vals = (fr[:, 1:L - 1].astype(np.int64) << np.arange(L - 2)).sum(1)
    return Chars(sym.pin, L, vals, ok, t_bit[at].astype(np.int64), sym.start[brk].astype(np.int64))


START_PREFIX = 20000      # bits of a line with no idle as long as a character over which its first start is chosen


def _first_start(lv: np.ndarray, L: int) -> int:
    """Where the walk over a bit string begins. A capture may start inside a character, and among back-to-back
    characters the first 0 is then a data bit: the walk from it stays a bit off until an idle as long as a character
    (in ASCII the eighth bit is always 0 and the stop 1, so the shifted frames hold), and a GPS's NMEA was so read and
    confirmed. Each of the first changes to active within two characters is tried, up to the first such idle (where
    every walk is outside a character and starts again at the next 0), the most characters framed kept; at a tie,
    the first (as the walk always began)."""
    zeros = np.flatnonzero(lv == 0)
    if not len(zeros):
        return 0
    first = int(zeros[0])
    falls = np.flatnonzero((lv[1:] == 0) & (lv[:-1] == 1)) + 1
    cands = [first] + [int(f) for f in falls if first < f < first + 2 * L]
    if len(cands) == 1:
        return first
    # the first idle run of L bits or more after the first 0: every walk is past its characters there
    d = np.diff(np.concatenate([[0], (lv[first:] == 1).astype(np.int8), [0]]))
    up, down = np.flatnonzero(d == 1), np.flatnonzero(d == -1)
    long_idle = np.flatnonzero(down - up >= L)
    end = first + int(down[long_idle[0]]) if len(long_idle) else len(lv)
    end = min(end, first + START_PREFIX)
    head = lv[:end]
    best, best_ok = first, -1
    for c in cands:
        at = _char_starts(head, L, c)
        n_ok = int(((head[at] == 0) & (head[at + L - 1] == 1)).sum()) if len(at) else 0
        if n_ok > best_ok:
            best, best_ok = c, n_ok
    return best


def _char_starts(lv: np.ndarray, L: int, i0: int = 0) -> np.ndarray:
    """Where each character starts in a bit string (0: active): a character is the L bits from a 0, and the next
    one is looked for after it."""
    at = []
    i, n = i0, len(lv)
    while i < n:
        if lv[i] == 0 and i + L <= n:
            at.append(i)
            i += L
        else:
            i += 1
    return np.asarray(at, np.int64)


# ---------------- protocol on words ----------------

SCL_WAIT = 4          # an SCL low longer than this many times the transaction's median low is a wait


def _scl_waits(scl, t: np.ndarray, a: int, b: int, factor: float = SCL_WAIT) -> tuple[float | None, list[dict]]:
    """(median SCL low, the lows longer than factor times it) inside a frame: each {after_word (the last word ended
    before it; -1 inside the first), bit (where in the next word it falls; 0: between words), ticks, s}. Whether the
    target held SCL (a stretch) or the controller paused is not told by the lines, so it is not named."""
    if b - a < 2:
        return None, []
    e = scl.edges                                         # sorted: the frame's edges as a slice, not a mask
    falls = e[np.searchsorted(e, t[a], side="right"):np.searchsorted(e, t[b - 1], side="left")]
    falls = falls[scl.level_at(falls) == 0]
    if not len(falls):
        return None, []
    rises = scl.edges[np.minimum(np.searchsorted(scl.edges, falls, side="right"), len(scl.edges) - 1)]
    lows = rises - falls
    med = float(np.median(lows))
    waits = []
    for s, n in zip(falls[lows > factor * med].tolist(), lows[lows > factor * med].tolist()):
        k = int(np.searchsorted(t[a:b], s))             # sampling edges of the frame before it
        waits.append({"after_word": k // 9 - 1, "bit": k % 9, "ticks": int(n), "s": int(s)})
    return med, waits


def i2c_from_words(fr: Frames, w: Words, scl=None) -> list[dict]:
    """I2C transactions from 9-bit words (8 data MSB first + ACK) per START..STOP/Sr frame.
    Frames here exclude the extra clock pulse before STOP (bits % 9 == 1 tail).
    An open last frame (no STOP) gives complete=False, with the bits clocked after
    its last whole byte in pending_bits. start_cond / end_cond: how it began ("S" / "Sr") and ended ("P" / "Sr" /
    None: still open at the window's end). With the SCL channel, scl_low_ticks (the median SCL low) and scl_waits
    (_scl_waits)."""
    out = []
    fof = w.frame_of
    cut = np.searchsorted(fof, np.arange(len(fr.bounds) + 1)) if np.all(np.diff(fof) >= 0) else None
    for i in range(len(fr.bounds)):
        v = w.values[cut[i]:cut[i + 1]] if cut is not None else w.values[fof == i]
        a, b = fr.bounds[i]
        t = fr.source.t
        last_open = fr.open_tail and i == len(fr.bounds) - 1
        if len(v) == 0:
            if last_open:  # stopped inside the address byte
                out.append({"addr": None, "rw": None, "addr_ack": False, "bytes": [], "acks": [], "start": int(t[a]),
                            "end": int(t[b - 1]), "complete": False, "pending_bits": [int(x) for x in fr.source.bits[0][a:b]]})
                _conds(out[-1], fr, i, scl, t, a, b)
            continue
        data = [int(x >> 1) for x in v]
        acks = [int(x & 1) == 0 for x in v]
        out.append({"addr": data[0] >> 1, "rw": "read" if data[0] & 1 else "write", "addr_ack": acks[0],
                    "bytes": data[1:], "acks": acks[1:], "start": int(t[a]), "end": int(t[max(a, b - 1)])})
        _conds(out[-1], fr, i, scl, t, a, b)
        if last_open:
            n = int(w.leftover_bits[i])
            out[-1]["complete"] = False
            out[-1]["pending_bits"] = [int(x) for x in fr.source.bits[0][b - n:b]] if n else []
    return out


def _conds(tx: dict, fr: Frames, i: int, scl, t: np.ndarray, a: int, b: int) -> None:
    if i < len(fr.conds):
        tx["start_cond"], tx["end_cond"] = fr.conds[i]
    if scl is not None:
        tx["scl_low_ticks"], tx["scl_waits"] = _scl_waits(scl, t, a, b)


def _unfold(runs: np.ndarray, unit: float) -> float:
    """The bit time, not a fraction of it: short runs that are no bits (a floating line's noise, glitches) make
    half the bit time fit as well, and every real run is then an even multiple of it. While the runs of at least
    a bit at that unit are nearly all even multiples, the unit doubles."""
    while True:
        r = runs[runs >= 0.75 * unit] / unit
        k = np.round(r)
        real = r[(np.abs(r - k) < 0.25) & (k >= 1)]
        if len(real) < 12 or np.mean(np.round(real) % 2 == 0) < 0.9:
            return unit
        unit *= 2


def async_segments(cap, sv: Survey, pin: str, half: int = 10, change: float = 1.5) -> list[tuple[int, int, float]]:
    """Split an async line where its bit time changes (e.g. a bootloader that
    switches baud rate mid-stream). The local bit time is the shortest run among
    the neighbouring runs (a character almost always contains a 1-bit run);
    a jump by `change` or more between neighbouring runs is a switch point,
    placed in the longest idle run near it. Each segment's unit is then
    estimated from all of its runs. Returns [(start_sample, end_sample, unit)]."""
    from numpy.lib.stride_tricks import sliding_window_view
    from .features import estimate_units
    ch = cap.channel(pin)
    start, length, level = ch.runs(cap.n_samples)
    inner = length[1:-1].astype(np.float64)
    f = sv.features[pin]
    if len(inner) < 4 * half:
        return [(0, cap.n_samples, f.units[0].samples)] if f.units else []
    x = np.where(inner > ch.step, inner, np.inf)  # 1-sample spikes are not bits
    w = 2 * half + 1
    loc = np.min(sliding_window_view(np.pad(x, (half, half), mode="edge"), w), axis=1)
    # windows with only 1-sample spikes have no bit-time estimate: carry the neighbour
    loc = np.where(np.isfinite(loc), loc, np.nan)
    if np.isnan(loc).all():
        return [(0, cap.n_samples, f.units[0].samples)] if f.units else []
    idx = np.where(~np.isnan(loc), np.arange(len(loc)), 0)
    np.maximum.accumulate(idx, out=idx)
    loc = loc[idx]
    loc = np.where(np.isnan(loc), np.nanmedian(loc), loc)
    lr = np.log(loc)
    # switch where the median of the next window differs from the previous one
    # medians of the next / previous w values, vectorized
    med_next = np.median(sliding_window_view(np.pad(lr, (0, w - 1), mode="edge"), w), axis=1)
    med_prev = np.concatenate((np.full(w, lr[0]), med_next[:-w])) if len(lr) > w else np.full(len(lr), lr[0])
    jump = np.abs(med_next - med_prev) >= np.log(change)
    cuts = []
    cand = np.flatnonzero(jump[w:len(lr) - w]) + w
    ci = 0
    i = w
    while ci < len(cand):
        i = int(cand[ci])
        if True:
            j = i + int(np.argmax(np.abs(med_next[i:i + w] - med_prev[i:i + w])))
            # the longest idle run around j is the boundary
            lo, hi = max(1, j - half), min(len(inner), j + half)
            # (`cand` is taken over by the idle runs here, so the walk below goes on in them and ends near the first
            # switches found: in effect the first switch only. Walking every jump instead splits a bouncing line, a
            # decoy, or a DMX512 / LIN line at its breaks in tens of blocks each read at a rate of its own - tried,
            # false UART claims and DMX512 / LIN lost; kept as it is)
            cand = [k for k in range(lo, hi) if level[k + 1] == f.idle_level]
            k = max(cand, key=lambda k: inner[k]) if cand else j
            cuts.append(int(start[k + 1] + length[k + 1] // 2))
            nxt = j + w
            ci = int(np.searchsorted(cand, nxt))
    def unit_of(s0, s1):
        sel = (start >= s0) & (start < s1)
        # active-level runs are whole bits; idle-level ones include inter-character gaps
        act = sel & (level != f.idle_level)
        act[0] = act[-1] = False
        runs = length[act] if act.sum() >= 12 else (length[sel][1:-1] if sel.sum() > 2 else length[sel])
        cands = estimate_units(runs, top=8)
        if not cands:
            return None
        # A character almost always has a 1-bit run, so the unit is about the
        # shortest run; short segments otherwise fall for integer fractions.
        rr = runs[runs > 1]
        shortest = float(np.quantile(rr, 0.02)) if len(rr) else cands[0].samples
        best = max(c.fit for c in cands)
        ok = [c for c in cands if c.fit >= best - 0.05 and 0.7 * shortest <= c.samples <= 1.15 * shortest]
        return _unfold(runs, max(ok, key=lambda c: c.samples).samples if ok else cands[0].samples)

    bounds = [0, *cuts, cap.n_samples]
    # refine every cut: among idle runs around it, pick the one that best splits
    # runs into "fits the unit before" and "fits the unit after"
    for ci in range(1, len(bounds) - 1):
        ua, ub = unit_of(bounds[ci - 1], bounds[ci]), unit_of(bounds[ci], bounds[ci + 1])
        if not ua or not ub:
            continue
        sel = (start >= bounds[ci - 1]) & (start < bounds[ci + 1])
        idx = np.flatnonzero(sel)
        r = length[idx].astype(np.float64)
        # Runs of the slower side are integer multiples of the faster unit too, so
        # "fits" cannot place the boundary. What the slower side never has is a
        # run shorter than its own bit: minimise the runs that are too short for
        # the unit of the side they would fall on (1-sample spikes ignored).
        sa = (r < 0.8 * ua) & (r > ch.step)
        sb = (r < 0.8 * ub) & (r > ch.step)
        ca = np.concatenate(([0], np.cumsum(sa)))              # too short for A before j
        cb = np.concatenate((np.cumsum(sb[::-1])[::-1], [0]))  # too short for B from j on
        bad = ca[:-1] + cb[:-1]
        j = _fewest_bad(bad, length[idx], level[idx] == f.idle_level, np.abs(start[idx] - bounds[ci]))
        if j >= 0:
            bounds[ci] = int(start[idx[j]] + length[idx[j]] // 2)
    # a stretch of too few runs for a rate of its own (under two windows: a break and a character or two before the
    # switch the refinement placed) goes with the next one
    merged = [bounds[0]]
    for x, y in zip(bounds[1:-1], bounds[2:]):
        if np.count_nonzero((start >= merged[-1]) & (start < x)) >= 2 * w and np.count_nonzero((start >= x) & (start < y)) >= 2 * w:
            merged.append(x)
    bounds = merged + [bounds[-1]]
    out = []
    for s0, s1 in zip(bounds[:-1], bounds[1:]):
        u = unit_of(s0, s1)
        if u:
            # neighbours whose bit times differ by less than a switch are one stretch (a spike made the jump; split,
            # each half held too few breaks for a DMX512 line to read as one): read again as one
            if out and max(u, out[-1][2]) / min(u, out[-1][2]) < change and out[-1][1] == s0:
                a0 = out[-1][0]
                out[-1] = (a0, s1, unit_of(a0, s1) or out[-1][2])
            else:
                out.append((s0, s1, u))
    return out


def _fewest_bad(bad: np.ndarray, length: np.ndarray, idle: np.ndarray, away: np.ndarray | None = None) -> int:
    """The idle run with the fewest bad runs around it; of those the nearest the cut found (`away`: each run's distance
    from it), then the longest, the first; -1: no idle run. (The slower side's runs are whole numbers of the faster
    unit, so often many places tie at none bad: the longest of them was the capture's leading idle, and the switch
    went missing.)"""
    if not idle.any():
        return -1
    cand = idle & (bad == bad[idle].min())
    if away is not None:
        cand &= away <= away[cand].min() + length[cand].max()
    return int(np.flatnonzero(cand & (length == length[cand].max()))[0])


def async_symbols_segment(cap, sv: Survey, pin: str, s0: int, s1: int, unit: float,
                          idle: int | None = None) -> AsyncSymbols:
    start, length, level = cap.channel(pin).runs(cap.n_samples)
    sel = (start >= s0) & (start < s1)
    units = np.clip(np.round(length[sel] / unit), 0, 32767).astype(np.int16)
    idle = sv.features[pin].idle_level if idle is None else idle
    return AsyncSymbols(pin, unit, idle, level[sel].astype(np.int8), units, start[sel])


def rate_blocks(cap, sv: Survey, pin: str, idle: int | None = None) -> RateBlocks:
    """The line's rate segments under its idle level (the survey's, or the one given)."""
    return RateBlocks(pin, sv.features[pin].idle_level if idle is None else idle, async_segments(cap, sv, pin))


def clock_rate_segments(sb: SyncBits, change: float = 1.5) -> RateSegments:
    """Annotate where the sampling-edge interval changes by `change` or more
    between bursts (inside a burst the rate is taken as constant)."""
    t = sb.t
    b = np.concatenate((sb.burst_start, [len(t)]))
    segs = []
    for a, z in zip(b[:-1], b[1:]):
        if z - a < 2:
            continue
        med = float(np.median(np.diff(t[a:z])))
        if segs and max(med, segs[-1][2]) / min(med, segs[-1][2]) < change:
            segs[-1] = (segs[-1][0], int(z), segs[-1][2])
        else:
            segs.append((int(a), int(z), med))
    return RateSegments(sb.clock, segs)


def block_chars(cap, sv: Survey, rb: RateBlocks) -> list[tuple[tuple[int, int, float], list, "Chars | None"]]:
    """Common per-block processing: estimate the character length inside each
    block, then build characters. The smallest length among the tied best is
    used (a 2-stop-bit line decodes correctly with 1 stop bit)."""
    from .survey import char_length
    out = []
    for blk in rb.blocks:
        s0, s1, u = blk
        cands = char_length(cap, rb.pin, rb.idle, u, s0, s1)
        if not cands:
            out.append((blk, [], None))
            continue
        best = cands[0][1]
        L = min(l for l, r in cands if r >= best - 0.02)
        ch = chars(cap, async_symbols_segment(cap, sv, rb.pin, s0, s1, u, rb.idle), L)
        u2 = refit_unit(np.asarray(cap.channel(rb.pin).edges, np.int64), ch, u)
        if u2 and abs(u2 / u - 1) > REFIT_MIN:
            # the bit time fitted over the characters read: read again with it, kept when it reads as many
            ch2 = chars(cap, async_symbols_segment(cap, sv, rb.pin, s0, s1, u2, rb.idle), L)
            if int(ch2.ok.sum()) >= int(ch.ok.sum()):
                blk, ch = (s0, s1, u2), ch2
        out.append((blk, cands, ch))
    return out


REFIT_MIN = 0.002      # a refitted bit time this close to the estimate reads the same: not read again


def refit_unit(edges: np.ndarray, ch: "Chars", u: float, rounds: int = 2) -> float | None:
    """A UART's bit time fitted over its well-formed characters: each edge inside one is the character's start plus a
    whole number of bits; edges more than 0.3 bit off that grid (a spike, a bounce) are left out; least squares over
    all characters (each its own offset). The estimate from run lengths (features.estimate_units) is pulled off by
    runs a spike or a bounce cut in two; the edges' places in the characters are not."""
    starts = np.sort(ch.start[ch.ok]).astype(np.int64)
    if len(starts) < 2 or not len(edges):
        return None
    L = ch.bits
    lo = np.searchsorted(edges, starts[0])
    e = edges[lo:]
    i = np.searchsorted(starts, e, side="right") - 1          # the character each edge may be in
    for _ in range(rounds):
        d = (e - starts[i]).astype(np.float64)
        inside = d < (L - 0.5) * u
        k = np.round(d / u)
        keep = inside & (np.abs(d / u - k) < 0.3)
        ci, d, k = i[keep], d[keep], k[keep]
        m = len(starts)
        n = np.bincount(ci, minlength=m).astype(np.float64)
        sk, sd = np.bincount(ci, k, m), np.bincount(ci, d, m)
        skd, skk = np.bincount(ci, k * d, m), np.bincount(ci, k * k, m)
        use = n >= 2
        if not use.any():
            return None
        # per character centred: sum (k - mean k)(d - mean d) = skd - sk sd / n, and the same for k k
        num = float((skd[use] - sk[use] * sd[use] / n[use]).sum())
        den = float((skk[use] - sk[use] ** 2 / n[use]).sum())
        if den <= 0:
            return None
        u = num / den
    return u if u > 0 else None


@dataclass
class PulseSymbols:
    """One pin as a sequence of active-level pulses classified short/long,
    split into frames at pauses. Thresholds come from the data: the width
    split is between the two width clusters, a pause is a pulse-to-pulse
    interval over 3x the typical one. Plugins map short/long to 1/0 themselves."""
    pin: str
    idle: int
    start: np.ndarray        # int64 pulse start
    width: np.ndarray        # int64 pulse width (samples)
    short: np.ndarray        # bool width below the split
    split: float
    bounds: np.ndarray       # (n_frames, 2) index range into start

    def nbytes(self) -> int:
        return int(np.ceil(len(self.short) / 8)) + 4 * len(self.bounds)


def _int_bits(bits) -> int:
    v = 0
    for x in bits:
        v = (v << 1) | int(x)
    return v


PULSE_CLUSTERS = 8     # the most populated width clusters tried in pairs as a pulse code's short and long


def pulse_symbols(cap, sv: Survey, pin: str) -> PulseSymbols | None:
    ch = cap.channel(pin)
    idle = sv.features[pin].idle_level
    lv = K.edge_levels(ch.edges, ch.initial)
    lead = ch.edges[lv != idle]          # pulse starts (leave idle)
    trail = ch.edges[lv == idle]         # pulse ends
    if len(lead) < 8:
        return None
    j = np.searchsorted(trail, lead, side="right")
    ok = j < len(trail)
    lead, width = lead[ok], trail[j[ok]] - lead[ok]
    # width clusters (log scale): SWIO's short and long pulses are two of them; other traffic on the pin (a debugger
    # trying the 2-wire form first, its long probing pulses) makes others, and in a short operation may outnumber
    # SWIO's own and move both the split and the pulse pitch frames are cut by
    lw = np.log(np.maximum(width, 1).astype(np.float64))
    bins = np.arange(lw.min(), lw.max() + 0.1, 0.08)
    if len(bins) < 3:
        return None
    raw, edges_ = np.histogram(lw, bins)
    centers = (edges_[:-1] + edges_[1:]) / 2
    # a cluster is a peak of the counts summed with the neighbouring bins: at a few samples a pulse a width falls in
    # two bins (12 and 13 samples are a bin apart), and either alone may count less than other traffic's clusters
    hist = raw + np.concatenate(([0], raw[:-1])) + np.concatenate((raw[1:], [0]))
    peak = (hist >= np.concatenate(([0], hist[:-1]))) & (hist > np.concatenate((hist[1:], [0])))
    order = []
    for i in np.flatnonzero(peak)[np.argsort(hist[peak])[::-1]].tolist():
        # peaks nearer than the pairs' least ratio are one cluster's (a broad one makes two)
        if all(abs(centers[i] - centers[k]) >= np.log(1.6) for k in order):
            order.append(i)
    order = np.asarray(order, np.int64)
    first = order[0]
    second = next((i for i in order[1:] if abs(centers[i] - centers[first]) >= np.log(1.6) and hist[i] >= 0.05 * hist[first]), None)
    if second is None:
        return None
    iv = np.diff(lead).astype(np.float64)

    def cut_by(split: float, near=None):
        """Frames where the pitch between pulses is over 3 times the usual (of the pulses near the two clusters)."""
        sel = iv if near is None else iv[near[:-1] & near[1:]]
        typical = float(np.median(sel)) if len(sel) else (float(np.median(iv)) if len(iv) else 1.0)
        cut = np.flatnonzero(iv > 3 * typical) + 1
        return np.concatenate(([0], cut, [len(lead)]))

    from .riscv_dm import NAMES

    def framed(split: float, b) -> int:
        """Pulses in frames that read as SWIO's: 41 opening with a short pulse (a 1) whose address is a Debug Module
        register, or 33 not all ones (a data0 read going on); a run of short pulses alone is no frame."""
        got = 0
        short = width < split
        for a, z in zip(b[:-1].tolist(), b[1:].tolist()):
            n = z - a
            if n not in (41, 33):
                continue
            f = short[a:z]
            if f.all():
                continue
            if n == 41 and (not f[0] or _int_bits(f[1:8]) not in NAMES):
                continue
            got += n
        return got

    split = float(np.exp((centers[first] + centers[second]) / 2))
    b = cut_by(split)
    best = (framed(split, b), split, b)
    # other pairs of the most populated clusters, each with the pitch of its own pulses: the one that frames the most
    # pulses in SWIO's lengths (the default stays when none frames more)
    top = [i for i in order[:PULSE_CLUSTERS] if hist[i] > 0]
    for x in range(len(top)):
        for y in range(x + 1, len(top)):
            i, j = top[x], top[y]
            if abs(centers[i] - centers[j]) < np.log(1.6):
                continue
            sp = float(np.exp((centers[i] + centers[j]) / 2))
            near = (np.abs(lw - centers[i]) < np.log(1.6)) | (np.abs(lw - centers[j]) < np.log(1.6))
            bb = cut_by(sp, near)
            got = framed(sp, bb)
            if got > best[0]:
                best = (got, sp, bb)
    _, split, b = best
    return PulseSymbols(pin, idle, lead, width, width < split, split, np.stack([b[:-1], b[1:]], 1))


@dataclass
class NrzBits:
    """An async line as bits: each run of one level becomes round(length/unit)
    bits (the receiver resynchronizes on every edge, as CAN does). Frames start
    after an idle run of `idle_bits` or more; idle runs are kept only up to
    `idle_bits + 2` bits so a frame may read a few idle bits at its end."""
    pin: str
    idle: int
    units: list[float]          # bit time per rate block
    bits: np.ndarray            # int8
    t: np.ndarray               # int64 sample of each bit
    bounds: np.ndarray          # (n_frames, 2) [first, next_start)
    rate: float                 # sample rate

    def bitrate(self) -> float:
        return self.rate / float(np.median(self.units))

    def nbytes(self) -> int:
        return int(np.ceil(len(self.bits) / 8)) + 4 * len(self.bounds)


def nrz_bits(cap, sv: Survey, rb: RateBlocks, idle_bits: int = 10) -> NrzBits | None:
    start, length, level = cap.channel(rb.pin).runs(cap.n_samples)
    bits, ts, bounds = [], [], []
    n = 0
    for s0, s1, u in rb.blocks:
        sel = (start >= s0) & (start < s1)
        st, ln, lv = start[sel], length[sel], level[sel]
        k = np.maximum(1, np.round(ln / u)).astype(np.int64)
        is_idle = lv == rb.idle
        long_idle = is_idle & (k >= idle_bits)
        k = np.where(long_idle, np.minimum(k, idle_bits + 2), k)
        first = np.cumsum(np.concatenate(([0], k[:-1]))) + n
        # a frame starts at the first bit after a long idle run
        starts = first[1:][long_idle[:-1]]
        b = np.repeat(lv.astype(np.int8), k)
        off = np.arange(len(b)) - np.repeat(first - n, k)
        tt = np.repeat(st, k) + (off * u).astype(np.int64)
        bits.append(b)
        ts.append(tt)
        end = n + len(b)
        bounds += list(zip(starts.tolist(), starts[1:].tolist() + [end]))
        n = end
    if not bounds:
        return None
    return NrzBits(rb.pin, rb.idle, [u for _, _, u in rb.blocks], np.concatenate(bits), np.concatenate(ts),
                   np.array(bounds, dtype=np.int64), cap.rate)
