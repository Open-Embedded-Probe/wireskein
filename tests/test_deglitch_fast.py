"""kernels.deglitch gives what its first form gave (one pass over every edge), reading only the runs of edges closer
than k one by one."""

import numpy as np

from wireskein._engine import kernels as K


def reference(edges, k, allow=None):
    out = []
    ok = allow.tolist() if allow is not None else None
    for i, x in enumerate(np.asarray(edges).tolist()):
        if out and x - out[-1] < k and (ok is None or ok[i]):
            out.pop()
        else:
            out.append(x)
    return np.asarray(out, dtype=np.int64)


def test_the_same_edges_as_one_pass():
    rng = np.random.default_rng(0)
    for seed in range(300):
        r = np.random.default_rng(seed)
        n = int(r.integers(0, 400))
        e = np.cumsum(r.integers(1, 12, n)).astype(np.int64)
        k = int(r.integers(1, 8))
        allow = r.random(n) < 0.7 if seed % 2 else None
        assert np.array_equal(K.deglitch(e, k, allow), reference(e, k, allow)), seed


def test_no_short_pulse_is_a_copy():
    e = np.arange(0, 1000, 10, dtype=np.int64)
    out = K.deglitch(e, 5)
    assert np.array_equal(out, e) and out is not e
