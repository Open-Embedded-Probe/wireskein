"""timing.i2c_restore gives what its first form gave (one scan over every SCL edge per START-like change), without
that scan: a line read as SDA with tens of thousands of START-like changes (RVSWD's data line tried as I2C) took
tens of minutes."""

import time

import numpy as np

from wireskein._engine import gen, kernels as K, timing
from wireskein._engine.model import Capture, Channel


def reference(cap, scl, sda):
    """i2c_restore as first written (wireskein 0.0.28), with the long low no wait's (0.0.38: RESTORE_LOW_MAX): the
    ticks it restores."""
    s, d = cap.channel(scl), cap.channel(sda)
    e = np.asarray(s.edges, np.int64)
    de = np.asarray(d.edges, np.int64)
    if len(e) < 10 or len(de) < 2 or s.step != 1:
        return []
    lv = K.edge_levels(e, s.initial)
    lows = np.diff(e)[lv[:-1] == 0]
    usual = float(np.median(lows))
    sl, dl = s.level_at(de), d.level_at(de)
    starts, stops = de[(sl == 1) & (dl == 0)], de[(sl == 1) & (dl == 1)]
    add = []
    for a in starts:
        ends = [x for x in (stops[stops > a][:1].tolist() + starts[starts > a][:1].tolist())]
        if not ends:
            continue
        b = min(ends)
        inside = (e > a) & (e < b)
        rises = int(np.count_nonzero(inside & (lv == 1)))
        if rises < 9 or rises % 9:
            continue
        idx = np.flatnonzero(inside[:-1] & (lv[:-1] == 0))
        long = [i for i in idx if e[i + 1] - e[i] > 1.5 * usual]
        if len(long) != 1:
            continue
        i = long[0]
        if e[i + 1] - e[i] > timing.RESTORE_LOW_MAX * usual:
            continue
        at = int(e[i] + 0.9 * usual)
        if np.any((de > e[i] + 0.6 * usual) & (de <= at)):
            continue
        add.append(at)
    return add


def i2c_with_a_vanished_clock(seed):
    """An I2C capture with one clock of a byte taken out (as a slow rise leaves it), at 10 MS/s."""
    rng = np.random.default_rng(seed)
    scl, sda = gen.Wave(1), gen.Wave(1)
    tx = [{"addr": int(rng.integers(8, 120)), "rw": "write", "bytes": rng.integers(0, 256, 3).tolist()}
          for _ in range(4)]
    end = gen.i2c(scl, sda, 20e-6, tx, 100e3)
    cap = gen.quantize({"SCL": scl, "SDA": sda}, 10e6, end + 20e-6, rng)
    e = np.asarray(cap.channel("SCL").edges, np.int64)
    k = 2 * int(rng.integers(30, len(e) // 2 - 30))           # a rise and its fall: one clock gone
    e = np.delete(e, [k + 1, k + 2])
    chans = [Channel("SCL", c.initial, e, c.step, c.phase, c.acquisition) if c.name == "SCL" else c
             for c in cap.channels]
    return Capture(cap.rate, cap.n_samples, chans)


def random_lines(seed, n):
    """Two lines toggling at random: many START- and STOP-like changes."""
    rng = np.random.default_rng(seed)
    a = np.unique(rng.integers(1, 50 * n, n))
    b = np.unique(rng.integers(1, 50 * n, n))
    return Capture(1e7, 50 * n + 10, [Channel("SCL", 1, a), Channel("SDA", 1, b)])


def test_the_same_ticks_restored_as_before():
    for seed in range(20):
        cap = i2c_with_a_vanished_clock(seed)
        assert timing.i2c_restore(cap, "SCL", "SDA")[1] == reference(cap, "SCL", "SDA"), seed
    for seed in range(20):
        cap = random_lines(seed, 3000)
        assert timing.i2c_restore(cap, "SCL", "SDA")[1] == reference(cap, "SCL", "SDA"), seed


def test_many_start_like_changes_take_no_scan_each():
    cap = random_lines(1, 1_000_000)
    t = time.perf_counter()
    timing.i2c_restore(cap, "SCL", "SDA")
    assert time.perf_counter() - t < 5


def test_local_period_s_quartile_is_np_percentile_to_the_bit():
    from wireskein._engine.kernels import _percentile_rows
    rng = np.random.default_rng(0)
    for m in range(1, 10):
        for q in (0, 10, 25, 50, 75, 90, 100):
            x = rng.integers(1, 10**6, (2000, m)).astype(np.float64)
            assert np.array_equal(_percentile_rows(x, q), np.percentile(x, q, axis=1)), (m, q)
