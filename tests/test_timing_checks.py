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
    ok, got, why = _judge(cap, phase("A", "B", 90, phase_abs_deg=1, freq_hz=10e3))
    assert ok is True and abs(got["phase_deg"] - 90) < 0.5, why
    ok, _, why = _judge(cap, phase("A", "B", 0, phase_abs_deg=1))
    assert ok is False and "phase" in why
    assert _judge(cap, phase("A", "B"))[0] == verify.MEASURED


def test_a_phase_near_zero_wraps():
    cap = _cap({"A": _pwm(2e-6), "B": _pwm(2e-6 + 99e-6)}, 1e-3)           # 356.4 deg: 3.6 deg before
    assert _judge(cap, phase("A", "B", -4, phase_abs_deg=1))[0] is True
    assert _judge(cap, phase("A", "B", 356, phase_abs_deg=1))[0] is True


def test_skew_of_two_patterns():
    a, b = gen.Wave(0), gen.Wave(0)
    for i, t in enumerate([1e-6, 2e-6, 2.5e-6, 4e-6, 7e-6, 7.2e-6]):
        a.set(t, 1 - a.level)
        b.set(t + 50e-9, 1 - b.level)
    cap = _cap({"A": a, "B": b}, 10e-6)
    ok, got, why = _judge(cap, skew("A", "B", delay_abs_s=100e-9))
    assert ok is True and got["pairs"] == 6 and abs(got["median_s"] - 50e-9) < 1e-12, why
    ok, _, why = _judge(cap, skew("A", "B", delay_abs_s=10e-9))
    assert ok is False, why
    assert _judge(cap, skew("A", "B", delay_abs_s=30e-9, delay_s=50e-9))[0] is True
    # within the resolution: 50 ns +- 25 ns against 40 ns
    assert _judge(cap, skew("A", "B", delay_abs_s=40e-9))[0] is None


def test_a_section_bound_between_the_two_edges_of_a_pair_does_not_decide_the_skew():
    # two 20 kHz PWMs rising 100 ns apart; the capture starts between A's first rise (outside) and B's (inside)
    a, b = gen.Wave(0), gen.Wave(0)
    gen.pwm(a, 50e-6, 1e-3, 20e3, 0.5)
    gen.pwm(b, 0.1e-6, 1e-3, 20e3, 0.5)
    cap = _cap({"A": a, "B": b}, 1e-3)
    ok, got, why = _judge(cap, skew("A", "B", delay_abs_s=0.2e-6, edge="rise"))
    assert ok is True and got["unpaired_b"] == 0, why
    # a stray rise mid-capture still fails (paired 12 us off, or left without a partner)
    b2 = gen.Wave(0)
    gen.pwm(b2, 50.1e-6, 500e-6, 20e3, 0.5)
    b2.set(512e-6, 1)                                                # a rise of its own, mid-capture
    b2.set(513e-6, 0)
    gen.pwm(b2, 550.1e-6, 1e-3, 20e3, 0.5)
    ok, got, why = _judge(_cap({"A": a, "B": b2}, 1e-3), skew("A", "B", delay_abs_s=0.2e-6, edge="rise"))
    assert ok is False, why


def test_a_missing_edge_fails_the_skew():
    a, b = gen.Wave(0), gen.Wave(0)
    for t in [1e-6, 2e-6, 3e-6, 4e-6]:
        a.set(t, 1 - a.level)
    for t in [1e-6, 2e-6]:
        b.set(t, 1 - b.level)
    ok, got, why = _judge(_cap({"A": a, "B": b}, 6e-6), skew("A", "B", delay_abs_s=50e-9))
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
    ok, got, why = _judge(cap, dead_time("HI", "LO", dead_min_s=400e-9))
    assert ok is True, why
    assert abs(got["hi_to_lo"]["min_s"] - 500e-9) < 30e-9 and abs(got["lo_to_hi"]["min_s"] - 1e-6) < 30e-9
    ok, _, why = _judge(cap, dead_time("HI", "LO", dead_min_s=600e-9))
    assert ok is False and "hi_to_lo" in why and "lo_to_hi" not in why
    assert _judge(cap, dead_time("HI", "LO"))[0] == verify.MEASURED


def test_an_asymmetric_dead_time_has_a_limit_each_way():
    cap = _cap(_bridge(500e-9, 1e-6), 1e-3)                          # hi off -> lo on 500 ns, lo off -> hi on 1 us
    ok, got, why = _judge(cap, dead_time("HI", "LO", hi_to_lo_min_s=400e-9, hi_to_lo_max_s=600e-9,
                                         lo_to_hi_min_s=900e-9, lo_to_hi_max_s=1.1e-6))
    assert ok is True, why
    assert abs(got["hi_to_lo"]["max_s"] - 500e-9) < 30e-9 and abs(got["lo_to_hi"]["max_s"] - 1e-6) < 30e-9
    ok, _, why = _judge(cap, dead_time("HI", "LO", hi_to_lo_min_s=400e-9, lo_to_hi_max_s=800e-9))
    assert ok is False and "lo_to_hi" in why and "hi_to_lo" not in why
    ok, _, why = _judge(cap, dead_time("HI", "LO", hi_to_lo_min_s=600e-9))
    assert ok is False and "hi_to_lo" in why
    # the limit for both ways still applies beside one way's own
    ok, _, why = _judge(cap, dead_time("HI", "LO", dead_min_s=700e-9, lo_to_hi_min_s=900e-9))
    assert ok is False and "hi_to_lo" in why and "lo_to_hi" not in why
    assert "lo_to_hi_min_s" not in dead_time("HI", "LO")


def test_shoot_through_fails():
    cap = _cap(_bridge(-200e-9, 500e-9), 1e-3)                       # LO turns on before HI is off
    ok, got, why = _judge(cap, dead_time("HI", "LO", dead_min_s=0))
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
    assert _judge(cap, i2c_timing("SCL", "SDA", "fast", limits={"t_vd_dat_max_s": 3e-6}))[0] is True
    ok, _, why = _judge(_i2c(500e3), i2c_timing("SCL", "SDA", "fast"))
    assert ok is False and "kHz" in why


def test_an_i2c_time_within_the_resolution_is_unchecked():
    cap = _i2c(100e3, rate=1e6)                     # 1 us a tick: the shortest low reads 4 us, so 3 to 5 us
    ok, _, why = _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low_min_s": 4.7e-6}))
    assert ok is None and "t_low" in why
    assert _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low_min_s": 2.5e-6}))[0] is True
    assert _judge(cap, i2c_timing("SCL", "SDA", None, limits={"t_low_min_s": 5.5e-6}))[0] is False


@pytest.mark.parametrize("mode", [0, 1, 2, 3])
def test_the_spi_mode_from_the_lines(mode):
    clk, mosi, miso, cs = gen.Wave(mode >> 1), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, miso, cs, 5e-6, [(bytes([0x9F, 0x00, 0x5A]), bytes([0x00, 0xEF, 0x40]))] * 3, 1e6,
                  mode=mode)
    cap = _cap({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, end + 5e-6, 20e6)
    ok, got, why = _judge(cap, spi("SCK", "MOSI", "MISO", "CS", mode=mode, mosi_bytes="9f005a" * 3))
    assert ok is True and (got["cpol"], got["cpha"]) == (mode >> 1, mode & 1), (why, got)
    # the other CPHA passes only when it reads the bytes too (the generator leaves a sample around both edges)
    other = mode ^ 1
    ok, got, why = _judge(cap, spi("SCK", "MOSI", "MISO", "CS", mode=other, mosi_bytes="9f005a" * 3))
    assert ok is (other in got["mode_candidates"]), (why, got["mode_candidates"])
    if ok is False:
        assert f"mode {other} does not read it" in why
    # a mode of the other CPOL never reads it
    ok, got, why = _judge(cap, spi("SCK", "MOSI", "MISO", "CS", mode=mode ^ 2))
    assert ok is False and "does not read it" in why


def test_the_spi_cpol_without_cs_is_the_level_sck_rests_at_around_the_transfers():
    # shiftOut / shiftIn bit-banged (mode 0, no CS): SCK rests low before and after, but between bytes it rests
    # low for less than it is held high on some bits (the longest pause inside is a high one)
    clk, mosi = gen.Wave(0), gen.Wave(0)
    t, rng = 5e-6, np.random.default_rng(1)
    for byte in b"\x3c\x9e\xa4":
        for i in range(8):
            mosi.set(t, (byte >> (7 - i)) & 1)            # data out while SCK is low
            t += 0.4e-6
            clk.set(t, 1)
            t += 0.9e-6 + (1.5e-6 if rng.random() < 0.3 else 0.0)   # held high, now and then longer
            clk.set(t, 0)
            t += 0.4e-6
        t += 0.6e-6                                     # the pause between bytes, shorter than a long high
    cap = _cap({"SCK": clk, "MOSI": mosi}, t + 5e-6, 20e6)
    ok, got, why = _judge(cap, spi("SCK", "MOSI", None, None, mode=0, mosi_bytes="3c9ea4"))
    assert ok is True and (got["cpol"], got["cpha"], got["mode"]) == (0, 0, 0), (why, got)


def test_an_spi_data_line_of_few_changes_without_cs_is_read_as_named():
    # shiftOut LSB first of f0 and then ff ff ff ff ff (MOSI held high: a line of two changes), shiftIn on MISO,
    # no CS: the test names the lines and the mode, so MOSI is read at the mode's sampling edge even where the
    # decoder's own reading takes MISO alone
    clk, mosi, miso = gen.Wave(0), gen.Wave(1), gen.Wave(0)
    out = bytes([0xF0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
    back = bytes([0x48, 0x53, 0x6B, 0xF2, 0x57, 0x06])
    t = 5e-6
    for o, i in zip(out, back):
        for k in range(8):                              # LSB first
            mosi.set(t, (o >> k) & 1)
            miso.set(t, (i >> k) & 1)
            t += 2.9e-6
            clk.set(t, 1)
            t += 2.9e-6
            clk.set(t, 0)
        t += 20e-6
    cap = _cap({"SCK": clk, "MOSI": mosi, "MISO": miso}, t + 5e-6, 20e6)
    x = spi("SCK", "MOSI", "MISO", None, mode=0, mosi_bytes="f0ffffffffff", miso_bytes="48536bf25706",
            bit_order="lsb")
    ok, got, why = _judge(cap, x)
    assert ok is True and got["bytes"]["MOSI"] == "f0ffffffffff", (why, got.get("bytes"))
