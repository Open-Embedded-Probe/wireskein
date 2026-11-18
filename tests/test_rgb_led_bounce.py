"""A WS2812-like line whose edges bounce (a pulse back of a tick or two after an edge, shorter than any pulse of the
line) is read with those pulses taken out: as it is, every bounce is a bit of its own and no byte is whole. Lines made
here (gen.rgb_led)."""

from types import SimpleNamespace

import numpy as np

from wireskein._engine import gen
from wireskein._engine.analyzers.pulse import RgbLedPlugin

RATE = 50e6


def test_bouncing_led_line_is_read():
    r = np.random.default_rng(4)
    w, t, frames = gen.Wave(0), 1e-4, []
    for _ in range(4):
        data = bytes(r.integers(0, 256, 12).tolist())
        frames.append(data.hex())
        t = gen.rgb_led(w, t, data, reset=100e-6)
    cap = gen.quantize({"D0": w}, RATE, t + 1e-4, np.random.default_rng(0))
    ch = cap.channel("D0")
    e = np.asarray(ch.edges, np.int64)
    b = e[::4] + 2                                       # every fourth edge bounces
    ch = type(ch)(ch.name, ch.initial, np.sort(np.concatenate([e, b, b + 1])))
    nodes = RgbLedPlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=RATE)), ch, None)
    assert nodes and nodes[0].total > 0.85 and nodes[0].params["deglitch"] >= 2
    assert [f["data"] for f in nodes[0].output.items["frames"]] == frames


def test_a_clock_of_half_duty_is_no_led():
    """A clock's highs are half its period: all one kind of bit, not split at half the period into varied bits (three
    SPI clocks with bounces were confirmed as LED strings so)."""
    w, t = gen.Wave(0), 1e-4
    for _ in range(6):
        gen.pwm(w, t, t + 40 / 0.5e6, 0.5e6, 0.5)
        t += 40 / 0.5e6 + 200e-6
    cap = gen.quantize({"D0": w}, RATE, t, np.random.default_rng(0), jitter_s=0.5 / RATE)   # highs 49-51 ticks
    nodes = RgbLedPlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=RATE)),
                               cap.channel("D0"), None)
    assert not nodes or nodes[0].total < 0.6             # 0.66 when split at half the period


def test_under_five_samples_a_bit_is_no_led():
    """A 500 kHz clock taken at 2 MS/s (4 samples a bit) whose spikes make its highs 1 to 3 ticks: no LED string, a
    tick is as wide as T0H and T1H differ there."""
    r = np.random.default_rng(6)
    w, t = gen.Wave(0), 1e-4
    for _ in range(10):
        gen.pwm(w, t, t + 24 / 0.5e6, 0.5e6, 0.5)
        t += 24 / 0.5e6 + 300e-6
    rate = 2e6
    cap = gen.quantize({"D0": w}, rate, t, np.random.default_rng(0))
    ch = cap.channel("D0")
    s = np.sort(r.integers(5, cap.n_samples - 5, 30))
    ch = type(ch)(ch.name, ch.initial, np.unique(np.concatenate([np.asarray(ch.edges, np.int64), s, s + 1])))
    nodes = RgbLedPlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=rate)), ch, None)
    assert not nodes or nodes[0].total < 0.3
