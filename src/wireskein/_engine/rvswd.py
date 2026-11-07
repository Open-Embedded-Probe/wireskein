"""WCH's debug transports, RVSWD (two wires) and SWIO (one wire): each carries RISC-V DMI accesses, which
riscv_dm.py reads as the RISC-V Debug Module (the same above both).

RVSWD frames are declared in decl/rvswd.toml; this module gives the framing the declaration names
(frame.reframe = "rvswd_stop"): from START to STOP, or at stop intervals for captures that hide them.

The short form (53 clocks, from wch-protocols captures/tools/rvswd.py): addr7, R/W (1 = write), header parity,
park, ctl4, data32, data parity, park, status2, pad2, stop. A 54-clock frame is a read with one extra turnaround
clock (data at bits 15..46). The long form (85 clocks) is in decl/rvswd.toml.
"""

from __future__ import annotations

import warnings

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


def reframe_startstop(fr, cap, hold: float = 1.5, w: int = 16, hold_min_s: float = 1.5e-6,
                      far: float = 4.0, far_min_s: float = 1e-6) -> list[tuple[int, int]]:
    """Frames from START to STOP, as WCH's RVSWD delimits them: START is DIO falling while CLK is high outside a
    frame; STOP is DIO rising while CLK is high, CLK then staying high longer than `hold` times its usual high (the
    median of the `w` highs before it) and at least `hold_min_s`. Inside a frame the target may move DIO while CLK is
    high (a read's turnaround), a fast LinkE holds CLK high about 3 to 4 times its usual high at a short frame's
    turnaround (0.5 us at 3 MHz) and now and then about twice inside a burst read (1.1 us): none is a STOP. A STOP
    holds CLK high 1.8 us or more (a fast LinkE, under the 2 us link-to-target names), 4 to 7 us at slower speeds;
    at its fastest (CLK high 30 ns) a LinkE's STOP may hold only 1.4 us, far over its usual high: so a high `far`
    times the usual and at least `far_min_s` is a STOP too.

    A reflection ringing DIO for a sample or two (a 1-sample rise inside a CLK high held at a turnaround) is no
    START or STOP: where the clock is slow enough (9 samples a period or more around it) DIO's pulses under 3 samples
    are left out first, as the bits are read (typed.RING); where it is fast (a LinkE reading at its high speed after
    connecting slowly) they are bits."""
    sb = fr.source
    c, d = cap.channel(sb.clock), cap.channel(sb.data[0])
    ce, de = np.asarray(c.edges), np.asarray(d.edges)
    if len(ce) < 4 or len(de) < 2:
        return []
    clv = K.edge_levels(ce, c.initial)
    rises = ce[clv == 1]
    from .typed import RING
    if len(rises) > 1 and d.step == 1:
        de = K.deglitch_slow(de, RING, rises)
        if len(de) < 2:
            return []
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
    if not len(falls_hi):
        return []                                           # no START: no frame
    k = np.searchsorted(rises, up, side="right") - 1        # the high DIO rose in
    up, k = up[k >= 0], k[k >= 0]
    if not len(up):
        return []
    idx = k[:, None] - np.arange(w, 0, -1)[None, :]         # the w highs before it
    prev = np.where(idx >= 0, highs[np.clip(idx, 0, None)], np.nan)
    with warnings.catch_warnings(), np.errstate(all="ignore"):
        warnings.simplefilter("ignore", RuntimeWarning)       # a STOP with no high before it: no usual high
        usual = np.nanmedian(np.where(np.isfinite(prev), prev, np.nan), axis=1)
    u = np.nan_to_num(usual, nan=np.inf)
    long_ = ((highs[k] > hold * u) & (highs[k] >= hold_min_s * cap.rate)) | \
            ((highs[k] >= far * u) & (highs[k] >= far_min_s * cap.rate))
    stop = up[~np.isfinite(highs[k]) | long_]
    # each STOP closes the frame opened by the first START after the STOP before it
    after_prev = np.concatenate(([-1], stop[:-1]))
    j = np.searchsorted(falls_hi, after_prev, side="right")
    has = j < len(falls_hi)
    st = np.where(has, falls_hi[np.minimum(j, len(falls_hi) - 1)], 0)
    keep = has & (st < stop)
    a = np.searchsorted(sb.t, st[keep], side="right")
    b = np.searchsorted(sb.t, stop[keep], side="right")
    out = [(int(x), int(y)) for x, y in zip(a.tolist(), b.tolist()) if y > x]
    if sb.sample_edge == "rise" and len(rises) == len(sb.t) and np.array_equal(rises, sb.t):
        out = [_repaired(x, y, highs) for x, y in out]
        out = _restored(out, sb.t, d.initial, de)
    return out


def known_length(n: int) -> bool:
    """A length a START / STOP frame of RVSWD has: short 53, long 85, the LinkE's query 80, a burst 38n+15."""
    return n in (53, 85, 80) or (n >= 91 and (n - 15) % 38 == 0)


def _repaired(a: int, b: int, highs: np.ndarray, glitch: int = 1, most: int = 3):
    """A frame of a length no form has, with as many clocks too many as it has rising edges whose high lasted
    `glitch` samples or less (a reflection's glitch on the clock, read as a clock): those edges dropped, (a, b,
    dropped indices); else (a, b). The checks of the layout then tell whether the frame reads."""
    n = b - a
    if known_length(n):
        return (a, b)
    short = np.flatnonzero(highs[a:b] <= glitch) + a
    for m in range(1, most + 1):
        if known_length(n - m) and len(short) >= m:
            drop = short[np.argsort(highs[short], kind="stable")[:m]]
            return (a, b, tuple(sorted(int(i) for i in drop)))
    return (a, b)


RESTORE_MARGIN = [1.5, 4.0]   # the places about as likely as the best: within this of its cost (times, + median intervals)


def _restored(out: list, t: np.ndarray, initial: int, de: np.ndarray, lengths=(53, 85), least: int = 4,
              near: int = 16) -> list:
    """Frames a clock short of a fixed form (52 or 84 clocks between START and STOP) whose clock lost a high between
    two samples (a LinkE at its fastest, its highs a sample or two at 50 MS/s): the lost clock restored where the
    frame's clock intervals best match the form's, one interval there as long as two (the form's intervals: the
    median, position by position, of the `near` whole frames of that length nearest it, at least `least` of them). Its
    bit is DIO at the tick it is estimated at (the interval before it, as the form has it, past the clock before).
    -> the frames, a restored one (a, b, (), ((at, bit, tick),)); the layout's checks then tell whether it reads."""
    out = list(out)
    for n in lengths:
        whole = [i for i, x in enumerate(out) if len(x) == 2 and x[1] - x[0] == n]
        short = [i for i, x in enumerate(out) if len(x) == 2 and x[1] - x[0] == n - 1]
        if len(whole) < least or not short:
            continue
        starts = np.asarray([out[i][0] for i in whole], np.int64)
        iv_all = np.diff(t[starts[:, None] + np.arange(n)[None, :]], axis=1).astype(np.float64)   # (frames, n - 1)
        for i in short:
            a, b = out[i]
            k = np.searchsorted(starts, a)
            lo = max(0, min(k - near // 2, len(starts) - near))
            T = np.median(iv_all[lo:lo + near], axis=0)                 # n - 1 intervals
            iv = np.diff(t[a:b]).astype(np.float64)                     # n - 2
            # interval j of the frame is T[j] + T[j + 1]: the ones before it are T[:j], the ones after T[j + 2:]
            pre = np.concatenate(([0.0], np.cumsum(np.abs(iv - T[:-1]))))          # pre[j]: |iv[:j] - T[:j]|
            post = np.concatenate((np.cumsum(np.abs(iv - T[1:])[::-1])[::-1], [0.0]))   # post[j]: |iv[j:] - T[j+1:]|
            j = np.arange(n - 2)
            cost = pre[j] + np.abs(iv - (T[:-1] + T[1:])) + post[j + 1]
            lv = ((initial + np.searchsorted(de, t[a:b], side="right")) & 1).astype(np.int64)
            # the places that match the form nearly as well as the best, tried best first: the first whose bits
            # pass the form's checks (where the clock was lost is not always plain: intervals alike around it)
            best = float(cost.min())
            cands = []
            for jb in np.argsort(cost, kind="stable").tolist():
                if cost[jb] > RESTORE_MARGIN[0] * best + RESTORE_MARGIN[1] * float(np.median(T)):
                    break
                tick = int(t[a + jb] + round(T[jb]))
                bit = int((initial + np.searchsorted(de, tick, side="right")) & 1)
                cands.append((jb, bit, tick, np.insert(lv, jb + 1, bit)))
            # restored only when every place about as likely gives the same bits, and they pass the checks
            if cands and all(np.array_equal(c[3], cands[0][3]) for c in cands) and _form_checks(cands[0][3], n):
                jb, bit, tick, _ = cands[0]
                out[i] = (a, b, (), ((jb + 1, bit, tick),))
    return out


def _form_checks(f: np.ndarray, n: int) -> bool:
    """A frame's bits pass its form's checks (rvswd.toml): short, its two parities; long, its echoes."""
    if n == 53:
        return bool(f[8] == (f[0:8].sum() & 1) and f[46] == (f[14:46].sum() & 1))
    if n == 85:
        return bool(np.array_equal(f[0:7], f[42:49]) and f[40] == f[83])
    return False


LONG_TARGET = 42               # the first clock of the target's phase in the long form


def _parity(bits: str) -> int:
    return bits.count("1") & 1


def frame_fields(bits: str, form: str, op: str | None = None) -> dict:
    """The fields of a frame by name, as bit strings (MSB first), its checks one by one.

    short (53): addr7 rw1 header_parity1 park1 ctl4 data32 data_parity1 park1 status2 pad2 stop1;
    a burst (15 + 38n): header14 (addr7 ...), then words of data32 aux6, the last word's aux6 and a stop the trailer7;
    long (85): host addr7 data32 op2 bit41, target addr7 data32 status2, bit83, a last clock."""
    if form == "short" and len(bits) == 53:
        f = {"addr": bits[0:7], "rw": bits[7], "header_parity": bits[8], "park": bits[9], "ctl": bits[10:14],
             "data": bits[14:46], "data_parity": bits[46], "park2": bits[47], "status": bits[48:50],
             "pad": bits[50:52], "stop": bits[52]}
        f["header_parity_ok"] = _parity(bits[0:8]) == int(bits[8])
        f["data_parity_ok"] = _parity(bits[14:46]) == int(bits[46])
        return f
    if op == "BURST" and len(bits) >= 91 and (len(bits) - 15) % 38 == 0:
        n = (len(bits) - 15) // 38
        return {"header": bits[0:14], "aux": [bits[46 + 38 * i:52 + 38 * i] for i in range(n - 1)],
                "trailer": bits[14 + 38 * n - 6:]}
    if form == "long" and len(bits) == 85:
        return {"host_addr": bits[0:7], "host_data": bits[7:39], "op": bits[39:41], "bit41": bits[41],
                "target_addr": bits[42:49], "target_data": bits[49:81], "status": bits[81:83], "bit83": bits[83],
                "last": bits[84]}
    return {}


def long_answer(cap, sb, a: int, b: int) -> dict:
    """When the target answered a long frame (rising edges [a, b) of sb), from what it should send: the host's address
    echoed (and, after a write, the host's data). At each bit where that changes, the change of DIO is found after
    the rising edge before that bit; its delay, in that clock period (0 to 1: in time for the bit's rising edge; over
    1: after it, the bit sampled late). answer_delay: the median and max over those changes, in periods;
    answer_delay_ns the same in ns; late: 1 when the median is over 1 (the answer slid a clock, as a CH32V103 does at
    times while it is reset: its echoed address then reads as another)."""
    bits = sb.bits[0][a:b].astype(np.int64)
    if len(bits) < 83:
        return {}
    host_addr, host_data, op = bits[0:7], bits[7:39], int(bits[39]) * 2 + int(bits[40])
    want = np.concatenate((host_addr, host_data)) if op == 2 else host_addr
    c, d = cap.channel(sb.clock), cap.channel(sb.data[0])
    ce = np.asarray(c.edges)
    rises = ce[K.edge_levels(ce, c.initial) == 1]
    k0 = int(np.searchsorted(rises, sb.t[a]))
    R = rises[k0:k0 + (b - a) + 1]
    if len(R) < LONG_TARGET + len(want) + 1:
        return {}
    de = np.asarray(d.edges)
    dlv = K.edge_levels(de, d.initial)
    delays, ns = [], []
    for i in range(1, len(want)):
        if want[i] == want[i - 1]:
            continue
        p = LONG_TARGET + i                    # the bit that changes; its rising edge R[p], the one before R[p - 1]
        r0, r1 = R[p - 1], R[p]
        j = int(np.searchsorted(de, r0, side="right"))
        while j < len(de) and dlv[j] != want[i]:
            j += 1
        if j >= len(de) or de[j] > R[min(p + 1, len(R) - 1)]:
            continue                           # not changed within two clocks: no answer to time
        delays.append((de[j] - r0) / (r1 - r0))
        ns.append((de[j] - r0) / cap.rate * 1e9)
    if not delays:
        return {}
    med = float(np.median(delays))
    out = {"answer_delay": {"median": round(med, 3), "max": round(float(max(delays)), 3)},
           "answer_delay_ns": {"median": round(float(np.median(ns)), 1), "max": round(float(max(ns)), 1)}}
    if med > 1:
        out["late"] = 1
    return out


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
        ns = 1e9 / c.cap.rate
        tx, lens = [], []

        def _pitches(a, b):
            """The space between pulses (idle level, the same whatever the bit), and the period from a pulse's start
            to the next one's after a 1 (short) and after a 0 (long): a pulse's period is its low and that space, so
            a 0's is longer."""
            if b - a < 2:
                return {}
            st, w, s = ps.start[a:b], ps.width[a:b], ps.short[a:b]
            d = np.diff(st)
            out = {"space_ns": round(float(np.median(d - w[:-1])) * ns, 1)}
            for key, m in (("pitch_1_ns", s[:-1]), ("pitch_0_ns", ~s[:-1])):
                out[key] = round(float(np.median(d[m])) * ns, 1) if m.any() else None
            return out

        def widths(a, b, rw=None):
            """How the frame's pulses sat against the split: the longest short one and the shortest long one (a
            margin), and the R/W bit's own (the bit a target misreads when its long low shrinks), in ns."""
            w, s = ps.width[a:b], ps.short[a:b]
            out = {"short_max_ns": round(float(w[s].max()) * ns, 1) if s.any() else None,
                   "long_min_ns": round(float(w[~s].min()) * ns, 1) if (~s).any() else None,
                   "short_median_ns": round(float(np.median(w[s])) * ns, 1) if s.any() else None,
                   "long_median_ns": round(float(np.median(w[~s])) * ns, 1) if (~s).any() else None,
                   **_pitches(a, b)}
            if rw is not None:
                out["rw_low_ns"] = round(float(ps.width[rw]) * ns, 1)
                if rw + 1 < b:                 # from the R/W bit's end to the first data pulse (the turnaround)
                    out["rw_gap_ns"] = round(float(ps.start[rw + 1] - ps.start[rw] - ps.width[rw]) * ns, 1)
            return out

        for a, b in ps.bounds.tolist():
            f = bits_all[a:b]
            lens.append(b - a)
            if b - a == 41 and f[0] == 1:
                tx.append({"t": int(ps.start[a]), "e": int(ps.start[b - 1] + ps.width[b - 1]), "op": "W" if f[8] else "R",
                           "addr": _int(f[1:8]), "data": _int(f[9:]), "status": 0, "ok": True, **widths(a, b, a + 8)})
            elif b - a == 33 and f[0] == 0 and tx and tx[-1]["op"] == "R" and tx[-1]["addr"] == 0x04:
                # a continuation of reading data0 (autoexec): only after a read of it, so a lone 33-pulse burst
                # (a LinkE's 32 pulses and one more before it connects) is no read
                tx.append({"t": int(ps.start[a]), "e": int(ps.start[b - 1] + ps.width[b - 1]), "op": "R", "addr": 0x04,
                           "data": _int(f[1:]), "status": 0, "ok": True, "fast": True, **widths(a, b)})
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
        return [Node("swio", {"idle": ps.idle, "split_ns": round(float(ps.split) * ns, 1)}, {"dio": ps.pin}, Stream("dmi", tx), m, layer_score=score, total=score)]


def frame_details(x: dict, sb, cap) -> dict:
    """A frame's details from its sampling edges (x["_span"]): its bits as sampled (the don't-care ones too), the
    clock's period, the glitch clocks left out, its fields by name and, for a long frame answered, the answer's
    timing. Made for the frames exported only (every hypothesis would make them otherwise)."""
    a, b, skip = x["_span"][:3]
    ins = x["_span"][3] if len(x["_span"]) > 3 else ()
    keep = np.setdiff1d(np.arange(a, b), np.asarray(skip, np.int64)) if skip else np.arange(a, b)
    bits, t = sb.bits[0][keep].astype(np.int64), sb.t[keep].astype(np.int64)
    for at, bit, tick in ins:
        bits, t = np.insert(bits, at, bit), np.insert(t, at, tick)
    out = {"bits": "".join("1" if v else "0" for v in bits.tolist())}
    if len(t) > 1:
        out["period_ns"] = round(float(np.median(np.diff(t))) / cap.rate * 1e9, 1)
    if skip:
        out["repaired"] = [int(sb.t[i]) for i in skip]              # the glitch clocks' ticks
    if ins:
        out["restored"] = [int(tick) for _, _, tick in ins]          # the lost clocks' ticks, as estimated
    fields = frame_fields(out["bits"], x.get("form", "short"), x.get("op"))
    if fields:
        out["fields"] = fields
    if x.get("form") == "long" and x.get("answered") is not False and not skip and not ins:
        out.update(long_answer(cap, sb, a, b))
    return out
