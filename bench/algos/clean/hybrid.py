"""relative's cut (r of the line's own unit, the same in time whatever the rate) with deglitch's floor (pulses
under 2 samples), the floor only where a bit is 4 samples or more: taken that slowly a real bit can be a sample long.

relative alone leaves 1-sample spikes on lines whose unit is a few samples (its cut under 2 samples: nothing done),
and claimed wrongly on noise in ticks; deglitch alone cuts real 1-sample bits of a slow capture and moves with the
rate. A line whose unit the comb cannot tell is left as it is."""

from dataclasses import replace

from wsbench import algorithm
from wsbench.kit import comb_unit, runs
from wireskein._engine import kernels as K
from wireskein._engine.model import Capture


def engine(**params):
    return {"preclean": lambda cap: hybrid(cap, **params)}


@algorithm("clean", "hybrid", status="trial", about="max(r of the line's unit, 2 samples where a bit has 4 or more)",
           variants={"r2": {"r": 0.2}, "r3": {"r": 0.3}}, engine=engine)
def hybrid(cap, r, floor=2, floor_from=4):
    out = []
    for ch in cap.channels:
        u, _ = comb_unit(runs(cap, ch), 12) if len(ch.edges) > 16 else (None, 0.0)
        if not u:
            out.append(ch)
            continue
        k = max(int(r * u), floor * ch.step if u >= floor_from * ch.step else 0)
        out.append(replace(ch, edges=K.deglitch(ch.edges, k)) if k >= 2 * ch.step else ch)
    return Capture(cap.rate, cap.n_samples, out)
