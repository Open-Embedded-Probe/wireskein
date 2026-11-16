"""A cosine comb over the rising-to-rising intervals: noise pulses split intervals at random and add about nothing."""

from wsbench import algorithm
from wsbench.kit import comb_unit, rising

import numpy as np


@algorithm("clock_period", "comb", status="trial",
           about="largest T the rising intervals are multiples of, by mean cos(2 pi iv / T)",
           variants={"m2": {"max_mult": 2}, "m4": {"max_mult": 4}})
def comb(cap, lines, max_mult):
    out = {}
    for n in lines:
        r = rising(cap.channel(n))
        T, s = comb_unit(np.diff(r).astype(np.float64), max_mult) if len(r) > 8 else (None, 0.0)
        out[n] = {"period_s": T / cap.rate if T else None, "confidence": s}
    return out
