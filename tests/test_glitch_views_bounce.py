"""A clock whose edges bounce at half of them (most of its runs a bounce's ticks) still gets the views with the
pulses taken out, up to 5 ticks: its typical run is taken also with the bounces out, and where it is slow enough is
judged from its periods with them out. Lines made here."""

from types import SimpleNamespace

import numpy as np

from wireskein._engine import gen
from wireskein._engine.model import Capture
from wireskein._engine.staged import glitch_views


def test_bouncing_clock_gets_its_views():
    clk = gen.Wave(0)
    gen.pwm(clk, 1e-5, 1e-5 + 400 / 1e5, 1e5, 0.5)
    cap = gen.quantize({"SCK": clk}, 2e8, 1e-5 + 400 / 1e5 + 1e-5, np.random.default_rng(0))
    ch = cap.channel("SCK")
    e = np.asarray(ch.edges, np.int64)
    r = np.random.default_rng(1)
    sel = e[r.random(len(e)) < 0.5]
    off = r.integers(1, 4, len(sel))
    ch = type(ch)(ch.name, ch.initial, np.unique(np.concatenate([e, sel + off, sel + off + r.integers(1, 4, len(sel))])))
    cap = Capture(cap.rate, cap.n_samples, [ch])
    views = list(glitch_views(cap, None, SimpleNamespace(clock="SCK", data=(), select=None)))
    ks = [k for _, k in views]
    assert max(ks) >= 5
    cleaned = dict((k, v) for v, k in views)[max(ks)].channel("SCK").edges
    assert len(cleaned) == len(e)                        # every bounce gone, every edge of the clock kept
