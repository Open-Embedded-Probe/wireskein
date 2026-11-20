"""A clock rebuilt from its period (views.REBUILDS): bounces as long as its half period, one cancelling the clock's next
change, are no width a filter tells from the clock's own runs; the period does."""
import numpy as np

from wireskein._engine import views as V


def _clock(n_bursts=6, per_burst=64, half=6.0, gap=400, start=100):
    """Bursts of a clock of a fractional half period (6.1 ticks: runs of 6 and 7), the line low between them."""
    out, t = [], start
    for _ in range(n_bursts):
        out += [int(round(t + k * half)) for k in range(2 * per_burst)]
        t = out[-1] + gap
    return np.asarray(out, np.int64)


def _bounced(e, rng, share=0.1):
    """At a share of the edges a pulse back 1-3 ticks wide within 4 ticks after it (the bench's bounce:L): with a
    half period of 6, some reach the next change and cancel it."""
    sel = e[rng.random(len(e)) < share]
    a = sel + rng.integers(1, 5, len(sel))
    extra = np.concatenate([a, a + rng.integers(1, 4, len(sel))])
    u, c = np.unique(np.concatenate([e, extra]), return_counts=True)
    return u[c % 2 == 1]


def test_a_bounced_clock_is_rebuilt_to_its_changes():
    clean = _clock(half=6.1)
    noisy = _bounced(clean, np.random.default_rng(3))
    assert len(noisy) != len(clean)
    best = 0.0
    for _, rebuild in V.REBUILDS:
        got = rebuild(noisy, 0, int(clean[-1]) + 1000)
        assert len(got) % 2 == 0                      # each burst ends low, as the line does
        if len(got) == len(clean):
            best = max(best, float(np.mean(np.abs(got - clean) <= 2)))
    assert best >= 0.98                               # one way at least: every change, within 2 ticks


def test_a_clean_clock_comes_back_as_it_is():
    clean = _clock(half=8.0)
    for _, rebuild in V.REBUILDS:
        assert np.array_equal(rebuild(clean, 0, int(clean[-1]) + 1000), clean)


def test_a_line_too_short_or_too_fast_is_left_alone():
    assert np.array_equal(V.clock_gridded(np.array([5, 9, 30]), 0, 100), [5, 9, 30])
    fast = np.arange(10, 200, 1, dtype=np.int64)       # runs of one tick: no half period to rebuild from
    for _, rebuild in V.REBUILDS:
        assert np.array_equal(rebuild(fast, 0, 300), fast)
