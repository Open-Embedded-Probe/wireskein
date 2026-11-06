"""WCH's debug transports, RVSWD (two wires) and SWIO (one wire): each carries RISC-V DMI accesses, which
riscv_dm.py reads as the RISC-V Debug Module (the same above both).

RVSWD frames are declared in decl/rvswd.toml; this module gives the framing the declaration names
(frame.reframe = "rvswd_stop"): from START to STOP, or at stop intervals for captures that hide them.

The short form (53 clocks, from wch-protocols captures/tools/rvswd.py): addr7, R/W (1 = write), header parity,
park, ctl4, data32, data parity, park, status2, pad2, stop. A 54-clock frame is a read with one extra turnaround
clock (data at bits 15..46). The long form (85 clocks) is in decl/rvswd.toml.
"""

from __future__ import annotations

import numpy as np

from . import kernels as K
from .riscv_dm import NAMES
from .stack import Node, Stream

def _int(bits) -> int:
    v = 0
    for b in bits:
        v = (v << 1) | int(b)
    return v


def reframe_rolling(fr, mult: float = 3.0, w: int = 33) -> list[tuple[int, int]]:
    """Alternative framing for fast transfers: a gap is an interval over `mult`
    times the moving median of the surrounding w intervals (frames contain
    turnaround intervals that are long compared with their direct neighbours)."""
    from numpy.lib.stride_tricks import sliding_window_view
    t = fr.source.t
    if len(t) < w + 2:
        return reframe(fr)
    iv = np.diff(t).astype(np.float64)
    med = np.median(sliding_window_view(np.pad(iv, (w // 2, w // 2), mode="edge"), w), axis=1)
    cut = np.flatnonzero(iv > mult * med) + 1
    b = np.concatenate(([0], cut, [len(t)]))
    out = []
    for x, y in zip(b[:-1].tolist(), b[1:].tolist()):
        if y - x >= 3 and t[y - 1] - t[y - 2] > 1.8 * np.median(np.diff(t[x:y])):
            y -= 1
        out.append((x, y))
    return out


def reframe_startstop(fr, cap, hold: float = 1.5, w: int = 16) -> list[tuple[int, int]]:
    """Frames from START to STOP, as WCH's RVSWD delimits them: START is DIO falling while CLK is high outside a
    frame; STOP is DIO rising while CLK is high, CLK then staying high longer than `hold` times its usual high
    (the median of the `w` highs before it). Inside a frame the target may move DIO while CLK is high (a read's
    turnaround): that is no STOP, since CLK goes low again within its usual time. The hold is measured against the
    clock itself, not a fixed time: a fast LinkE holds CLK high under 2 us after a STOP."""
    sb = fr.source
    c, d = cap.channel(sb.clock), cap.channel(sb.data[0])
    ce, de = np.asarray(c.edges), np.asarray(d.edges)
    if len(ce) < 4 or len(de) < 2:
        return []
    clv = K.edge_levels(ce, c.initial)
    rises = ce[clv == 1]
    falls = ce[clv == 0]
    # each high: from a rise to the next fall
    fi = np.searchsorted(falls, rises, side="right")
    ok = fi < len(falls)
    highs = np.full(len(rises), np.inf)
    highs[ok] = falls[fi[ok]] - rises[ok]
    hi_at = c.level_at(de) == 1
    dlv = K.edge_levels(de, d.initial)
    falls_hi = de[hi_at & (dlv == 0)]                       # START candidates
    up = de[hi_at & (dlv == 1)]
    k = np.searchsorted(rises, up, side="right") - 1        # the high DIO rose in
    up, k = up[k >= 0], k[k >= 0]
    if not len(up):
        return []
    idx = k[:, None] - np.arange(w, 0, -1)[None, :]         # the w highs before it
    prev = np.where(idx >= 0, highs[np.clip(idx, 0, None)], np.nan)
    with np.errstate(all="ignore"):
        usual = np.nanmedian(np.where(np.isfinite(prev), prev, np.nan), axis=1)
    stop = up[~np.isfinite(highs[k]) | (highs[k] > hold * np.nan_to_num(usual, nan=np.inf))]
    # each STOP closes the frame opened by the first START after the STOP before it
    after_prev = np.concatenate(([-1], stop[:-1]))
    j = np.searchsorted(falls_hi, after_prev, side="right")
    has = j < len(falls_hi)
    st = np.where(has, falls_hi[np.minimum(j, len(falls_hi) - 1)], 0)
    keep = has & (st < stop)
    a = np.searchsorted(sb.t, st[keep], side="right")
    b = np.searchsorted(sb.t, stop[keep], side="right")
    return [(int(x), int(y)) for x, y in zip(a.tolist(), b.tolist()) if y > x]


def reframe(fr) -> list[tuple[int, int]]:
    """RVSWD knowledge: a frame ends with a long clock interval (about 2.7x the
    period) before a final stop clock, and frames may follow each other with a
    gap only ~3x the period. Split at intervals > 2.5x the frame's median and
    drop a trailing stop clock that follows an interval > 1.8x the median."""
    t = fr.source.t
    out = []
    for a, b in fr.bounds.tolist():
        if b - a < 8:
            out.append((a, b))
            continue
        iv = np.diff(t[a:b]).astype(np.float64)
        med = float(np.median(iv))
        cuts = [a, *(a + 1 + np.flatnonzero(iv > 2.5 * med)).tolist(), b]
        for x, y in zip(cuts[:-1], cuts[1:]):
            if y - x >= 3 and t[y - 1] - t[y - 2] > 1.8 * med:
                y -= 1
            if y > x:
                out.append((x, y))
    return out


class SwioPlugin:
    """WCH SWIO (1-wire SDI) on pulse symbols: short pulse = 1, long = 0.
    41 pulses: start(1) addr7 R/W(1 = write) data32; 33 pulses: 0 + data32
    (fast-read continuation, data0). Carries the same DMI transactions as RVSWD,
    so the same Debug Module plugin sits on top. SWIO has no parity: the
    evidence is the frame structure and the DM register addresses."""
    name = "swio"
    consumes = ("pulses",)

    def run(self, c, ps, g):
        bits_all = ps.short.astype(np.int8)
        tx, lens = [], []
        for a, b in ps.bounds.tolist():
            f = bits_all[a:b]
            lens.append(b - a)
            if b - a == 41 and f[0] == 1:
                tx.append({"t": int(ps.start[a]), "op": "W" if f[8] else "R", "addr": _int(f[1:8]), "data": _int(f[9:]),
                           "status": 0, "ok": True})
            elif b - a == 33 and f[0] == 0:
                tx.append({"t": int(ps.start[a]), "op": "R", "addr": 0x04, "data": _int(f[1:]), "status": 0, "ok": True,
                           "fast": True})
        if len(tx) < 2:
            return []
        lens = np.asarray(lens)
        structured = float(np.isin(lens, (41, 33)).mean())
        # share of pulses inside 41/33 frames: other traffic on the pin (e.g. a
        # debugger probing in the 2-wire form first) must not dilute the evidence
        pulses = float(lens[np.isin(lens, (41, 33))].sum() / max(1, lens.sum()))
        known = float(np.mean([x["addr"] in NAMES for x in tx]))
        m = {"frames": int(len(lens)), "structured": structured, "pulse_share": pulses, "dmi": len(tx),
             "dm_addr": known, "split_ns": ps.split / c.cap.rate * 1e9}
        score = known * (0.5 + 0.5 * pulses) * (1 - 0.5 ** (len(tx) / 4))
        return [Node("swio", {"idle": ps.idle}, {"dio": ps.pin}, Stream("dmi", tx), m, layer_score=score, total=score)]
