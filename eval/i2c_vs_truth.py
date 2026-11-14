"""An I2C capture against the waveform it should show, when what was sent is known: each transaction's changes on
SCL and SDA, in order, compared with those a right bus makes for it (START, the address byte and R/W, the ACK bit,
STOP), as the samples show them and as WireSkein reads them (timing.i2c_input). Not part of the package; for
captures whose truth is known from the test that made them (tests/data/i2c_bench/h417_slow), never from
WireSkein's own reading.

    uv run python eval/i2c_vs_truth.py FILE... [--addr 0x13] [--rw write] [--ack 0] [--gap-us 100]

Per file: transactions found (stretches of activity apart by --gap-us of quiet), how many show exactly the right
changes, and over all of them the changes too many and too few on each line (an edit distance on the changes'
sequence), as sampled and as read.
"""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

import numpy as np

from wireskein._engine import timing
from wireskein.decode import load


def expected(addr: int, rw: str, ack: int) -> list[str]:
    """The changes a right bus makes, in order, from idle (both high) to idle: 'SDA-' a fall, 'SCL+' a rise."""
    out = ["SDA-", "SCL-"]                                   # START, then the first bit's low
    sda = 0
    bits = [(addr >> (6 - i)) & 1 for i in range(7)] + [1 if rw == "read" else 0] + [0 if ack else 1]
    for b in bits:
        if b != sda:
            out.append("SDA+" if b else "SDA-")
            sda = b
        out += ["SCL+", "SCL-"]
    if sda:                                                  # STOP: SDA low, SCL up, SDA up
        out.append("SDA-")
    out += ["SCL+", "SDA+"]
    return out


def changes(cap, s: int, e: int) -> list[str]:
    ev = []
    for name in ("SCL", "SDA"):
        ch = cap.channel(name)
        x = np.asarray(ch.edges, np.int64)
        x = x[(x >= s) & (x < e)]
        lv = ch.level_at(x)
        ev += [(int(t), f"{name}{'+' if v else '-'}") for t, v in zip(x, lv)]
    ev.sort()
    return [k for _, k in ev]


def diff(got: list[str], want: list[str]) -> tuple[collections.Counter, collections.Counter]:
    """The changes too many and too few, by line and direction: a Levenshtein alignment (a swap is one of each)."""
    n, m = len(got), len(want)
    d = np.zeros((n + 1, m + 1), np.int32)
    d[:, 0] = np.arange(n + 1)
    d[0, :] = np.arange(m + 1)
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i, j] = min(d[i - 1, j] + 1, d[i, j - 1] + 1, d[i - 1, j - 1] + (0 if got[i - 1] == want[j - 1] else 2))
    extra, missing = collections.Counter(), collections.Counter()
    i, j = n, m
    while i or j:
        if i and j and got[i - 1] == want[j - 1] and d[i, j] == d[i - 1, j - 1]:
            i, j = i - 1, j - 1
        elif i and d[i, j] == d[i - 1, j] + 1:
            extra[got[i - 1]] += 1
            i -= 1
        else:
            missing[want[j - 1]] += 1
            j -= 1
    return extra, missing


def transactions(cap, gap: int) -> list[tuple[int, int]]:
    e = np.sort(np.concatenate([np.asarray(c.edges, np.int64) for c in cap.channels]))
    if not len(e):
        return []
    cut = np.flatnonzero(np.diff(e) > gap)
    starts = np.concatenate([[e[0]], e[cut + 1]])
    ends = np.concatenate([e[cut], [e[-1]]])
    return [(int(a) - 1, int(b) + 1) for a, b in zip(starts, ends)]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("files", nargs="+", type=Path)
    ap.add_argument("--addr", type=lambda s: int(s, 0), default=0x13)
    ap.add_argument("--rw", default="write")
    ap.add_argument("--ack", type=int, default=0, help="1 when a target answers the address (0: NACK)")
    ap.add_argument("--gap-us", type=float, default=100.0)
    a = ap.parse_args(argv)
    want = expected(a.addr, a.rw, a.ack)
    for f in a.files:
        cap = load(f)
        read, _ = timing.i2c_input(cap, "SCL", "SDA")
        row = {}
        for label, c in (("sampled", cap), ("read", read)):
            exact, extra, missing = 0, collections.Counter(), collections.Counter()
            spans = transactions(c, int(a.gap_us * 1e-6 * float(c.rate)))
            for s, e in spans:
                x, mi = diff(changes(c, s, e), want)
                exact += not x and not mi
                extra += x
                missing += mi
            row[label] = (len(spans), exact, dict(extra), dict(missing))
        print(f"{f.stem}:")
        for label, (n, exact, extra, missing) in row.items():
            print(f"  {label:8s} {exact:3d} of {n:3d} exact; too many {extra}; too few {missing}")


if __name__ == "__main__":
    main()
