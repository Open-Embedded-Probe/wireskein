"""Checks on analog channels: voltage levels, and the logic checks on a line
captured by an ADC (threshold=, with hysteresis)."""

from fractions import Fraction

import numpy as np
import pytest

from wireskein import fileformat
from wireskein._engine.analog import schmitt, to_logic
from wireskein._engine.model import AnalogTrace
from wireskein.runlog import Recorder, level, pulses, square, uart, voltage
from wireskein.verify import verify

RNG = np.random.default_rng(7)


def run(tmp_path, expect, channels, tick_hz=1_000_000):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=expect):
        rec.capture(rec.armed(), tick_hz, channels=channels)
    rec.close()
    return [(r["check"], r["ok"], r["reason"]) for r in verify(tmp_path)["results"]]


def pwm_volts(n, rate, freq, duty, lo=0.0, hi=3.3, noise=0.02):
    t = np.arange(n) / rate
    return np.where((t * freq) % 1 < duty, hi, lo) + RNG.normal(0, noise, n)


def test_voltage_mean_range_and_ripple(tmp_path):
    v = 3.30 + RNG.normal(0, 0.01, 2000)
    chans = [fileformat.analog_volts("VBUS", v.tolist(), 100_000)]
    got = run(tmp_path, [voltage("VBUS", 3.3, tol_v=0.05, min_v=3.2, max_v=3.4, ripple=0.1),
                         voltage("VBUS", 5.0, tol_v=0.25), voltage("VBUS", ripple=0.01)], chans, 100_000)
    assert [ok for _, ok, _ in got] == [True, False, False]
    assert got[1][2].startswith("mean 3.") and "vs 5.0000" in got[1][2]
    assert "peak-to-peak" in got[2][2]


def test_voltage_from_raw_values_and_without_a_conversion(tmp_path):
    raw = [4095 * 1650 // 3300] * 100                                    # 1.65 V on a 12-bit ADC at 3.3 V
    chans = [fileformat.analog_raw("MID", raw, 100_000, value_bits=12, zero=0, scale_nv=3300e6 / 4095),
             fileformat.analog_raw("RAW", raw, 100_000)]
    got = run(tmp_path, [voltage("MID", 1.65, tol_v=0.01), voltage("RAW", 1.65)], chans, 100_000)
    assert got[0][1] is True
    assert got[1][1] is None and "no conversion to volts" in got[1][2]


def test_square_on_an_adc_line(tmp_path):
    rate = 200_000                                                       # 200 samples per 1 kHz period
    chans = [fileformat.analog_volts("PWM", pwm_volts(20_000, rate, 1000, 0.3).tolist(), rate)]
    got = run(tmp_path, [square("PWM", 1000, 0.3, threshold=1.65), square("PWM", 1000, 0.3)], chans, rate)
    assert got[0][:2] == ("square", True), got[0]
    assert got[1][1] is None and "threshold=" in got[1][2]               # analog without a threshold: unchecked


def test_analog_on_a_logic_capture_keeps_its_own_rate(tmp_path):
    tick = 10_000_000                                                    # logic at 10 MHz, ADC at 48 kHz (not a divisor)
    rate = Fraction(80_000_000, 1667)
    n = 4800
    v = pwm_volts(n, float(rate), 500, 0.25)
    logic = fileformat.Channel("CLK", fileformat.pack(bytes(1000)), 1000)
    chans = [logic, fileformat.analog_volts("PWM", v.tolist(), rate, t0_ticks=Fraction(7, 2))]
    got = run(tmp_path, [square("PWM", 500, 0.25, threshold=1.65), level("CLK", 0)], chans, tick)
    assert [ok for _, ok, _ in got] == [True, True], got


def test_hysteresis_keeps_a_noisy_slow_edge_one_edge():
    rate = 1_000_000
    ramp = np.concatenate([np.zeros(200), np.linspace(0, 3.3, 400), np.full(200, 3.3),
                           np.linspace(3.3, 0, 400), np.zeros(200)])
    v = ramp + RNG.normal(0, 0.08, len(ramp))
    a = AnalogTrace("SLOW", v, Fraction(rate), encoding="analog-f32")
    assert len(to_logic(a, rate, 1.65).edges) > 2                        # one threshold: the noise makes a burst
    ch = to_logic(a, rate, (1.2, 2.1))
    assert len(ch.edges) == 2 and ch.initial == 0
    assert abs(ch.edges[0] - (200 + 400 * 2.1 / 3.3)) < 30          # near where the ramp reaches 2.1 V (noise: ~10 samples)


def test_uart_through_an_adc(tmp_path):
    rate, baud = 1_000_000, 9600
    bits = [1] * 200
    for byte in b"OK\n":
        bits += [0] + [(byte >> k) & 1 for k in range(8)] + [1]
    bits += [1] * 20
    per = rate / baud
    idx = (np.arange(int(len(bits) * per)) / per).astype(int)
    v = np.array(bits)[idx] * 3.3 + RNG.normal(0, 0.05, len(idx))
    chans = [fileformat.analog_volts("TX", v.tolist(), rate)]
    got = run(tmp_path, [uart("TX", baud, data="4f4b0a", threshold=(1.0, 2.3)), pulses("TX", threshold=1.65)],
              chans, rate)
    assert got[0][:2] == ("uart", True), got[0]
    assert got[1][1] is True


def test_edges_are_placed_between_samples():
    v = np.array([0.0, 0.0, 1.0, 3.0, 3.0])                              # crosses 1.5 V a quarter past sample 2
    a = AnalogTrace("X", v, Fraction(1_000), encoding="analog-f32")
    ch = to_logic(a, 100_000, 1.5)                                       # 100 ticks per sample
    assert list(ch.edges) == [225] and ch.step == 100
    assert list(schmitt(np.array([2.0, np.nan, 0.1]), 1.0, 1.5)) == [1, 1, 0]


def test_a_threshold_needs_volts():
    a = AnalogTrace("R", np.array([1, 2, 3]), Fraction(1000))
    with pytest.raises(ValueError, match="no conversion to volts"):
        to_logic(a, 1000, 1.5)


def test_analyze_decodes_an_adc_line(tmp_path):
    """analyze --threshold: a UART seen only by an ADC is found and decoded like a logic line."""
    import json
    import subprocess
    import sys
    rate, baud = 1_000_000, 57_600
    bits = [1] * 400
    for byte in b"hello adc\n" * 6:
        bits += [0] + [(byte >> k) & 1 for k in range(8)] + [1, 1]
    bits += [1] * 400
    per = rate / baud
    idx = (np.arange(int(len(bits) * per)) / per).astype(int)
    v = np.array(bits)[idx] * 3.3 + RNG.normal(0, 0.05, len(idx))
    p = tmp_path / "a.wireskein"
    fileformat.write(p, rate, [fileformat.Channel("IDLE", fileformat.pack(bytes(1000)), 1000),
                               fileformat.analog_volts("TX", v.tolist(), rate)])
    r = subprocess.run([sys.executable, "-m", "wireskein", "analyze", str(p), "--threshold", "TX=1.0,2.3"],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    doc = json.loads(r.stdout)
    (claim,) = [c for c in doc["claims"] if c["protocol"] == "uart"]
    assert claim["roles"] == {"data": "TX"}
    assert "hello adc" in json.dumps(claim["result"])
    r = subprocess.run([sys.executable, "-m", "wireskein", "analyze", str(p), "--threshold", "NOPE=1"],
                       capture_output=True, text=True)
    assert r.returncode != 0 and r.stderr.startswith("wireskein analyze: NOPE: not an analog channel")


def esp32_12db(name, raw):
    """A classic ESP32 frontend at 12 dB, as the fixed probe declares it: 150..2450 mV over codes 0..4095."""
    return fileformat.analog_raw(name, raw, 100_000, value_bits=12, zero=-267, scale_nv=561_661)


def test_clipped_codes_are_not_voltages(tmp_path):
    rail = [4095] * 50                                                    # a 3V3 rail: beyond the 2.45 V end
    gnd = [0] * 50                                                        # GND: below the 150 mV end
    mid = [2048] * 50                                                     # 1.30 V
    chans = [esp32_12db("RAIL", rail), esp32_12db("GND", gnd), esp32_12db("MID", mid)]
    got = run(tmp_path, [
        voltage("RAIL", min_v=3.0),                  # >= 2.45 V cannot tell whether it reaches 3.0 V
        voltage("RAIL", max_v=2.0),                  # >= 2.45 V is above 2.0 V for sure
        voltage("GND", max_v=0.5),                   # <= 0.15 V is below 0.5 V for sure: fine
        voltage("GND", min_v=0.2),                   # <= 0.15 V is below 0.2 V for sure
        voltage("RAIL", 3.3, tol_v=0.1),             # a mean of clipped samples is no voltage
        voltage("MID", 1.30, tol_v=0.01),
    ], chans, 100_000)
    assert [ok for _, ok, _ in got] == [None, False, True, False, None, True], got
    assert "cannot be told" in got[0][2] and "clipped at the high end" in got[1][2]


def test_a_threshold_outside_the_range_is_refused():
    from wireskein._engine.model import AnalogTrace
    a = AnalogTrace("X", np.array([0, 4095, 0, 4095]), Fraction(1000), Fraction(0), "analog", 16, 12, -267, 561_661)
    assert len(to_logic(a, 1000, 1.65).edges) == 3                       # inside 0.15..2.45 V: clipped ends are sure
    with pytest.raises(ValueError, match="not inside the frontend's range"):
        to_logic(a, 1000, 3.0)


def test_an_inverting_frontend_clips_code_0_at_the_high_end(tmp_path):
    inv = fileformat.analog_raw("INV", [0] * 20, 100_000, value_bits=12, zero=4095, scale_nv=-600_000)   # 0 -> +2.457 V
    got = run(tmp_path, [voltage("INV", max_v=2.0), voltage("INV", min_v=0.1)], [inv], 100_000)
    assert [ok for _, ok, _ in got] == [False, True], got                   # >= the high end: above 2.0 for sure
