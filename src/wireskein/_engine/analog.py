"""Analog channels for the checks: levels in volts, and analog read as logic.

to_logic() turns an analog trace into a logic Channel on the capture's ticks,
so every logic check (square, uart, i2c, ...) can run on a line captured by an
ADC. A threshold is one voltage, or (low, high) for hysteresis: the level goes
high when a sample reaches `high`, low when one reaches `low`, and keeps its
level in between (a noisy slow edge then makes one edge, not a burst).

Each edge is placed where the line between the two samples around it crosses
the threshold, rounded up to a tick (an edge at e: tick e is the first of the
new level). The channel's step is one analog sample: an ADC only sees the line
at its samples, so that is the resolution the checks must allow for.
"""

from __future__ import annotations

import math
from fractions import Fraction

import numpy as np

from .model import AnalogTrace, Channel


def thresholds(threshold) -> tuple[float, float]:
    """(low, high) from one value or a pair."""
    if isinstance(threshold, (list, tuple)):
        low, high = (float(v) for v in threshold)
    else:
        low = high = float(threshold)
    if low > high:
        raise ValueError(f"threshold {threshold}: low must be <= high")
    return low, high


def schmitt(v: np.ndarray, low: float, high: float) -> np.ndarray:
    """Level (0/1) of each sample with hysteresis; NaN samples keep the level."""
    state = np.full(len(v), np.nan)
    with np.errstate(invalid="ignore"):
        state[v >= high] = 1
        state[v <= low] = 0
    known = ~np.isnan(state)
    if not known.any():                    # never reached either threshold: the side of the middle it is on
        mid = np.nanmean(v) if np.isfinite(v).any() else 0.0
        return np.full(len(v), 1 if mid >= (low + high) / 2 else 0, np.uint8)
    idx = np.where(known, np.arange(len(v)), 0)
    np.maximum.accumulate(idx, out=idx)    # forward fill: each sample takes the last decided level
    first = int(np.argmax(known))
    out = state[idx]
    out[:first] = state[first]             # before the first decision: the level it settles to
    return out.astype(np.uint8)


def crossings(a: AnalogTrace, tick_hz, threshold) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """(levels per sample, crossing times in ticks as floats, new level at each):
    where the line between the two samples around each change crosses the
    threshold (hysteresis: `high` going up, `low` going down). Needs volts."""
    v = a.volts()
    if v is None:
        raise ValueError(f"{a.name}: no conversion to volts, so no threshold in volts applies")
    low, high = thresholds(threshold)
    clip = clipped(a)
    if clip is not None and not clip[2] < low <= high < clip[3]:
        raise ValueError(f"{a.name}: threshold {low:g}..{high:g} V is not inside the frontend's range "
                         f"{clip[2]:.3f}..{clip[3]:.3f} V (the ends clip, so they say only 'at or beyond')")
    lv = schmitt(v, low, high)
    per = float(Fraction(tick_hz) / a.rate_hz)
    k = np.flatnonzero(lv[1:] != lv[:-1]) + 1          # first sample of each new level
    th = np.where(lv[k] == 1, high, low)
    before, after = v[k - 1], v[k]
    with np.errstate(invalid="ignore", divide="ignore"):
        frac = np.clip((th - before) / (after - before), 0.0, 1.0)
    frac = np.where(np.isfinite(frac), frac, 1.0)
    return lv, float(a.t0_ticks) + (k - 1 + frac) * per, lv[k]


def to_logic(a: AnalogTrace, tick_hz, threshold) -> Channel:
    """The analog trace as a logic channel (see the module docstring). Needs volts."""
    lv, times, new = crossings(a, tick_hz, threshold)
    low, high = thresholds(threshold)
    per = float(Fraction(tick_hz) / a.rate_hz)
    t0 = float(a.t0_ticks)
    edges = np.ceil(times - 1e-9).astype(np.int64)
    if len(edges) > 1:                                  # an ADC faster than the ticks: the last level at each tick
        keep = np.concatenate([np.diff(edges) > 0, [True]])
        if not keep.all():
            edges, lv_k = edges[keep], new[keep]
            flip = np.concatenate([[lv_k[0] != lv[0]], lv_k[1:] != lv_k[:-1]])
            edges = edges[flip]
    return Channel(a.name, int(lv[0]) if len(lv) else 0, edges, max(1, round(per)), max(0, math.floor(t0)),
                   {**a.acquisition, "threshold_v": [low, high]})


def clipped(a: AnalogTrace):
    """Where a raw channel sat at an end of its converter (OEP capture §1.2): the code 0 means the input was at or
    below the frontend's low end, the top code (2^value_bits - 1) at or above its high end, so neither is a
    voltage. -> (low mask, high mask, low end V, high end V), or None when the file cannot tell (volts only,
    no value_bits, or no conversion to volts)."""
    if a.encoding != "analog" or not a.value_bits or a.zero is None or a.scale_nv is None:
        return None
    top = (1 << a.value_bits) - 1
    raw = np.asarray(a.values)
    to_v = lambda code: (code - a.zero) * a.scale_nv * 1e-9    # noqa: E731
    if a.scale_nv < 0:                    # an inverting frontend: code 0 is the high end
        return raw >= top, raw <= 0, float(to_v(top)), float(to_v(0))
    return raw <= 0, raw >= top, float(to_v(0)), float(to_v(top))


def levels(a: AnalogTrace) -> dict | None:
    """Mean, min, max and peak-to-peak in volts over the samples inside the converter's range (None: no conversion
    to volts); with the counts of samples clipped at either end and that range, when the file can tell."""
    v = a.volts()
    if v is None:
        return None
    ok = np.isfinite(v)
    out = {}
    clip = clipped(a)
    if clip is not None:
        low, high, lo_v, hi_v = clip
        out.update(clipped_low=int(np.count_nonzero(low & ok)), clipped_high=int(np.count_nonzero(high & ok)),
                   range_v=[lo_v, hi_v])
        ok &= ~low & ~high
    v = v[ok]
    if not len(v):
        return {"samples": 0, **out}
    return {"samples": int(len(v)), "mean_v": float(v.mean()), "min_v": float(v.min()), "max_v": float(v.max()),
            "p2p_v": float(v.max() - v.min()), "std_v": float(v.std()), **out}
