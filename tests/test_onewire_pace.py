"""A 1-Wire master starts its slots at one pace: a DMX512 line read at overdrive (its break a reset, its start code's
low a presence, its lows slots, bounces 1s) has slots whole bits apart, scattered, and no 1-Wire claim of weight; a
real 1-Wire's slots keep their pace. Lines made here (gen)."""

from types import SimpleNamespace

import numpy as np

from wireskein._engine import gen
from wireskein._engine.analyzers.pulse import OneWirePlugin

RATE = 4e6


def score(w: gen.Wave, t1: float, bounce: bool = False) -> float:
    cap = gen.quantize({"D0": w}, RATE, t1, np.random.default_rng(0))
    ch = cap.channel("D0")
    if bounce:                                       # a pulse back of one tick after every third edge
        e = np.asarray(ch.edges, np.int64)
        extra = e[::3] + 2
        ch = type(ch)(ch.name, ch.initial, np.sort(np.concatenate([e, extra, extra + 1])))
    nodes = OneWirePlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=RATE)), ch, None)
    return nodes[0].total if nodes else 0.0


def test_dmx_with_bounces_is_no_onewire():
    # a seed whose line read as 1-Wire at 0.69 (likely) before the pace was weighed
    r = np.random.default_rng(23)
    w = gen.Wave(1)
    t = gen.dmx(w, 2e-4, [bytes(r.integers(0, 256, int(r.integers(8, 40))).tolist()) for _ in range(int(r.integers(2, 6)))])
    assert score(w, t + 2e-4, bounce=True) < 0.3


def test_real_onewire_keeps_its_pace():
    r = np.random.default_rng(3)
    w, t = gen.Wave(1), 2e-4
    for _ in range(4):
        t = gen.onewire(w, t, bytes(r.integers(0, 256, 6).tolist())) + 5e-4
    assert score(w, t) > 0.85


def test_spiked_onewire_is_read():
    """Spikes of a tick (shorter than the shortest 1-Wire pulse, 1 us) read as slots of their own or split lows: the
    line is read with them taken out too."""
    r = np.random.default_rng(3)
    w, t = gen.Wave(1), 2e-4
    for _ in range(4):
        t = gen.onewire(w, t, bytes(r.integers(0, 256, 6).tolist())) + 5e-4
    cap = gen.quantize({"D0": w}, RATE, t, np.random.default_rng(0))
    ch = cap.channel("D0")
    s = np.sort(np.random.default_rng(5).integers(10, cap.n_samples - 10, 12))
    ch = type(ch)(ch.name, ch.initial, np.sort(np.concatenate([np.asarray(ch.edges, np.int64), s, s + 1])))
    nodes = OneWirePlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=RATE)), ch, None)
    assert nodes and nodes[0].total > 0.85 and nodes[0].params["deglitch"] >= 2
