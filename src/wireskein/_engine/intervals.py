"""What a channel kept by interval (IntervalTrace) says for sure.

An interval channel keeps one summary per `step` ticks, not the line itself, so
a check answers only what follows from the summaries; otherwise it says it
cannot tell (unchecked), never a guess:

- any: a value at the inactive level means the whole interval was inactive; a
  value at `active` means at least one tick was active (where, how often: not
  kept).
- latch: bit 0 is the level at the interval's last tick; bit 1 says the line
  went to `active` inside it (from the tick before), bit 1 clear that it did
  not. The channel's first tick is compared with nothing, so a line active
  there may fall inside the first interval unseen.
"""

from __future__ import annotations

import numpy as np

from .model import IntervalTrace


def known(t: IntervalTrace) -> list[int]:
    """The levels the line surely had, in time order (one entry per known point; repeats kept)."""
    a, out = t.active, []
    for v in t.values.tolist():
        if t.kind == "any":
            out.append(1 - a if v == 1 - a else a)       # inactive: all of it; active: somewhere in it
        else:
            if v >> 1:
                out += [1 - a, a]                        # a change to active: inactive just before, active after
            out.append(v & 1)                            # the level at the interval's last tick
    return out


def rises_at_least(t: IntervalTrace) -> int:
    """Rising edges (0 -> 1) the line surely had: each 0 known before a 1 needs one in between."""
    seq = known(t)
    return sum(1 for p, q in zip(seq, seq[1:]) if p == 0 and q == 1)


def start_level(t: IntervalTrace) -> int | None:
    """The level at the channel's first tick, when the summaries fix it."""
    if not len(t.values):
        return None
    v, a = int(t.values[0]), t.active
    if t.kind == "any":
        return 1 - a if v == 1 - a else None             # active somewhere does not say it was at the start
    return a if v == a else None                         # latch: active at the end and no change to active: all active


def end_level(t: IntervalTrace) -> int | None:
    """The level at the channel's last tick (end - 1), when the summaries fix it."""
    if not len(t.values):
        return None
    v, a = int(t.values[-1]), t.active
    if t.kind == "any":
        return 1 - a if v == 1 - a else None
    return v & 1


def constant(t: IntervalTrace, level: int) -> bool | None:
    """Whether the line stayed at `level` all through the channel (None: the summaries cannot tell)."""
    vals, a = t.values, t.active
    if t.kind == "any":
        if level == 1 - a:
            return bool(np.all(vals == 1 - a))
        return False if np.any(vals == 1 - a) else None
    if np.any(vals >> 1) or np.any((vals & 1) != level):
        return False
    return True if level == a else None                  # inactive throughout but for the first interval's start


def moved(t: IntervalTrace) -> bool | None:
    """Whether the line changed at all (None: cannot tell)."""
    seq = known(t)
    if len(set(seq)) > 1:
        return True
    if t.kind == "latch" and seq and seq[0] == t.active:
        return False                                     # active at every end, never a change to active
    return None if t.kind == "latch" or (seq and seq[0] == t.active) else False


def describe(t: IntervalTrace) -> str:
    return f"{t.name} is kept by interval ({t.encoding}, {t.step} ticks a value)"
