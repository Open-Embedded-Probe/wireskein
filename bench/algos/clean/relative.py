"""Pulses short against the line's own bit, not against a sample: the same cut whatever the rate it was taken at.

Each line's unit (the shortest whole run) comes from the cosine comb over its runs, which noise hardly moves
(bit_unit/comb); pulses shorter than r of it are dropped (kernels.deglitch: a pulse's two edges go; a bounce right
after an edge moves the edge to the bounce's end, the last crossing, as an input with hysteresis reads it)."""

from dataclasses import replace

from wsbench import algorithm
from wsbench.kit import comb_unit, runs
from wireskein._engine import kernels as K
from wireskein._engine.model import Capture


@algorithm("clean", "relative", status="retired", about="pulses under r of the line's own unit (comb) dropped",
           variants={"r2": {"r": 0.2}, "r3": {"r": 0.3}, "r4": {"r": 0.4}},
           reason="clean/hybrid does as well on rate and better on noise in ticks (Actions run 20261116T060355Z: "
                  "e2e 0.912 vs 0.908 on rate, 0.818 vs 0.783 on noise); it leaves 1-sample spikes where a "
                  "line's unit is a few samples")
def relative(cap, r):
    out = []
    for ch in cap.channels:
        u, _ = comb_unit(runs(cap, ch), 12) if len(ch.edges) > 16 else (None, 0.0)
        k = int(r * u) if u else 0
        out.append(replace(ch, edges=K.deglitch(ch.edges, k)) if k >= 2 * ch.step else ch)
    return Capture(cap.rate, cap.n_samples, out)
