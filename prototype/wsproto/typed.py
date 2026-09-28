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
from .survey import Survey


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

    def nbytes(self) -> int:
        return int(len(self.values) * 2) + 4 * len(self.start)


# ---------------- builders ----------------

def sample_edge_for(cap, clock: str, data: tuple[str, ...]) -> list[str]:
    """Sample on the clock edge farther from where the data changes. When the
    data changes about midway between both edges there is no evidence here, and
    both edges are returned as sibling candidates for a later stage to resolve."""
    c = cap.channel(clock)
    lv = K.edge_levels(c.edges, c.initial)
    rise, fall = c.edges[lv == 1], c.edges[lv == 0]
    dr, df = [], []
    for d in data:
        de = cap.channel(d).edges
        if len(de) and len(rise) and len(fall):
            dr.append(np.median(K.nearest_distance(de, rise)[0]))
            df.append(np.median(K.nearest_distance(de, fall)[0]))
    a, b = float(np.mean(dr or [1])), float(np.mean(df or [0]))
    if min(a, b) > 0 and max(a, b) / min(a, b) < 1.3:
        return ["rise", "fall"]
    return ["rise" if a >= b else "fall"]


def sync_bits(cap, sv: Survey, clock: str, data: tuple[str, ...], select: str | None, edge: str | None = None) -> SyncBits:
    c = cap.channel(clock)
    edge = edge or sample_edge_for(cap, clock, data)[0]
    lv = K.edge_levels(c.edges, c.initial)
    t = c.edges[lv == (1 if edge == "rise" else 0)]
    bits = np.stack([cap.channel(d).level_at(t) for d in data]).astype(np.uint8)
    return SyncBits(clock, data, select, edge, t, bits, K.burst_split(t))


def async_symbols(cap, sv: Survey, pin: str) -> AsyncSymbols | None:
    f = sv.features[pin]
    if not f.units or f.idle_level is None:
        return None
    u = f.units[0].samples
    start, length, level = cap.channel(pin).runs(cap.n_samples)
    units = np.clip(np.round(length / u), 0, 32767).astype(np.int16)
    return AsyncSymbols(pin, u, f.idle_level, level.astype(np.int8), units, start)


def frames_select(cap, sv: Survey, sb: SyncBits) -> Frames:
    s = cap.channel(sb.select)
    start, length, level = s.runs(cap.n_samples)
    # The active level is the one whose windows hold the clock, decided from the
    # pin relation rather than from the select line's own idle estimate (a
    # sparse line with long selections looks "idle" at its active level).
    best = None
    for active in (0, 1):
        act = level == active
        win = np.stack([start[act], start[act] + length[act]], 1)
        inside, k = K.in_windows(win, sb.t)
        if best is None or inside.sum() > best[0].sum():
            best = (inside, k, win)
    inside, k, win = best
    bounds = []
    if inside.any():
        ks = k[inside]
        idx = np.flatnonzero(inside)
        cut = np.flatnonzero(np.diff(ks)) + 1
        for seg in np.split(idx, cut):
            bounds.append((seg[0], seg[-1] + 1))
    return Frames(sb, "select", np.asarray(bounds, dtype=np.int64).reshape(-1, 2))


def frames_gap(sb: SyncBits) -> Frames:
    b = np.concatenate((sb.burst_start, [len(sb.t)]))
    return Frames(sb, "gap", np.stack([b[:-1], b[1:]], 1))


def frames_startstop(cap, sb: SyncBits) -> Frames:
    """START: data falls while clock high; STOP: data rises while clock high.
    Uses the first data pin (the I2C SDA / RVSWD DIO candidate)."""
    c, d = cap.channel(sb.clock), cap.channel(sb.data[0])
    de = d.edges
    hi = c.level_at(de) == 1
    after = K.edge_levels(de, d.initial)
    ev_t = de[hi]
    ev_k = np.where(after[hi] == 0, 1, 2)  # 1 START-like, 2 STOP-like
    events, bounds = [], []
    open_at = None
    pos = np.searchsorted(sb.t, ev_t)
    for tt, kind, p in zip(ev_t.tolist(), ev_k.tolist(), pos.tolist()):
        if kind == 1:
            if open_at is not None:
                bounds.append((open_at, p))
                events.append(("Sr", tt))
            else:
                events.append(("S", tt))
            open_at = p
        else:
            if open_at is not None:
                bounds.append((open_at, p))
            events.append(("P", tt))
            open_at = None
    return Frames(sb, "startstop", np.asarray(bounds, dtype=np.int64).reshape(-1, 2), events)


def words(fr: Frames, size: int, row: int = 0, drop_tail: int = 0) -> Words:
    """Group each frame's bits into words of `size` bits (MSB first).
    drop_tail: bits at the end of each frame that are not data (e.g. none)."""
    bits = fr.source.bits[row]
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
    # expand runs into a bit string (bounded) and walk it
    lv = np.repeat(sym.level, np.minimum(sym.units, L + 2).astype(np.int64))
    if sym.idle == 0:
        lv = 1 - lv
    t_bit = np.repeat(sym.start, np.minimum(sym.units, L + 2).astype(np.int64))
    vals, ok, st = [], [], []
    i, n = 0, len(lv)
    while i < n:
        if lv[i] == 0 and i + L <= n:
            fr = lv[i:i + L]
            v = int((fr[1:L - 1].astype(np.int64) << np.arange(L - 2)).sum())
            vals.append(v)
            ok.append(bool(fr[0] == 0 and fr[L - 1] == 1))
            st.append(int(t_bit[i]))
            i += L
        else:
            i += 1
    return Chars(sym.pin, L, np.asarray(vals, np.int64), np.asarray(ok, bool), np.asarray(st, np.int64))


# ---------------- protocol on words ----------------

def i2c_from_words(fr: Frames, w: Words) -> list[dict]:
    """I2C transactions from 9-bit words (8 data MSB first + ACK) per START..STOP/Sr frame.
    Frames here exclude the extra clock pulse before STOP (bits % 9 == 1 tail)."""
    out = []
    for i in range(len(fr.bounds)):
        v = w.values[w.frame_of == i]
        if len(v) == 0:
            continue
        data = [int(x >> 1) for x in v]
        acks = [int(x & 1) == 0 for x in v]
        out.append({"addr": data[0] >> 1, "rw": "read" if data[0] & 1 else "write", "addr_ack": acks[0],
                    "bytes": data[1:]})
    return out


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
    x = np.where(inner > 1, inner, np.inf)  # 1-sample spikes are not bits
    w = 2 * half + 1
    loc = np.min(sliding_window_view(np.pad(x, (half, half), mode="edge"), w), axis=1)
    lr = np.log(loc)
    # switch where the median of the next window differs from the previous one
    med_prev = np.array([np.median(lr[max(0, i - w):i]) if i > 0 else lr[0] for i in range(len(lr))])
    med_next = np.array([np.median(lr[i:i + w]) for i in range(len(lr))])
    jump = np.abs(med_next - med_prev) >= np.log(change)
    cuts = []
    i = w
    while i < len(lr) - w:
        if jump[i]:
            j = i + int(np.argmax(np.abs(med_next[i:i + w] - med_prev[i:i + w])))
            # the longest idle run around j is the boundary
            lo, hi = max(1, j - half), min(len(inner), j + half)
            cand = [k for k in range(lo, hi) if level[k + 1] == f.idle_level]
            k = max(cand, key=lambda k: inner[k]) if cand else j
            cuts.append(int(start[k + 1] + length[k + 1] // 2))
            i = j + w
        else:
            i += 1
    def unit_of(s0, s1):
        sel = (start >= s0) & (start < s1)
        runs = length[sel][1:-1] if sel.sum() > 2 else length[sel]
        cands = estimate_units(runs, top=8)
        if not cands:
            return None
        # A character almost always has a 1-bit run, so the unit is about the
        # shortest run; short segments otherwise fall for integer fractions.
        rr = runs[runs > 1]
        shortest = float(np.quantile(rr, 0.02)) if len(rr) else cands[0].samples
        best = max(c.fit for c in cands)
        ok = [c for c in cands if c.fit >= best - 0.05 and 0.7 * shortest <= c.samples <= 1.15 * shortest]
        return max(ok, key=lambda c: c.samples).samples if ok else cands[0].samples

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
        sa = (r < 0.8 * ua) & (r > 1)
        sb = (r < 0.8 * ub) & (r > 1)
        ca = np.concatenate(([0], np.cumsum(sa)))              # too short for A before j
        cb = np.concatenate((np.cumsum(sb[::-1])[::-1], [0]))  # too short for B from j on
        bad = ca[:-1] + cb[:-1]
        cand = [j for j in range(len(idx)) if level[idx[j]] == f.idle_level]
        if cand:
            j = min(cand, key=lambda j: (bad[j], -length[idx[j]]))
            bounds[ci] = int(start[idx[j]] + length[idx[j]] // 2)
    out = []
    for s0, s1 in zip(bounds[:-1], bounds[1:]):
        u = unit_of(s0, s1)
        if u:
            out.append((s0, s1, u))
    return out


def async_symbols_segment(cap, sv: Survey, pin: str, s0: int, s1: int, unit: float) -> AsyncSymbols:
    start, length, level = cap.channel(pin).runs(cap.n_samples)
    sel = (start >= s0) & (start < s1)
    units = np.clip(np.round(length[sel] / unit), 0, 32767).astype(np.int16)
    return AsyncSymbols(pin, unit, sv.features[pin].idle_level, level[sel].astype(np.int8), units, start[sel])


def rate_blocks(cap, sv: Survey, pin: str) -> RateBlocks:
    return RateBlocks(pin, sv.features[pin].idle_level, async_segments(cap, sv, pin))


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
        out.append((blk, cands, chars(cap, async_symbols_segment(cap, sv, rb.pin, s0, s1, u), L)))
    return out
