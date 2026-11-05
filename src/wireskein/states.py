"""The states a logic line goes through, and where it changes: a short summary of a long capture.

A line is, at any time, at a level (0 or 1) or running a periodic wave (a period and a high time). `states` splits
each line into those stretches and lists the pulses between them that belong to neither (a period cut by a change, a
glitch). The whole capture comes down to a few lines per pin - what a reader needs to see what happened, and where,
without the samples.

    wireskein states CAPTURE [--pin P] [--part K] [--tol 0.02] [--min-periods 3] [--json]

Each stretch: t0 / t1 (ticks of the part), and either {"level": v} or {"period_s", "high_s", "freq_hz", "duty",
"periods"}; between stretches, "odd" pulses (rise tick, high_s, period_s). A wave is a run of at least min_periods
whole periods alike within tol (relative to the period, or 2 samples); a level is a stretch with no edge longer than
the period of the waves beside it (or than the whole part, for a line that never moves).
"""

from __future__ import annotations

from pathlib import Path

import numpy as np


def pulse_rows(edges: np.ndarray, initial: int) -> np.ndarray:
    """Every whole pulse of a line: (rise, high, period to the next rise) in ticks."""
    rises = edges[0::2] if initial == 0 else edges[1::2]
    falls = edges[1::2] if initial == 0 else edges[0::2]
    if len(rises) < 2:
        return np.zeros((0, 3), np.int64)
    j = np.searchsorted(falls, rises[:-1])
    ok = j < len(falls)
    r = rises[:-1][ok]
    return np.stack([r, falls[j[ok]] - r, rises[1:][ok] - r], axis=1).astype(np.int64)


def of_line(edges: np.ndarray, initial: int, n: int, rate: float, step: int = 1, tol: float = 0.02,
            min_periods: int = 3, max_odd: int = 3) -> list[dict]:
    """The stretches of one line over ticks [0, n), in order: levels, waves, and the odd pulses between them."""
    edges = np.asarray(edges, np.int64)
    if not len(edges):
        return [{"t0": 0, "t1": n, "level": int(initial)}]
    rows = pulse_rows(edges, initial)
    slack = 2 * (step + 1)

    def alike(a, b) -> bool:                            # period and high each within tol of their own (or 2 samples)
        return abs(a[2] - b[2]) <= max(slack, tol * b[2]) and abs(a[1] - b[1]) <= max(slack, tol * b[1])

    # runs of alike whole pulses
    runs, i = [], 0
    while i < len(rows):
        k = i + 1
        while k < len(rows) and alike(rows[k], rows[i]):
            k += 1
        runs.append((i, k))
        i = k
    waves = [(a, b) for a, b in runs if b - a >= min_periods]
    out: list[dict] = []
    near = [int(rows[a:b, 2].max()) for a, b in waves]

    def between(t0: int, t1: int, before: dict | None) -> None:
        """Ticks [t0, t1) outside the waves: quiet longer than the waves' periods (with no wave, than 10 times the
        edges' spacing here) is a level; a shorter high is a pulse, odd unless it is the wave before it ending whole."""
        e = edges[(edges >= t0) & (edges < t1)]
        marks = np.concatenate(([t0], e, [t1])).astype(np.int64)
        gaps = np.diff(marks)
        floor = max(near) if near else (10 * float(np.median(gaps)) if len(gaps) > 2 else 0)
        lv0 = int(initial ^ (np.searchsorted(edges, t0, side="left") & 1))     # the level just before t0's edge
        for i in range(len(marks) - 1):
            a, b = int(marks[i]), int(marks[i + 1])
            if b <= a:
                continue
            here = int(initial ^ (np.searchsorted(edges, a, side="right") & 1)) if i or len(e) and e[0] == t0 else lv0
            if b - a > floor:
                out.append({"t0": a, "t1": b, "level": here})
            elif here == 1 and (i or len(e) and e[0] == t0):         # a high that started at an edge here
                c = int(marks[i + 2]) if i + 2 < len(marks) else None
                per = c - a if c is not None and c <= t1 and c - b <= floor else None
                if before is not None and a == before["t1"] and alike_high(b - a, before) and per is None:
                    before["t1"] = b                                  # the wave's last pulse, whole: the wave ends here
                    continue
                out.append({"odd": a, "high_s": (b - a) / rate, "period_s": per / rate if per else None,
                            "end": c if per else b})

    def alike_high(high: int, w: dict) -> bool:
        return abs(high - w["high_s"] * rate) <= max(slack, tol * w["high_s"] * rate)

    t, prev = 0, None
    for a, b in waves:
        w0 = int(rows[a, 0])
        between(t, w0, prev)
        rs = rows[a:b]
        prev = {"t0": w0, "t1": int(rows[b - 1, 0] + rows[b - 1, 2]), "period_s": float(np.median(rs[:, 2])) / rate,
                "high_s": float(np.median(rs[:, 1])) / rate, "freq_hz": rate / float(np.median(rs[:, 2])),
                "duty": float(np.median(rs[:, 1] / rs[:, 2])), "periods": int(b - a)}
        out.append(prev)
        t = prev["t1"]
    between(t, n, prev)
    out.sort(key=lambda x: x.get("t0", x.get("odd")))
    return _collapse(_merge_levels(out), max_odd)


def _collapse(out: list[dict], max_odd: int) -> list[dict]:
    """More than max_odd odd pulses in a row (a bus, a burst: not a wave) as one stretch of activity."""
    res: list[dict] = []
    i = 0
    while i < len(out):
        k = i
        while k < len(out) and "odd" in out[k]:
            k += 1
        if k - i > max_odd:
            last = out[k - 1]
            res.append({"t0": out[i]["odd"], "t1": last["end"], "activity": k - i})
            i = k
        elif k > i:
            res.extend(out[i:k])
            i = k
        else:
            res.append(out[i])
            i += 1
    return res


def _first_row_at(rows: np.ndarray, t: int) -> int:
    return int(np.searchsorted(rows[:, 0], t)) if len(rows) else 0


def _merge_levels(out: list[dict]) -> list[dict]:
    merged: list[dict] = []
    for x in out:
        if merged and "level" in x and "level" in merged[-1] and merged[-1]["level"] == x["level"] \
                and merged[-1]["t1"] == x["t0"]:
            merged[-1]["t1"] = x["t1"]
        else:
            merged.append(x)
    return merged


def of_file(path: str | Path, pins: list[str] | None = None, part: int | None = None, tol: float = 0.02,
            min_periods: int = 3) -> dict:
    """{"tick_hz", "parts": [{"part", "ticks", "lines": {pin: [stretches]}}]} of a capture's logic lines."""
    from ._engine import fileio
    caps = fileio.load_parts(Path(path))
    out = {"file": str(path), "tick_hz": float(caps[0].rate) if caps else None, "parts": []}
    for k, cap in enumerate(caps):
        if part is not None and k != part:
            continue
        lines = {}
        for ch in cap.channels:
            if pins and ch.name not in pins:
                continue
            lines[ch.name] = of_line(ch.edges, ch.initial, cap.n_samples, cap.rate, ch.step, tol, min_periods)
        out["parts"].append({"part": k, "ticks": int(cap.n_samples), "lines": lines})
    return out


def text(doc: dict) -> list[str]:
    """One line per stretch and odd pulse, times in ms from the part's start."""
    hz = doc["tick_hz"]
    out = []
    for p in doc["parts"]:
        if len(doc["parts"]) > 1:
            out.append(f"part {p['part']}: {p['ticks'] / hz:.6g} s")
        for pin, xs in p["lines"].items():
            out.append(f"{pin}:")
            for x in xs:
                if "activity" in x:
                    out.append(f"  {x['t0'] / hz * 1e3:12.3f} - {x['t1'] / hz * 1e3:.3f} ms  activity: {x['activity']} "
                               f"pulses, no wave")
                elif "odd" in x:
                    out.append(f"  {x['odd'] / hz * 1e3:12.3f} ms  odd pulse: high {x['high_s'] * 1e6:.1f} us"
                               + (f", period {x['period_s'] * 1e6:.1f} us" if x["period_s"] is not None else ""))
                elif "level" in x:
                    out.append(f"  {x['t0'] / hz * 1e3:12.3f} - {x['t1'] / hz * 1e3:.3f} ms  level {x['level']}")
                else:
                    out.append(f"  {x['t0'] / hz * 1e3:12.3f} - {x['t1'] / hz * 1e3:.3f} ms  {x['freq_hz']:.6g} Hz, "
                               f"high {x['high_s'] * 1e6:.1f} us (duty {x['duty']:.4f}), {x['periods']} periods")
    return out
