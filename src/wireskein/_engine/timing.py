"""Times between edges of the lines of a bus or of a group of outputs: the phase of two PWMs, the dead time of a
complementary pair, the skew of two patterns, I2C's per-bit times (UM10204), the SPI mode from the lines.

A Channel's edge at tick e changed somewhere in (e - step, e], so the time between edges of two channels is known
to res = the larger of their steps (in ticks). A limit is met when the whole interval [v - res, v + res] is on its
side, broken when the whole interval is on the other side, and undecided otherwise (`judge`).
"""

from __future__ import annotations

import numpy as np

from .model import Channel


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


def skew(a: Channel, b: Channel, rate: float) -> dict:
    """Each edge of a with b's nearest edge of the same direction (b - a, ticks). An edge without one nearer than
    half the closest spacing of a's edges of that direction is unpaired (b moved less often, or not with a)."""
    out = {"delays": np.zeros(0, np.int64), "unpaired_a": 0, "unpaired_b": 0}
    for edge in ("rise", "fall"):
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
    follows the bus being free, t_buf). t_vd_dat: from SCL falling to the last SDA change of that low."""
    starts, lv = states([scl, sda], n)
    out = {k: [] for k in I2C_TIMES}
    out["periods"] = []
    in_tx = False
    last_rise = last_fall = last_stop = start_at = None
    cond = False                        # a START or STOP since SCL last rose
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
                    out["t_vd_dat"].append((last_fall, changes[-1] - last_fall))
                if last_rise is not None and not cond:
                    out["periods"].append((last_rise, t - last_rise))
            last_rise, cond = t, False
        elif s0 != s1:                                              # SCL falls
            if start_at is not None:
                out["t_hd_sta"].append((start_at, t - start_at))
            elif in_tx and last_rise is not None and not cond:
                out["t_high"].append((last_rise, t - last_rise))
            start_at, changes = None, []
            last_fall = t if in_tx else None
    return out


# ---------------- SPI ----------------

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
