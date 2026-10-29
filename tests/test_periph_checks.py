"""LIN, I2S and a quadrature encoder read from their lines."""

import numpy as np
import pytest

from wireskein import verify
from wireskein._engine import gen
from wireskein.runlog import i2s, lin, quadrature


def _judge(cap, x):
    return verify._judge(cap, x, verify._pins_of(x))


def _lin(frames, baud=19200, break_bits=14):
    w = gen.Wave(1)
    t = 100e-6
    for ident, data in frames:
        w.set(t, 0)
        t += break_bits / baud
        w.set(t, 1)
        t += 1.5 / baud
        pid = gen.lin_pid(ident)
        cs = gen.lin_checksum(data, None if ident in (0x3C, 0x3D) else pid)
        t = gen.uart(w, t, bytes([0x55, pid]) + data + bytes([cs]), baud, gap_bits=0.5) + 20 / baud
    return gen.quantize({"L": w}, 2e6, t, np.random.default_rng(0))


def test_lin_frames_breaks_and_checksums():
    cap = _lin([(0x10, bytes([1, 2, 3, 4])), (0x3C, bytes(8))])
    want = [{"id": 0x10, "data": "01020304"}, {"id": 0x3C, "data": "00" * 8}]
    ok, got, why = _judge(cap, lin("L", 19200, want))
    assert ok is True, why
    assert got["checksum_model"] == ["enhanced", "classic"] and got["break_bits"][0] == pytest.approx(14, abs=0.1)
    ok, _, why = _judge(cap, lin("L", 19200, want, checksum="enhanced"))
    assert ok is False and "frame #1" in why and "enhanced" in why


def test_a_short_lin_break_and_a_slow_master():
    ok, _, why = _judge(_lin([(0x10, b"\x01")], break_bits=11), lin("L", 19200))
    assert ok is False and why.startswith("break 573 us < 677.1 us"), why
    ok, _, why = _judge(_lin([(0x10, b"\x01")], baud=18800), lin("L", 19200, tol_baud=0.01))
    assert ok is False and "baud" in why, why


@pytest.mark.parametrize("fmt", ["i2s", "left", "right"])
def test_i2s_samples(fmt):
    b, w, d = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    frames = [(i * 1000 - 3000, -i * 7 + 5) for i in range(10)]
    t = gen.i2s(b, w, d, 10e-6, frames, 48e3, 32, 24, fmt)
    cap = gen.quantize({"BCLK": b, "WS": w, "SD": d}, 50e6, t + 10e-6, np.random.default_rng(0))
    ok, got, why = _judge(cap, i2s("BCLK", "WS", "SD", fmt, 24, fs_hz=48e3, slot_bits=32, samples=frames[2:6]))
    assert ok is True, why
    other = {"i2s": "left", "left": "i2s", "right": "left"}[fmt]
    ok, _, why = _judge(cap, i2s("BCLK", "WS", "SD", other, 24, samples=frames[2:6]))
    assert ok is False and "samples" in why


def test_quadrature_count_and_direction():
    a, b = gen.Wave(0), gen.Wave(0)
    t = gen.quadrature(a, b, 1e-6, [40, -12, 8], 2e-6)
    cap = gen.quantize({"A": a, "B": b}, 10e6, t + 1e-6, np.random.default_rng(0))
    ok, got, why = _judge(cap, quadrature("A", "B", steps=36, cycles=9))
    assert ok is True and got["reversals"] == 2, why
    ok, _, why = _judge(cap, quadrature("A", "B", direction="forward"))
    assert ok is False and "12 steps backward" in why
    # both lines on one tick: sampled too slowly for the encoder
    cap = gen.quantize({"A": a, "B": b}, 0.4e6, t + 1e-6, np.random.default_rng(0))
    ok, got, why = _judge(cap, quadrature("A", "B"))
    assert ok is False and got["errors"] > 0 and "at once" in why
