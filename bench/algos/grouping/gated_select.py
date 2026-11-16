"""A clocked bus's frames cut by its select line (SPI's CS) read with the select's changes closer than T / w merged
(T: the bus clock's period): a spike or a crosstalk pulse on CS no longer ends a frame mid-word.

Why (2026-11-16, the calibration run 20261116T112652Z): of main's 49 wrong confirmed claims, 40 were SPI buses read
right but without CS. In some, CS was not even a candidate (lineclass/gated_sparse); in the others it was, but
typed.frames_select cut the frames at every spike on CS, the words fell apart and the hypothesis without CS (frames
at clock gaps) scored higher. Into the engine as its select_frames step (wireskein._engine.methods); grouping's
own stage does not frame, so this file registers it as an e2e algorithm (inject-like) with its engine steps."""

from dataclasses import replace

import numpy as np

from wsbench import algorithm, registry
from wireskein._engine import methods, typed
from wireskein._engine.model import Capture


def collapse(edges, w: float) -> np.ndarray:
    """grouping/gated.py's (an algorithm file imports no other): changes each within w ticks of the one before are
    one (an odd number a change at the last, an even number none)."""
    e = np.asarray(edges, np.int64)
    if len(e) < 2 or w < 2:
        return e
    first = np.concatenate(([True], np.diff(e) >= w))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


def select_frames(w: float):
    def frames(cap, sv, sb):
        ci = sv.clocks.get(sb.clock)
        if ci is None or not ci.period:
            return typed.frames_select(cap, sv, sb)
        chans = [replace(x, edges=collapse(x.edges, ci.period / w)) if x.name == sb.select else x
                 for x in cap.channels]
        return typed.frames_select(Capture(cap.rate, cap.n_samples, chans), sv, sb)
    return frames


def engine(w, use):
    """The select step and the named algorithms' steps (combo in e2e/score.py takes them all)."""
    steps = {"select_frames": select_frames(w)}
    for ref in use:
        stage, name = ref.split("/", 1)
        steps.update(registry.get(stage, name).engine_steps())
    return steps


def _claims(cap):
    from wireskein import decode
    return decode.export(decode.analyze(cap), cap, mode="all", alternatives=True)["claims"]


@algorithm("e2e", "select", status="trial", engine=engine,
           about="frames cut by a select line read with its changes closer than T / w merged (and the steps named)",
           variants={  # the select step alone; with the sparse count gated too; and with grouping/gated's pairs
                     "w4": {"w": 4, "use": []},
                     "w4_sparse": {"w": 4, "use": ["lineclass/gated_sparse.w4"]},
                     "w4_sparse_gated": {"w": 4, "use": ["lineclass/gated_sparse.w4", "grouping/gated.w4"]}})
def select(cap, w, use):
    with methods.use(engine(w, use)):
        return _claims(cap)
