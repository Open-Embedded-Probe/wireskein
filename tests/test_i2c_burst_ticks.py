"""timing.i2c_burst_ticks' upper quartiles around each of SCL's rises, from one sort: the same values as one
np.quantile per rise (which took minutes on a fast clock's million edges), at the ends of the line too."""

import numpy as np

from wireskein._engine import gen, timing
from wireskein._engine.model import Capture, Channel


def _one_by_one(d):
    return np.array([np.quantile(d[max(0, i - 4):i + 5], 0.75) for i in range(len(d))])


def test_the_upper_quartiles_are_np_quantile_s():
    rng = np.random.default_rng(1)
    for n in (0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 17, 300):
        for d in (rng.integers(1, 60, n).astype(np.float64), rng.choice([1.0, 2.0, 3.0, 40.0], n),
                  rng.uniform(0.5, 9.5, n)):
            assert np.array_equal(timing.upper_quartiles_around(d), _one_by_one(d)), n


def _burst_ticks_one_by_one(cap, scl, at):
    """i2c_burst_ticks as it was, a quantile per rise."""
    from wireskein._engine import kernels as K
    tr = timing.I2C_RISE_S * float(cap.rate)
    ch = cap.channel(scl)
    e = np.asarray(ch.edges, np.int64)
    rises = e[K.edge_levels(e, ch.initial) == 1]
    d = np.diff(rises).astype(np.float64)
    keep = [int(rises[0])]
    for i, r in enumerate(rises[1:].tolist()):
        if r - keep[-1] >= float(np.quantile(d[max(0, i - 4):i + 5], 0.75)) / 4:
            keep.append(r)
    return np.minimum(K.local_period(np.asarray(keep, np.int64), np.asarray(at, np.int64)) / 6, tr).astype(np.int64)


def test_burst_ticks_on_a_bouncing_clock_as_before():
    rate = 100e6
    w = gen.Wave(0)
    gen.pwm(w, 0, 2e-3, 100e3, 0.5)
    cap = gen.quantize({"SCL": w}, rate, 2e-3, np.random.default_rng(0))
    rng = np.random.default_rng(2)
    e = np.asarray(cap.channel("SCL").edges, np.int64)
    # bounces: a pulse back a few ticks after a third of the edges
    sel = e[rng.random(len(e)) < 0.3]
    extra = np.concatenate([sel + 3, sel + 3 + rng.integers(1, 6, len(sel))])
    u, c = np.unique(np.concatenate([e, extra]), return_counts=True)
    ch = Channel("SCL", cap.channel("SCL").initial, u[c % 2 == 1])
    cap = Capture(cap.rate, cap.n_samples, [ch])
    at = ch.edges[::7]
    assert np.array_equal(timing.i2c_burst_ticks(cap, "SCL", at=at), _burst_ticks_one_by_one(cap, "SCL", at))
