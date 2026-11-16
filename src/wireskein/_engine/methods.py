"""The analysis's replaceable steps, by name: where another way of doing a step can be put in to be measured.

    from wireskein._engine import methods
    with methods.use({"pin_classes": my_pin_classes}):
        staged.analyze(cap)

Outside such a block (and in any other thread or context) every step is the engine's own: nothing changes unless a
caller asks. The algorithm bench (bench/, not part of the package) measures its trials this way, the whole analysis
with one step swapped, against the same analysis as it is. Not a public interface: the names and what each step is
given may change with the engine.

Steps (name: what replaces it is called as the engine's own is):
    preclean      fn(cap) -> Capture            the lines before anything reads them (default: as captured)
    pin_classes   fn(sv, cap) -> {pin: PinClass}  taxonomy.pin_classes: each line's class candidates
    pair_relation fn(cap, clk, other) -> PairRelation   survey.pair_relation: how a line keeps to a clock
"""

from __future__ import annotations

from contextlib import contextmanager
from contextvars import ContextVar

STEPS = ("preclean", "pin_classes", "pair_relation")

_SWAPPED: ContextVar[dict] = ContextVar("wireskein_methods", default={})


def get(name: str, default):
    """The step to call: the one put in by use(), else the engine's own (default)."""
    if name not in STEPS:
        raise KeyError(f"no step {name!r} (steps: {', '.join(STEPS)})")
    return _SWAPPED.get().get(name, default)


@contextmanager
def use(steps: dict):
    """Within the block, the given steps replace the engine's (nested blocks add to the outer one's)."""
    unknown = set(steps) - set(STEPS)
    if unknown:
        raise KeyError(f"no step {sorted(unknown)} (steps: {', '.join(STEPS)})")
    token = _SWAPPED.set({**_SWAPPED.get(), **steps})
    try:
        yield
    finally:
        _SWAPPED.reset(token)
