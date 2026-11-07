"""What a line kept as its changes at a resolution of `step` ticks (EventTrace) says for sure.

Each listed quantum says how many times the line changed in it (127: at least) and its level at the quantum's last
tick; a quantum not listed did not change. So the levels at the quantum ends, whether the line moved and the number
of rising edges are exact (the last one a lower bound when a count saturated); where a change fell inside its quantum
is not kept, so timing checks see the line at that resolution, and checks that need every edge are unchecked when a
quantum hides more than one change.

A change_latch line (acquisition["reduction"], wireskein-format §4.5) says less: per quantum whether the line changed
and its level at the end, so a count is 1 for an odd number and 2 for an even one (2 or more). The levels and whether
it moved are still exact; the number of rising edges is a lower bound.
"""

from __future__ import annotations

import numpy as np

from .model import EventTrace

SATURATED = 127


def before(t: EventTrace) -> np.ndarray:
    """The level just before each listed quantum."""
    return np.concatenate(([t.start_level], t.levels[:-1])).astype(np.int64)


def rises(t: EventTrace) -> tuple[int, bool]:
    """(rising edges, exact): from level b, c changes hold ceil(c/2) rises when b is 0 and floor(c/2) when b is 1. A
    saturated count makes the total a lower bound."""
    c = t.counts.astype(np.int64)
    b = before(t)
    n = int(np.sum(np.where(b == 0, (c + 1) // 2, c // 2)))
    return n, not bool(np.any(c >= SATURATED)) and not (latched(t) and len(c))


def latched(t: EventTrace) -> bool:
    """A change_latch line: its counts are lower bounds (an odd number, or an even one)."""
    return t.acquisition.get("reduction") == "change_latch"


def first_level(t: EventTrace) -> int | None:
    """The level at the first tick: start_level unless quantum 0 changed - then, with step > 1, whether at its first
    tick or later is not known (None)."""
    if len(t.quanta) and t.quanta[0] == 0:
        return None
    return int(t.start_level)


def end_level(t: EventTrace) -> int:
    return int(t.levels[-1]) if len(t.levels) else int(t.start_level)


def moved(t: EventTrace) -> bool:
    return bool(len(t.quanta))


def describe(t: EventTrace) -> str:
    if latched(t):
        return (f"{t.name} is kept as change_latch at {t.step} ticks an interval (whether it changed in each, not how "
                f"often or where; {t.hidden} intervals hold two or more changes)")
    return (f"{t.name} is kept as changes at {t.step} ticks a quantum ({t.hidden} quanta hold more than one change, "
            f"where in them is not kept)")
