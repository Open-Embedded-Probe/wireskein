"""Captures of the shapes that make analysis heavy, at a given number of changes (for tests/test_complexity.py)."""
import numpy as np

from wireskein._engine import gen

RATE = 10e6


def random_line(n, rng):
    w = gen.Wave(1)
    rt = 50e3
    gen.random_toggles(w, 0, n / rt, rt, rng)
    return {"R": w}, n / rt


def clock_and_data(n, rng):
    clk, d = gen.Wave(0), gen.Wave(0)
    f = 500e3
    t1 = n / (2 * f)
    gen.pwm(clk, 0, t1, f, 0.5)
    gen.random_toggles(d, 0, t1, f / 4, rng)
    return {"CLK": clk, "DAT": d}, t1


def uart_text(n, rng):
    w = gen.Wave(1)
    data = bytes(rng.integers(0, 256, max(1, n // 5)).tolist())
    t1 = gen.uart(w, 1e-4, data, 115200)
    return {"TX": w}, t1


def uart_gappy(n, rng):
    """Characters with short rests between (1.6 to 3 characters: under a Modbus RTU frame's gap, over a character's)."""
    w = gen.Wave(1)
    t, bt = 1e-4, 1 / 115200
    while len(w.times) < n:
        t = gen.uart(w, t, bytes(rng.integers(0, 256, int(rng.integers(1, 4))).tolist()), 115200)
        t += float(rng.uniform(1.6, 3.0)) * 10 * bt
    return {"TX": w}, t


def pulse_line(n, rng):
    w = gen.Wave(0)
    t = 1e-4
    while len(w.times) < n:
        t = gen.rgb_led(w, t, bytes(rng.integers(0, 256, 30).tolist()), reset=60e-6)
    return {"LED": w}, t


def opposite_pair(n, rng):
    a = gen.Wave(1)
    rt = 50e3
    gen.random_toggles(a, 0, n / rt, rt, rng)
    b = gen.Wave(0, times=list(a.times))
    b.level = a.level ^ 1
    return {"P": a, "N": b}, n / rt


SHAPES = {"random_line": random_line, "clock_and_data": clock_and_data, "uart_text": uart_text, "uart_gappy": uart_gappy,
          "pulse_line": pulse_line, "opposite_pair": opposite_pair}


def capture(shape, n, seed=0):
    rng = np.random.default_rng(seed)
    waves, t1 = SHAPES[shape](n, rng)
    return gen.quantize(waves, RATE, t1 + 1e-4, np.random.default_rng(seed + 1))
