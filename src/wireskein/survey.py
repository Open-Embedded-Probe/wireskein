"""An overview of a capture, read once a block at a time (any length): how each line moves over the whole, before
deciding what to analyse and how (wireskein-survey/1).

    wireskein survey CAPTURE [--pins A,B] [--bins 1000] [--top 10] [--plan] [--split KEY=VALUE,...]

Per line: its changes; their count over the time in `bins` bins (activity); the intervals between its changes and
the lengths of its highs and lows in bins of powers of two; its longest stills (where, how long); the share of its
intervals of a tick or two (a floating input trembling, a reflection ringing); and, for a line that never stops
(its longest interval within four times its usual one), its period and duty. Over all the lines: the longest
stretches where none of them moves. With --plan, the pieces a long capture would be cut into (wireskein.split).
Python: survey_file(path, ...), or survey(source, ...) on a wireskein.split source.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np

FORMAT = "wireskein-survey/1"
SAMPLE = 200_000          # intervals kept per line for its median (the first ones)


def _bin(d: np.ndarray) -> np.ndarray:
    """The power-of-two bin of each length (ticks >= 1): 0 for 1, 1 for 2-3, 2 for 4-7, ..."""
    return np.minimum(63, np.floor(np.log2(np.maximum(d, 1))).astype(np.int64))


def _hist(counts: np.ndarray) -> dict:
    return {f"{1 << k}-{(2 << k) - 1}" if k else "1": int(c) for k, c in enumerate(counts) if c}


class _Top:
    """The `n` longest stills seen: (length, start)."""

    def __init__(self, n: int):
        self.n, self.len, self.s = n, np.zeros(0, np.int64), np.zeros(0, np.int64)

    def add(self, lengths: np.ndarray, starts: np.ndarray) -> None:
        if not len(lengths):
            return
        L, S = np.concatenate((self.len, lengths)), np.concatenate((self.s, starts))
        if len(L) > self.n:                   # the longest; of equal ones the earliest (the same whatever the blocks)
            o = np.lexsort((S, -L))[: self.n]
            L, S = L[o], S[o]
        self.len, self.s = L, S

    def doc(self, rate: float) -> list:
        o = np.lexsort((self.s, -self.len))
        return [{"s": int(self.s[i]), "e": int(self.s[i] + self.len[i]), "seconds": float(self.len[i] / rate)}
                for i in o]


class _Line:
    def __init__(self, name: str, initial: int, bins: int, top: int):
        self.name, self.initial = name, initial
        self.level, self.last = initial, None
        self.count = 0
        self.activity = np.zeros(bins, np.int64)
        self.ivl = np.zeros(64, np.int64)
        self.high = np.zeros(64, np.int64)
        self.low = np.zeros(64, np.int64)
        self.sample, self.hsample, self.short = [], [], 0
        self.top = _Top(top)
        self.max_ivl = 0

    def add(self, e: np.ndarray, n: int, bins: int) -> None:
        if not len(e):
            return
        self.activity += np.bincount(np.minimum(bins - 1, e * bins // max(1, n)), minlength=bins)
        prev = np.concatenate(([self.last], e[:-1])) if self.last is not None else e[:-1]
        cur = e if self.last is not None else e[1:]
        d = cur - prev
        if len(d):
            self.ivl += np.bincount(_bin(d), minlength=64)
            # the level of each run: the level before the change that ends it (change j of the line, from 0:
            # initial ^ (j & 1))
            j0 = self.count if self.last is not None else 1
            lv = (self.initial ^ (np.arange(j0, j0 + len(d)) & 1)).astype(bool)
            self.high += np.bincount(_bin(d[lv]), minlength=64)
            self.low += np.bincount(_bin(d[~lv]), minlength=64)
            self.short += int(np.sum(d <= 2))
            self.top.add(d, prev)
            self.max_ivl = max(self.max_ivl, int(d.max()))
            if len(self.sample) < SAMPLE:
                take = SAMPLE - len(self.sample)
                self.sample.extend(d[:take].tolist())
                self.hsample.extend(d[:take][lv[:take]].tolist())
        self.count += len(e)
        self.last = int(e[-1])

    def doc(self, rate: float, n: int) -> dict:
        out = {"name": self.name, "initial": self.initial, "edges": self.count,
               "activity": self.activity.tolist(), "intervals": _hist(self.ivl), "high": _hist(self.high),
               "low": _hist(self.low), "longest_still": self.top.doc(rate)}
        k = max(1, self.count - 1)
        if self.count > 2:
            out["short_share"] = round(self.short / k, 4)
            usual = float(np.median(self.sample))
            out["usual_interval_s"] = usual / rate
            if self.max_ivl <= 4 * usual:             # never stops: a free-running line
                d = np.asarray(self.sample, np.float64)
                period = float(np.median(d[:-1] + d[1:])) if len(d) > 1 else 2 * usual
                hi = float(np.median(self.hsample)) if self.hsample else None
                out["free_running"] = {"period_s": period / rate, "freq_hz": rate / period if period else None,
                                       **({"duty": round(hi / period, 4)} if hi and period else {})}
        return out


def survey(source, bins: int = 1000, top: int = 10, plan=None) -> dict:
    """The overview of a wireskein.split source (CaptureSource, FileSource, SrSource). plan: split options to also
    give the pieces a long capture is cut into (a second read)."""
    lines = {k: _Line(k, int(source.initial.get(k, 0)), bins, top) for k in source.names}
    still = _Top(top)
    last_any = None
    for _, got in source.blocks():
        for k, e in got.items():
            lines[k].add(np.asarray(e, np.int64), source.n, bins)
        allc = np.sort(np.concatenate([np.asarray(e, np.int64) for e in got.values()] or [np.zeros(0, np.int64)]))
        if len(allc):
            prev = np.concatenate(([last_any], allc[:-1])) if last_any is not None else allc[:-1]
            cur = allc if last_any is not None else allc[1:]
            still.add(cur - prev, prev)
            last_any = int(allc[-1])
    out = {"format": FORMAT, "ticks": source.n, "rate_hz": source.rate, "bins": bins,
           "bin_seconds": source.n / bins / source.rate if source.n else 0,
           "lines": [lines[k].doc(source.rate, source.n) for k in source.names],
           "all_still": still.doc(source.rate)}
    trembling = {ln["name"]: ln["short_share"] for ln in out["lines"] if ln.get("short_share", 0) > 0.3}
    if trembling:
        out["trembling"] = trembling
    if plan is not None:
        from . import split
        opts = split.options(plan)
        out["pieces"] = [p.doc() for p, _ in split.run(source, opts)]
    return out


def survey_file(path, pins: list[str] | None = None, bins: int = 1000, top: int = 10, plan=None) -> dict:
    """The overview of a capture file: a WireSkein file of one part or a logic .sr read a block at a time; anything
    else read whole first."""
    from . import split
    from ._fileformat import sniff
    from .decode import _capture_of, _file_source, load, only
    path = Path(path)
    src = _file_source(path, pins)
    if src is None:
        if sniff(path) == "wireskein":
            from ._fileformat import read_header
            if len(read_header(path)["parts"]) > 1:
                raise ValueError(f"{path}: several parts: survey one (convert --part K)")
        cap = only(load(path), pins)
        src = split.CaptureSource(cap)
        tick = cap.meta.get("tick_hz") or cap.rate
        from fractions import Fraction
        t = Fraction(tick).limit_denominator(10**9)
        raw = {"id": cap.meta.get("capture_id"), "tick_hz": [t.numerator, t.denominator],
               "complete": cap.meta.get("complete", True), "lost": cap.meta.get("lost", []),
               "layout": "timeline", "parts": [{}]}
    else:
        raw = src.raw
    from . import __version__
    return {**survey(src, bins, top, plan), "wireskein": __version__, "capture": _capture_of(path, raw)}


def text(doc: dict) -> list[str]:
    """The overview as lines to read."""
    rate = doc["rate_hz"]
    out = [f"{doc['capture']['file']}: {doc['ticks']} ticks at {rate:g} Hz ({doc['ticks'] / rate:.6g} s)"]
    for ln in doc["lines"]:
        act = np.asarray(ln["activity"])
        busy = float(np.mean(act > 0)) if len(act) else 0.0
        s = f"  {ln['name']:<12} {ln['edges']:>12} changes, active in {busy:6.1%} of the time"
        if ln.get("free_running"):
            fr = ln["free_running"]
            s += f", free-running {fr['freq_hz']:.6g} Hz" + (f" duty {fr['duty']:.3f}" if "duty" in fr else "")
        if ln.get("short_share", 0) > 0.3:
            s += f", trembling ({ln['short_share']:.0%} of intervals 1-2 ticks)"
        if ln["longest_still"]:
            s += f", longest still {ln['longest_still'][0]['seconds']:.6g} s"
        out.append(s)
    if doc["all_still"]:
        st = doc["all_still"][0]
        out.append(f"  every line still at most {st['seconds']:.6g} s (from tick {st['s']})")
    if "pieces" in doc:
        from collections import Counter
        c = Counter(p["cut"] for p in doc["pieces"])
        out.append(f"  pieces: {len(doc['pieces'])} ({', '.join(f'{k} {v}' for k, v in c.items())})")
    return out
