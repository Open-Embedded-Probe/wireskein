"""kernels.segment_medians gives np.median of each segment, the same float (the reframing of RVSWD relies on it to keep
its output to the byte)."""

import numpy as np

from wireskein._engine import kernels as K


def test_the_same_as_np_median():
    rng = np.random.default_rng(0)
    for _ in range(300):
        v = rng.integers(0, 60, 400).astype(np.float64) * rng.choice([1.0, 0.37])
        s = np.sort(rng.integers(0, 399, 25))
        e = np.minimum(s + rng.integers(1, 40, 25), 400)
        got = K.segment_medians(v, s, e)
        assert np.array_equal(got, [np.median(v[a:b]) for a, b in zip(s, e)])


def test_none():
    assert len(K.segment_medians(np.arange(5.0), [], [])) == 0


def test_rolling_median_the_same_as_numpy():
    from numpy.lib.stride_tricks import sliding_window_view
    rng = np.random.default_rng(1)
    for _ in range(300):
        n = int(rng.integers(1, 400))
        x = rng.integers(0, int(rng.integers(1, 120)), n).astype(np.float64) * rng.choice([1.0, 0.5])
        for w in (1, 3, 5, 33):
            if n >= w:
                assert np.array_equal(K.rolling_median(x, w), np.median(sliding_window_view(x, w), axis=1))
