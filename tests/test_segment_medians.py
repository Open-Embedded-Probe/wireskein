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
