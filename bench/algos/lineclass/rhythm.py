"""Each line's own rate first, then the line read through it, then its class (the user's hypothesis, 2026-11-16: with
the fundamental known, noise is easy to gate).

1. unit u: the comb over the line's runs (noise hardly moves it: bit_unit/comb)
2. gate: changes closer than g * u are one change (an odd number of them: at the last; even: none)
3. on the gated line: rising intervals of one period (clock-like), runs whole multiples of u (data-like), how few
   changes it has against the other lines' gated counts (select-like); all measured after the gate, so that noise
   on the busiest line does not make the others look sparse
4. static / clock / periodic / select / data / random

taxonomy.pin_classes (main) measures on the raw lines, and calls a line sparse against the busiest raw line."""

import numpy as np

from wsbench import algorithm
from wireskein._engine.taxonomy import PinClass, pin_classes
from wsbench.kit import comb_unit, runs
from wireskein._engine import kernels as K


def _collapse(e: np.ndarray, w: float) -> np.ndarray:
    if len(e) < 2 or w < 2:
        return e
    first = np.concatenate(([True], np.diff(e) >= w))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


def _two_valued(hi: np.ndarray, lo: np.ndarray, tol: float = 0.15, share: float = 0.8) -> bool:
    """Each level's runs of one length (a clock's high and low halves; a PWM's on and off), most of them within tol."""
    def tight(x):
        x = x[x < 4 * np.median(x)] if len(x) else x          # the gaps between bursts are not halves
        if len(x) < 4:
            return False
        # pieces of a half that noise split (under a third of the usual half) are not halves either, as long as
        # most runs are whole
        whole = x[x >= np.median(x) / 3]
        if len(whole) < 0.6 * len(x):
            return False
        m = np.median(whole)
        return float(np.mean(np.abs(whole - m) <= max(1.0, tol * m))) >= share
    return tight(hi) and tight(lo)


def _local_regular(x: np.ndarray, tol: float = 0.15) -> float:
    """Share of neighbouring values within tol of each other, gaps (over 3x both neighbours) left out: a clock whose
    rate changes between bursts, or slowly, is regular here and not to the comb (as features.local_clock, here on a
    gated line)."""
    if len(x) < 12:
        return 0.0
    prev = np.concatenate(([np.inf], x[:-1]))
    nxt = np.concatenate((x[1:], [np.inf]))
    inb = ~(x > 3 * np.minimum(prev, nxt))
    both = inb[1:] & inb[:-1]
    if both.sum() < 8:
        return 0.0
    r = x[1:][both] / np.maximum(x[:-1][both], 1)
    return float(np.mean(np.abs(np.log(r)) < np.log(1 + tol) + 1.0 / np.maximum(x[1:][both], 1)))


def _line(cap, ch, gate, short_ok=False):
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 2:
        return None
    r = runs(cap, ch)
    u, us = comb_unit(r, 12) if len(r) >= 8 else (None, 0.0)
    # heavy noise leaves the comb unsure: the unit again from the runs over 2 samples. Where it is 6 samples or
    # more, a run of 2 cannot be a bit: noise, to be gated
    long_r = r[r > 2 * ch.step]
    u2, us2 = comb_unit(long_r, 12) if len(long_r) >= 8 else (None, 0.0)
    if (not u or us < 0.3) and u2 and us2 >= 0.3 and u2 >= 6 * ch.step:
        u, us = u2, us2
    if u and us >= 0.3:
        # at least 2 samples where a unit has 4 or more: a 1-sample pulse is no bit of it (as clean/hybrid)
        e = _collapse(e, max(gate * u, 2 * ch.step if u >= 4 * ch.step else 0))
    if len(e) < 2:
        return None
    lv = K.edge_levels(e, ch.initial)
    rise = e[lv == 1]
    iv = np.diff(rise).astype(np.float64)
    # clock-like: inside bursts (gaps over 3x the neighbours left out), each rise one period after the one before
    period, pscore = comb_unit(iv, 2) if len(iv) >= 8 else (None, 0.0)
    if short_ok and 3 <= len(iv) < 8:
        # too few for the comb (a capture of one short transfer): most intervals within 15 % of their median
        m = float(np.median(iv))
        x = iv[iv <= 3 * m]
        period, pscore = m, float(np.mean(np.abs(x - m) <= max(1.0, 0.15 * m))) * len(x) / len(iv)
    _, length, level = K.runs(e, ch.initial, cap.n_samples)
    inner, ilev = length[1:-1].astype(np.float64), level[1:-1]
    hi = inner[ilev == 1]
    lo = inner[ilev == 0]
    duty = float(hi.sum() / max(1.0, hi.sum() + lo.sum())) if len(inner) else 0.5
    # continuous: no gap longer than 8 periods anywhere
    continuous = bool(period) and len(iv) > 0 and float(iv.max()) <= 8 * period and (rise[0] < 8 * period + e[0])
    # data-like: the gated runs whole multiples of one unit
    u2, dscore = comb_unit(inner, 12) if len(inner) >= 8 else (None, 0.0)
    # a clock whose rate changes: each rise one interval after the last, each half as long as the half before
    local = min(_local_regular(iv), max(_local_regular(hi), _local_regular(lo)))
    return {"edges": len(e), "runs": len(inner), "period": period, "pscore": pscore, "duty": duty,
            "continuous": continuous,
            "dscore": dscore, "unit": u2, "two_valued": _two_valued(hi, lo), "local": local}


def engine(**params):
    """Into the engine as its pin classes: rhythm decides whether a line is a clock; the engine's other candidates
    (sparse for a select, pulse) stay as its own pin_classes gives them (rhythm leaves select to grouping)."""
    def classes(sv, cap):
        own = pin_classes(sv, cap)
        got = rhythm(cap, **params)
        out = {}
        for n, pc in own.items():
            label = (got.get(n) or {}).get("label")
            sc, cand = dict(pc.scores), list(pc.candidates)
            if label in ("clock", "periodic"):
                sc["clock"] = max(sc["clock"], 0.9)
                cand = ["clock"] + [c for c in cand if c not in ("clock", "data")]
            elif label in ("data", "random"):
                sc["clock"] = min(sc["clock"], 0.1)
                cand = [c for c in cand if c != "clock"]
                if "data" not in cand:
                    cand.append("data")
            out[n] = PinClass(n, sc, cand)
        return out
    return {"pin_classes": classes}


@algorithm("lineclass", "rhythm", status="trial", about="each line's unit, the line gated by it, then its class",
           engine=engine, variants={
               # no select here (grouping tells it, from what the line frames); too few runs: data; a line of a
               # few clocks only (3 to 7 rising intervals) judged on them
               "g3ns": {"gate": 0.3, "sparse": 0.0, "local": 0.8, "few_data": True, "select": False,
                        "short_ok": True}})
@algorithm("lineclass", "rhythm", status="retired", about="each line's unit, the line gated by it, then its class",
           variants={"g3": {"gate": 0.3, "sparse": 0.05}, "g3s15": {"gate": 0.3, "sparse": 0.15},
                     "g3s15l": {"gate": 0.3, "sparse": 0.15, "local": 0.8},
                     "g3t15l": {"gate": 0.3, "sparse": 0.15, "local": 0.8, "base": "top", "few_data": True},
                     "g3n": {"gate": 0.3, "sparse": 0.0, "local": 0.8, "few_data": True, "select": False}},
           reason="steps on the way to g3ns (2026-11-16, local runs on noise, stress, rate, real-i2c-tune): g3 and "
                  "g3s15 lose clocks whose rate changes (freqhop 0.56-0.61) and call real I2C's SDA select (a "
                  "line's few edges against the median); g3t15l calls slow data lines select against the busiest "
                  "line; g3n misses clocks of a few cycles. Select is a role for grouping, not a line's class")
def rhythm(cap, gate, sparse, local=None, base="median", few_data=False, select=True, short_ok=False):
    info = {ch.name: _line(cap, ch, gate, short_ok) for ch in cap.channels}
    counts = [i["edges"] for i in info.values() if i]
    med = float(np.median(counts)) if counts else 0.0
    top = float(max(counts)) if counts else 0.0
    out = {}
    for name, i in info.items():
        if i is None:
            out[name] = {"label": "static"}
            continue
        if (i["pscore"] >= 0.6 and i["two_valued"]) or (local is not None and i["local"] >= local):
            label = "periodic" if i["continuous"] and abs(i["duty"] - 0.5) > 0.15 else "clock"
        elif select and ((i["edges"] <= sparse * top) if base == "top" else (i["edges"] <= max(8, sparse * med))):
            label = "select"           # few changes, and not one on-time and one off-time (a CS's windows vary)
        elif few_data and i["runs"] < 8:
            label = "data"             # too few runs to tell: data rather than noise
        elif i["dscore"] >= 0.4:
            label = "data"
        else:
            label = "random"
        out[name] = {"label": label, "pscore": i["pscore"], "dscore": i["dscore"], "edges": i["edges"]}
    return out
