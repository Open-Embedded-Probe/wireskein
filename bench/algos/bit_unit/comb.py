"""A cosine comb over the run lengths: every run of an asynchronous line is a whole number of bits."""

from wsbench import algorithm
from wsbench.kit import comb_unit, runs


@algorithm("bit_unit", "comb", status="trial", about="largest unit the runs are multiples of, by mean cos(2 pi run / u)")
def comb(cap, lines, max_mult=12):
    out = {}
    for n in lines:
        u, s = comb_unit(runs(cap, cap.channel(n)), max_mult)
        out[n] = {"unit_s": u / cap.rate if u else None, "confidence": s}
    return out
