"""What WireSkein does today: an asynchronous line split where its bit time changes (typed.async_segments); a clock
not split at all (its protocols sample on its edges, whatever the rate: docs/design 3.1), one block at the survey's
period."""

from wsbench import algorithm
from wireskein._engine import typed
from wireskein._engine.features import channel_features
from wireskein._engine.survey import clock_info, survey


@algorithm("segments", "current", status="main", about="async: typed.async_segments; clock: one block, its period")
def current(cap, lines):
    out = {}
    sv = None
    for ln, kind in lines.items():
        if kind == "async":
            sv = sv or survey(cap)
            out[ln] = [tuple(x) for x in typed.async_segments(cap, sv, ln)]
        else:
            ci = clock_info(cap, channel_features(cap, cap.channel(ln)))
            out[ln] = [(0, cap.n_samples, ci.period)] if ci and ci.period else []
    return out
