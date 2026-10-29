"""Times between lines: phase, skew, dead time, I2C's per-bit times (UM10204) and the SPI mode read from the lines."""

import numpy as np
import pytest

from wireskein import verify
from wireskein._engine import gen
from wireskein.runlog import dead_time, i2c_timing, phase, skew, spi

RATE = 40e6                     # 25 ns a tick


def _cap(waves, end, rate=RATE):
    return gen.quantize(waves, rate, end, np.random.default_rng(0))


def _judge(cap, x):
    ok, got, why = verify._judge(cap, x, verify._pins_of(x))
    return ok, got, why


def _pwm(t0, freq=10e3, duty=0.5, end=1e-3):
    w = gen.Wave(0)
    gen.pwm(w, t0, end, freq, duty)
    return w


def test_the_phase_of_two_pwms():
    cap = _cap({"A": _pwm(1e-6), "B": _pwm(1e-6 + 25e-6)}, 1e-3)          # a quarter of 100 us
    ok, got, why = _judge(cap, phase("A", "B", 90, tol_deg=1, freq_hz=10e3))
    assert ok is True and abs(got["phase_deg"] - 90) < 0.5, why
    ok, _, why = _judge(cap, phase("A", "B", 0, tol_deg=1))
    assert ok is False and "phase" in why
    assert _judge(cap, phase("A", "B"))[0] == verify.MEASURED


def test_a_phase_near_zero_wraps():
    cap = _cap({"A": _pwm(2e-6), "B": _pwm(2e-6 + 99e-6)}, 1e-3)           # 356.4 deg: 3.6 deg before
    assert _judge(cap, phase("A", "B", -4, tol_deg=1))[0] is True
    assert _judge(cap, phase("A", "B", 356, tol_deg=1))[0] is True


def test_skew_of_two_patterns():
    a, b = gen.Wave(0), gen.Wave(0)
    for i, t in enumerate([1e-6, 2e-6, 2.5e-6, 4e-6, 7e-6, 7.2e-6]):
        a.set(t, 1 - a.level)
        b.set(t + 50e-9, 1 - b.level)
    cap = _cap({"A": a, "B": b}, 10e-6)
    ok, got, why = _judge(cap, skew("A", "B", tol_s=100e-9))
    assert ok is True and got["pairs"] == 6 and abs(got["median_s"] - 50e-9) < 1e-12, why
    ok, _, why = _judge(cap, skew("A", "B", tol_s=10e-9))
    assert ok is False, why
    assert _judge(cap, skew("A", "B", tol_s=30e-9, delay_s=50e-9))[0] is True
    # within the resolution: 50 ns +- 25 ns against 40 ns
    assert _judge(cap, skew("A", "B", tol_s=40e-9))[0] is None


def test_a_missing_edge_fails_the_skew():
    a, b = gen.Wave(0), gen.Wave(0)
    for t in [1e-6, 2e-6, 3e-6, 4e-6]:
        a.set(t, 1 - a.level)
    for t in [1e-6, 2e-6]:
        b.set(t, 1 - b.level)
    ok, got, why = _judge(_cap({"A": a, "B": b}, 6e-6), skew("A", "B", tol_s=50e-9))
    assert ok is False and got["unpaired_a"] == 2 and "without a partner" in why


def _bridge(dead_hl, dead_lh, freq=20e3, duty=0.5, end=1e-3):
    hi, lo = gen.Wave(0), gen.Wave(1)
    p, t = 1 / freq, 1e-6
    lo.set(t - dead_lh, 0)
    while t + p < end:
        hi.set(t, 1)
        hi.set(t + duty * p, 0)
        lo.set(t + duty * p + dead_hl, 1)
        lo.set(t + p - dead_lh, 0)
        t += p
    return {"HI": hi, "LO": lo}


def test_the_dead_time_of_a_half_bridge():
    cap = _cap(_bridge(500e-9, 1e-6), 1e-3)
    ok, got, why = _judge(cap, dead_time("HI", "LO", min_s=400e-9))
    assert ok is True, why
    assert abs(got["hi_to_lo"]["min_s"] - 500e-9) < 30e-9 and abs(got["lo_to_hi"]["min_s"] - 1e-6) < 30e-9
    ok, _, why = _judge(cap, dead_time("HI", "LO", min_s=600e-9))
    assert ok is False and "hi_to_lo" in why and "lo_to_hi" not in why
    assert _judge(cap, dead_time("HI", "LO"))[0] == verify.MEASURED


def test_shoot_through_fails():
    cap = _cap(_bridge(-200e-9, 500e-9), 1e-3)                       # LO turns on before HI is off
    ok, got, why = _judge(cap, dead_time("HI", "LO", min_s=0))
    assert ok is False and "both on" in why and got["overlaps"]["count"] > 10


def _i2c(freq, rate=RATE):
    scl, sda = gen.Wave(1), gen.Wave(1)
    end = gen.i2c(scl, sda, 20e-6, [{"addr": 0x48, "rw": "write", "bytes": [0x10], "repeated_start": True},
                                    {"addr": 0x48, "rw": "read", "bytes": [0xAB]},
                                    {"addr": 0x50, "rw": "write", "bytes": [0x01]}], freq)
    return _cap({"SCL": scl, "SDA": sda}, end + 20e-6, rate)


def test_i2c_times_against_um10204():
    # gen.i2c puts every edge a quarter bit apart: at 100 kHz, 2.5 us holds and setups, 5 us low and high
    cap = _i2c(100e3)
    ok, got, why = _judge(cap, i2c_timing("SCL", "SDA", "standard"))
    assert ok is False
    for k in ("t_hd_sta", "t_su_sta", "t_su_sto"):
        assert k in why
    for k in ("t_low ", "t_high", "t_su_dat", "t_buf", "SCL"):
        assert k not in why
    assert got["t_low"]["min_s"] == pytest.approx(5e-6, abs=30e-9) and got["t_su_sta"]["count"] == 1
    assert got["t_buf"]["count"] == 1 and got["f_scl"]["median_hz"] == pytest.approx(100e3, rel=0.01)
    # fast mode's minimums are met at 100 kHz, but the data is valid 2.5 us after SCL falls, not within 0.9 us
    ok, _, why = _judge(cap, i2c_timing("SCL", "SDA", "fast"))
    assert ok is False and why.startswith("t_vd_dat 2.5 us > 900 ns") and ";" not in why, why
    assert _judge(cap, i2c_timing("SCL", "SDA", "fast", limits={"t_vd_dat": (None, 3e-6)}))[0] is True
    ok, _, why = _judge(_i2c(500e3), i2c_timing("SCL", "SDA", "fast"))
    assert ok is False and "kHz" in why


def test_an_i2c_time_within_the_resolution_is_unchecked():
    cap = _i2c(100e3, rate=1e6)                     # 1 us a tick: the shortest low reads 4 us, so 3 to 5 us
    ok, _, why = _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low": 4.7e-6}))
    assert ok is None and "t_low" in why
    assert _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low": 2.5e-6}))[0] is True
    assert _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low": 5.5e-6}))[0] is False


@pytest.mark.parametrize("mode", [0, 1, 2, 3])
def test_the_spi_mode_from_the_lines(mode):
    clk, mosi, miso, cs = gen.Wave(mode >> 1), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, miso, cs, 5e-6, [(bytes([0x9F, 0x00, 0x5A]), bytes([0x00, 0xEF, 0x40]))] * 3, 1e6,
                  mode=mode)
    cap = _cap({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, end + 5e-6, 20e6)
    ok, got, why = _judge(cap, spi("SCK", "MOSI", "MISO", "CS", mode=mode, mosi_bytes="9f005a" * 3))
    assert ok is True and (got["cpol"], got["cpha"]) == (mode >> 1, mode & 1), (why, got)
    ok, got, why = _judge(cap, spi("SCK", "MOSI", "MISO", "CS", mode=mode ^ 1))
    assert ok is False and "mode" in why
