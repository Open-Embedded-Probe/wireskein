"""Times between edges of the lines of a bus or of a group of outputs: the phase of two PWMs, the dead time of a
complementary pair, the skew of two patterns, I2C's per-bit times (UM10204), the SPI mode from the lines.

A Channel's edge at tick e changed somewhere in (e - step, e], so the time between edges of two channels is known
to res = the larger of their steps (in ticks). A limit is met when the whole interval [v - res, v + res] is on its
side, broken when the whole interval is on the other side, and undecided otherwise (`judge`).
"""

from __future__ import annotations

import numpy as np

from .model import Capture, Channel


def res_of(*chs: Channel) -> int:
    return max(int(c.step) for c in chs)


def judge(v: float, res: float, lo: float | None = None, hi: float | None = None) -> bool | None:
    """True when every value in [v - res, v + res] is in [lo, hi], False when none is, None otherwise."""
    a, b = v - res, v + res
    if (lo is not None and b < lo) or (hi is not None and a > hi):
        return False
    if (lo is not None and a < lo) or (hi is not None and b > hi):
        return None
    return True


def states(chs: list[Channel], n: int) -> tuple[np.ndarray, np.ndarray]:
    """(starts, levels): the lines' levels held from starts[k] to starts[k + 1] (the last to n), levels[k, i] for
    chs[i]. Changes on the same tick make one state."""
    t = np.unique(np.concatenate([np.asarray(c.edges, np.int64) for c in chs] + [np.zeros(0, np.int64)]))
    t = t[(t > 0) & (t < n)]
    starts = np.concatenate(([0], t)).astype(np.int64)
    return starts, np.stack([c.level_at(starts) for c in chs], 1).astype(np.int8)


def _dirs(ch: Channel, edge: str) -> np.ndarray:
    e = np.asarray(ch.edges, np.int64)
    rises = e[0::2] if ch.initial == 0 else e[1::2]
    falls = e[1::2] if ch.initial == 0 else e[0::2]
    return {"rise": rises, "fall": falls}[edge]


def _stats(v: np.ndarray, rate: float) -> dict:
    if len(v) == 0:
        return {"count": 0}
    return {"count": int(len(v)), "min_s": float(v.min()) / rate, "max_s": float(v.max()) / rate,
            "median_s": float(np.median(v)) / rate}


# ---------------- two lines ----------------

def phase(a: Channel, b: Channel, rate: float, edge: str = "rise") -> dict:
    """b's edges after a's of the same direction, as a fraction of a's period (degrees, 0 to 360). Each pair is a's
    edge and b's first edge at or after it, within one period of a."""
    ea, eb = _dirs(a, edge), _dirs(b, edge)
    if len(ea) < 3 or len(eb) < 2:
        return {"pairs": 0}
    per_a = float(np.median(np.diff(ea)))
    per_b = float(np.median(np.diff(eb)))
    j = np.searchsorted(eb, ea, side="left")
    ok = j < len(eb)
    d = eb[j[ok]] - ea[ok]
    keep = d < per_a
    d = d[keep].astype(np.float64)
    if len(d) == 0:
        return {"pairs": 0, "freq_hz": rate / per_a, "freq_b_hz": rate / per_b}
    deg = d / per_a * 360.0
    res = res_of(a, b)
    return {"pairs": int(len(d)), "freq_hz": rate / per_a, "freq_b_hz": rate / per_b,
            "phase_deg": float(np.median(deg)), "min_deg": float(deg.min()), "max_deg": float(deg.max()),
            "delay_s": float(np.median(d)) / rate, "resolution_deg": res / per_a * 360.0, "deg": deg}


def circular_diff(x: float, y: float) -> float:
    """x - y in degrees, folded into [-180, 180)."""
    return (x - y + 180.0) % 360.0 - 180.0


def skew(a: Channel, b: Channel, rate: float, edge: str = "both") -> dict:
    """Each edge of a with b's nearest edge of the same direction (b - a, ticks); edge "rise" / "fall" pairs only
    those. An edge without one nearer than half the closest spacing of a's edges of that direction is unpaired (b
    moved less often, or not with a)."""
    out = {"delays": np.zeros(0, np.int64), "unpaired_a": 0, "unpaired_b": 0}
    for edge in (("rise", "fall") if edge == "both" else (edge,)):
        ea, eb = _dirs(a, edge), _dirs(b, edge)
        if len(ea) == 0 or len(eb) == 0:
            out["unpaired_a"] += len(ea)
            out["unpaired_b"] += len(eb)
            continue
        gap = float(np.min(np.diff(ea))) / 2 if len(ea) > 1 else float("inf")
        j = np.clip(np.searchsorted(eb, ea), 1, len(eb)) - 1
        k = np.minimum(j + 1, len(eb) - 1)
        near = np.where(np.abs(eb[k] - ea) < np.abs(eb[j] - ea), k, j)
        d = eb[near] - ea
        ok = np.abs(d) < gap
        # one edge of b answers one edge of a
        ok &= np.concatenate(([True], near[1:] != near[:-1]))
        out["delays"] = np.concatenate((out["delays"], d[ok]))
        out["unpaired_a"] += int(np.count_nonzero(~ok))
        out["unpaired_b"] += len(eb) - int(np.count_nonzero(ok))
    return out


def dead_time(hi: Channel, lo: Channel, n: int, active: int = 1) -> dict:
    """The two outputs of a half bridge (both active at `active`). Dead time: from one turning off to the other
    turning on, with neither on between (0 when both change on one tick). Overlap: both on (shoot-through).
    A pause with the same output on at both sides is not a dead time. Ticks."""
    starts, lv = states([hi, lo], n)
    on = lv == active
    code = on[:, 0] * 1 + on[:, 1] * 2          # 0 none, 1 hi, 2 lo, 3 both
    ends = np.concatenate((starts[1:], [n]))
    hi_lo, lo_hi, overlaps = [], [], []
    for k in range(len(code) - 1):
        c, d = code[k], code[k + 1]
        if c == 3 and 0 < k:
            overlaps.append((int(starts[k]), int(ends[k] - starts[k])))
        if (c, d) == (1, 2):
            hi_lo.append((int(starts[k + 1]), 0))
        elif (c, d) == (2, 1):
            lo_hi.append((int(starts[k + 1]), 0))
        elif c == 0 and 0 < k:
            p = code[k - 1]
            if (p, d) == (1, 2):
                hi_lo.append((int(starts[k]), int(ends[k] - starts[k])))
            elif (p, d) == (2, 1):
                lo_hi.append((int(starts[k]), int(ends[k] - starts[k])))
    return {"hi_to_lo": hi_lo, "lo_to_hi": lo_hi, "overlaps": overlaps}


# ---------------- I2C ----------------

# UM10204 (rev. 7) table 10, seconds: (min, max); None where the mode has no limit
I2C_MODES = {
    "standard": {"f_scl": (None, 100e3), "t_hd_sta": (4.0e-6, None), "t_low": (4.7e-6, None),
                 "t_high": (4.0e-6, None), "t_su_sta": (4.7e-6, None), "t_hd_dat": (0.0, None),
                 "t_su_dat": (250e-9, None), "t_su_sto": (4.0e-6, None), "t_buf": (4.7e-6, None),
                 "t_vd_dat": (None, 3.45e-6)},
    "fast": {"f_scl": (None, 400e3), "t_hd_sta": (0.6e-6, None), "t_low": (1.3e-6, None),
             "t_high": (0.6e-6, None), "t_su_sta": (0.6e-6, None), "t_hd_dat": (0.0, None),
             "t_su_dat": (100e-9, None), "t_su_sto": (0.6e-6, None), "t_buf": (1.3e-6, None),
             "t_vd_dat": (None, 0.9e-6)},
    "fast-plus": {"f_scl": (None, 1e6), "t_hd_sta": (0.26e-6, None), "t_low": (0.5e-6, None),
                  "t_high": (0.26e-6, None), "t_su_sta": (0.26e-6, None), "t_hd_dat": (0.0, None),
                  "t_su_dat": (50e-9, None), "t_su_sto": (0.26e-6, None), "t_buf": (0.5e-6, None),
                  "t_vd_dat": (None, 0.45e-6)},
}
I2C_TIMES = ("t_low", "t_high", "t_hd_sta", "t_su_sta", "t_hd_dat", "t_su_dat", "t_vd_dat", "t_su_sto", "t_buf")


def i2c_times(scl: Channel, sda: Channel, n: int) -> dict:
    """Every instance of UM10204's times on the lines, as (tick, ticks) lists, and the SCL periods (rise to rise
    inside a transaction). An SDA change while SCL is low before or after its tick is data; only one with SCL high
    on both sides is a START (falling) or STOP (rising). t_su_sta is measured for repeated STARTs (a first START
    follows the bus being free, t_buf). t_vd_dat: from SCL falling to the last SDA change of that low, for a data
    bit: not the low before a high holding a START or STOP (SDA set up for the condition), nor one the window ends
    in."""
    starts, lv = states([scl, sda], n)
    out = {k: [] for k in I2C_TIMES}
    out["periods"] = []
    in_tx = False
    last_rise = last_fall = last_stop = start_at = None
    cond = False                        # a START or STOP since SCL last rose
    vd = None                           # this high's t_vd_dat, kept when it ends without a condition
    changes: list[int] = []             # SDA's data changes in this low
    for k in range(1, len(starts)):
        t = int(starts[k])
        s0, s1, d0, d1 = lv[k - 1, 0], lv[k, 0], lv[k - 1, 1], lv[k, 1]
        if d0 != d1:
            if s0 == 1 and s1 == 1:
                if d1 == 0:                                         # START
                    if in_tx and last_rise is not None:
                        out["t_su_sta"].append((last_rise, t - last_rise))
                    elif not in_tx and last_stop is not None:
                        out["t_buf"].append((last_stop, t - last_stop))
                    in_tx, start_at = True, t
                else:                                               # STOP
                    if in_tx and last_rise is not None:
                        out["t_su_sto"].append((last_rise, t - last_rise))
                    in_tx, last_stop, last_fall = False, t, None
                cond = True
            elif in_tx and last_fall is not None:
                if not changes:
                    out["t_hd_dat"].append((last_fall, t - last_fall))
                changes.append(t)
        if s0 != s1 and s1 == 1:                                    # SCL rises
            if in_tx and last_fall is not None:
                out["t_low"].append((last_fall, t - last_fall))
                if changes:
                    out["t_su_dat"].append((changes[-1], t - changes[-1]))
                    vd = (last_fall, changes[-1] - last_fall)
                if last_rise is not None and not cond:
                    out["periods"].append((last_rise, t - last_rise))
            last_rise, cond = t, False
        elif s0 != s1:                                              # SCL falls
            if vd is not None and not cond:
                out["t_vd_dat"].append(vd)
            vd = None
            if start_at is not None:
                out["t_hd_sta"].append((start_at, t - start_at))
            elif in_tx and last_rise is not None and not cond:
                out["t_high"].append((last_rise, t - last_rise))
            start_at, changes = None, []
            last_fall = t if in_tx else None
    return out


# ---------------- SPI ----------------

def sampling_margins(clk: Channel, data: list[Channel], edge: str, rate: float, cs: Channel | None = None,
                     cs_active: int = 0) -> dict[str, dict]:
    """Per data line, the shortest time (s) from its last change to a sampling edge of SCK (setup) and from that edge
    to its next change (hold), over the edges inside CS windows (all edges without CS). No expectation needed."""
    lv = clk.initial ^ ((np.arange(len(clk.edges)) + 1) & 1)
    t = clk.edges[lv == (1 if edge == "rise" else 0)]
    if cs is not None:
        t = t[cs.level_at(t) == cs_active]
    out = {}
    for d in data:
        de = d.edges
        if not len(t) or not len(de):
            out[d.name] = {"setup_s": None, "hold_s": None}
            continue
        j = np.searchsorted(de, t)                           # the changes at or after each edge
        before = (t - de[j - 1])[j > 0]
        after = de[j[j < len(de)]] - t[j < len(de)]
        out[d.name] = {"setup_s": float(before.min()) / rate if len(before) else None,
                       "hold_s": float(after.min()) / rate if len(after) else None}
    return out


def spi_read(clk: Channel, data: list[Channel], edge: str, n: int, cs: Channel | None = None, cs_active: int = 0,
             lsb: bool = False) -> dict[str, str]:
    """Each data line's bytes (hex), read just before each sampling edge of SCK, per window of CS active (all of
    the capture as one without CS); the bits left over in a window are dropped."""
    lv = clk.initial ^ ((np.arange(len(clk.edges)) + 1) & 1)
    t = clk.edges[lv == (1 if edge == "rise" else 0)]
    if cs is not None:
        keep = cs.level_at(t) == cs_active
        win = np.searchsorted(np.concatenate(([0], cs.edges, [n])), t, side="right")
    else:
        keep, win = np.ones(len(t), bool), np.zeros(len(t), np.int64)
    out = {}
    for d in data:
        data_bytes = []
        for w in np.unique(win[keep]):
            ts = t[keep & (win == w)]
            bits = d.level_at(np.maximum(ts - 1, 0)).astype(int)
            for k in range(len(bits) // 8):
                b = bits[8 * k:8 * k + 8]
                data_bytes.append(int("".join(map(str, b[::-1] if lsb else b)), 2))
        out[d.name] = bytes(data_bytes).hex()
    return out


def spi_modes(clk: Channel, data: list[Channel], cpol: int, rate: float, n: int, cs: Channel | None = None,
              cs_active: int = 0, lsb: bool = False, edge_sample: str | None = None) -> list[dict]:
    """The two modes of SCK's idle level, each with whether a receiver could read the lines in it (no data line
    changing at its sampling edges) and the bytes it reads: [{"mode", "readable", "bytes"}]. One readable mode tells
    the mode; readable ones reading the same bytes need not be told apart. edge_sample "before": a change in the same
    sample as a sampling edge is after it (a part whose output hold is shorter than a sample: the edge reads the
    value before), so a hold under a sample still reads."""
    hold_min = 0.0 if edge_sample == "before" else 1 / rate
    out = []
    for h in (0, 1):
        mode, edge = cpol * 2 + h, "rise" if cpol == h else "fall"
        mg = sampling_margins(clk, data, edge, rate, cs, cs_active)
        ok = all((v["setup_s"] is None or v["setup_s"] >= 1 / rate) and (v["hold_s"] is None or v["hold_s"] >= hold_min)
                 for v in mg.values())
        out.append({"mode": mode, "readable": ok, "bytes": spi_read(clk, data, edge, n, cs, cs_active, lsb)})
    return out


def spi_modes_ok(clk: Channel, data: list[Channel], cpol: int, rate: float, cs: Channel | None = None,
                 cs_active: int = 0) -> list[int]:
    """The SPI modes a receiver could read the lines in, from the lines alone: CPOL as SCK's idle level, and at that
    mode's sampling edges no data line changing (setup and hold of a sample or more)."""
    out = []
    for mode in (0, 1, 2, 3):
        c, h = mode >> 1, mode & 1
        if c != cpol:
            continue
        mg = sampling_margins(clk, data, "rise" if c == h else "fall", rate, cs, cs_active)
        if all(v[k] is None or v[k] >= 1 / rate for v in mg.values() for k in ("setup_s", "hold_s")):
            out.append(mode)
    return out


def spi_mode(clk: Channel, data: list[Channel], n: int, cs: Channel | None = None, cs_active: int = 0) -> dict:
    """CPOL: SCK's level when CS becomes active (without CS: its level over the longest pause). CPHA, per data line:
    the SCK edge each change of the line follows (the last one at or before it) inside a CS window: after a leading
    edge (leaving the idle level) it was shifted out there and is sampled on the trailing one (CPHA 1); after a
    trailing edge, CPHA 0. Changes before the first edge of a window decide only when there are no others (CPHA 0:
    the first bit is out before the first edge samples it)."""
    if cs is not None:
        ce = np.asarray(cs.edges, np.int64)
        lv_after = cs.level_at(ce)
        on = ce[lv_after == cs_active]
        off = ce[lv_after != cs_active]
        wins = []
        if cs.initial == cs_active:
            wins.append((0, int(off[0]) if len(off) else n, False))
        for t in on:
            j = np.searchsorted(off, t, side="right")
            wins.append((int(t), int(off[j]) if j < len(off) else n, True))
        cpols = [int(clk.level_at(np.array([t]))[0]) for t, _, whole in wins if whole]
    else:
        st, ln, lvl = clk.runs(n)
        if len(ln) < 2:
            return {"windows": 0}
        inner = slice(1, len(ln) - 1) if len(ln) > 2 else slice(0, len(ln))
        k = int(np.argmax(ln[inner])) + inner.start
        cpols = [int(lvl[k])]
        wins = [(0, n, False)]
    if not cpols:
        return {"windows": len(wins)}
    cpol = max(set(cpols), key=cpols.count)
    ce = np.asarray(clk.edges, np.int64)
    leading = clk.level_at(ce) != cpol
    votes = {}
    for ch in data:
        lead = trail = before = 0
        de = np.asarray(ch.edges, np.int64)
        for a, b, _ in wins:
            x = de[(de > a) & (de < b)]
            if len(x) == 0:
                continue
            j = np.searchsorted(ce, x, side="right") - 1
            in_win = (j >= 0) & (ce[np.maximum(j, 0)] > a)
            before += int(np.count_nonzero(~in_win))
            lead += int(np.count_nonzero(leading[j[in_win]]))
            trail += int(np.count_nonzero(~leading[j[in_win]]))
        # a line may take its first bit when CS becomes active in either mode: those changes decide only alone
        cpha = (1 if lead > trail else 0) if lead != trail else (0 if before and not lead else None)
        votes[ch.name] = {"after_leading": lead, "after_trailing": trail, "before_first": before, "cpha": cpha}
    return {"windows": len(wins), "cpol": cpol, "cpol_votes": cpols, "lines": votes}


def i2c_same_sample(scl, sda) -> list[tuple[int, str]]:
    """(tick, "rise" / "fall") where SDA changed in the same sample as SCL rose or fell: SDA's setup before SCL rises,
    or its hold after SCL falls, is under a sample. A decoder cannot tell the order there, and SDA may read as moving
    while SCL is high (a START or STOP that is not there)."""
    if scl.step != 1 or sda.step != 1:
        return []
    e = np.asarray(scl.edges, np.int64)
    lv = scl.initial ^ ((np.arange(len(e)) + 1) & 1)        # the level after each change
    both = np.intersect1d(e, np.asarray(sda.edges, np.int64))
    if not len(both):
        return []
    k = np.searchsorted(e, both)
    return [(int(t), "rise" if lv[i] == 1 else "fall") for t, i in zip(both.tolist(), k.tolist())]


I2C_SPIKE_S = 50e-9     # UM10204's t_SP: the spikes an I2C input's filter suppresses
I2C_RISE_S = 1000e-9    # UM10204's t_r, standard mode: the slowest an edge of a working bus rises


def short_half(ch: Channel, n_samples: int) -> float | None:
    """The shorter of a line's median high and median low, in ticks (its first and last runs left out), or None."""
    if len(ch.edges) < 5:
        return None
    _, length, level = ch.runs(n_samples)
    length, level = length[1:-1], level[1:-1]
    hi, lo = length[level == 1], length[level == 0]
    if not len(hi) or not len(lo):
        return None
    return min(float(np.median(hi)), float(np.median(lo)))


def edge_bursts(ch: Channel, w) -> list[tuple[int, int]]:
    """Runs of changes on a line each within w ticks of the one before (w: one number, or one per edge, the allowance
    before it), an odd number of 3 or more: one edge crossing the threshold more than once (a slow edge with noise on
    it, or ringing). -> (index of its first change, of its last)."""
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 3:
        return []
    w = np.broadcast_to(np.asarray(w, np.float64), e.shape)
    out, i = [], 0
    close = np.diff(e) <= w[1:]
    while i < len(e) - 1:
        if close[i]:
            j = i
            while j < len(close) and close[j]:
                j += 1
            if (j - i + 1) % 2 == 1:                           # changes i..j: a net change, crossed j - i + 1 times
                out.append((i, j))
            i = j + 1
        else:
            i += 1
    return out


def i2c_burst_ticks(cap: Capture, scl: str, freq_hz: float | None = None, at=None):
    """How far one edge's crossings may spread: at most t_r (I2C_RISE_S, the slowest rise of a working bus), and
    under a third of SCL's half (a line never changes three times in that) - the half from the clock's frequency
    when it is given; else per edge of `at`, from SCL's period around it (its rising edges' lower quartile before
    and after: a clock that changes speed is fast where it is fast), or SCL's shorter half over all without `at`.
    0 (none) when a sample is longer than a fifth of t_r: a capture that coarse cannot show an edge's crossings, and
    a pulse of a sample next to an edge is noise, not the edge."""
    rate = float(cap.rate)
    tr = I2C_RISE_S * rate
    if tr < 5:
        return 0
    if freq_hz:
        return int(min(tr, rate / (2 * float(freq_hz)) / 3))
    ch = cap.channel(scl)
    if at is None:
        h = short_half(ch, cap.n_samples)
        return int(min(tr, h / 3)) if h else 0
    from . import kernels as K
    e = np.asarray(ch.edges, np.int64)
    rises = e[K.edge_levels(e, ch.initial) == 1]
    if len(rises) < 3:
        return 0
    return np.minimum(K.local_period(rises, np.asarray(at, np.int64)) / 6, tr).astype(np.int64)


def i2c_input(cap: Capture, scl: str, sda: str, spike_s: float = I2C_SPIKE_S,
              freq_hz: float | None = None) -> tuple[Capture, bool]:
    """The two lines as an I2C input sees them: pulses shorter than spike_s left out (its spike filter; none can be
    when a sample is spike_s or longer), and an edge crossing the threshold more than once within a third of SCL's
    half around it (i2c_burst_ticks) read as one at its last change (its hysteresis). -> (the capture, whether anything changed)."""
    from . import kernels as K
    k = int(np.ceil(spike_s * float(cap.rate) - 1e-9))     # a pulse of fewer samples is shorter than spike_s
    changed = False
    chans = []
    for c in cap.channels:
        if c.name in (scl, sda) and c.step == 1:
            e = np.asarray(c.edges, np.int64)
            if k >= 2:
                e = K.deglitch(e, k)
            c2 = Channel(c.name, c.initial, e, c.step, c.phase, c.acquisition)
            b = edge_bursts(c2, i2c_burst_ticks(cap, scl, freq_hz, e))
            if b:
                keep = np.ones(len(e), bool)
                for i, j in b:
                    keep[i:j] = False
                e = e[keep]
            if len(e) != len(c.edges):
                changed = True
                c = Channel(c.name, c.initial, e, c.step, c.phase, c.acquisition)
        chans.append(c)
    if not changed:
        return cap, False
    return Capture(cap.rate, cap.n_samples, chans, cap.meta, cap.analog, cap.events), True
