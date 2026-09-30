"""Aligning an analog track to the logic ticks by a signal seen on both."""

import json
import subprocess
import sys
from fractions import Fraction

import numpy as np
import pytest

from wireskein import align, fileformat
from wireskein._engine import fileio

TICK = 10_000_000                         # logic at 10 MHz
ADC = Fraction(80_000_000, 1667)          # an ADC at ~48 kHz, not a divisor of the ticks
RNG = np.random.default_rng(3)


def marker_times(n_ticks):
    """Irregular pulse edges (a marker, not a clock): (rise, fall) tick pairs."""
    t, out = 20_000, []
    while t < n_ticks - 40_000:
        width = int(RNG.integers(2_000, 9_000))
        out.append((t, t + width))
        t += width + int(RNG.integers(6_000, 30_000))
    return out


def make(tmp_path, true_offset_us, true_ppm, noise=0.03, uncertainty_ns=500_000, periodic=False):
    """A file whose analog track starts `true_offset_us` later than the file says and runs `true_ppm` fast."""
    n_ticks = 2_000_000                                                      # 200 ms
    pulses = [(t, t + 5_000) for t in range(20_000, n_ticks - 20_000, 10_000)] if periodic else marker_times(n_ticks)
    level = np.zeros(n_ticks, np.uint8)
    for a, b in pulses:
        level[a:b] = 1
    logic = fileformat.Channel("SYNC", fileformat.pack(level.tobytes()), n_ticks)
    per = TICK / float(ADC)
    n = int(n_ticks / per) - 200
    stated_t0 = 1000.0                                                       # what the probe said (ticks)
    true_t = true_offset_us * 1e-6 * TICK + stated_t0 + np.arange(n) * per * (1 + true_ppm * 1e-6)
    idx = np.clip(np.floor(true_t).astype(int), 0, n_ticks - 1)
    v = level[idx] * 3.3 + RNG.normal(0, noise, n)
    a = fileformat.analog_volts("SYNC_A", v.tolist(), ADC, t0_ticks=Fraction(stated_t0),
                                start_uncertainty_ns=uncertainty_ns)
    b = fileformat.analog_volts("VBUS", (3.3 + RNG.normal(0, 0.01, n)).tolist(), ADC, t0_ticks=Fraction(stated_t0))
    p = tmp_path / "m.wireskein"
    fileformat.write(p, TICK, [logic, a, b], start_uncertainty_ns=5_000)
    return p


@pytest.mark.parametrize("offset_us, ppm", [(200, 0), (-150, 1500), (37.3, -800), (201.25, 1500)])
def test_finds_offset_and_scale(tmp_path, offset_us, ppm):
    cap = fileio.load(make(tmp_path, offset_us, ppm))
    a = align.find(cap, "SYNC", "SYNC_A", threshold=(1.0, 2.3))
    c = a["channels"]["SYNC_A"]
    # the aligned time of the stated start: a + b * t0 must be where it really was
    true_start = offset_us * 1e-6 * TICK + 1000
    assert c["offset_ticks"] + c["scale"] * 1000 == pytest.approx(true_start, abs=0.3 * TICK / float(ADC))
    assert c["scale"] == pytest.approx(1 + ppm * 1e-6, abs=30e-6)
    assert c["matched"] >= 0.9 * c["overlap_edges"] and c["overlap_edges"] >= 0.9 * c["edges"]
    assert abs(c["scale"] - (1 + ppm * 1e-6)) < 5 * c["scale_ppm_uncertainty"] * 1e-6 + 5e-6
    assert set(a["channels"]) == {"SYNC_A", "VBUS"}                  # the ADC's other channel gets it too


def test_apply_and_save(tmp_path):
    p = make(tmp_path, 200, 1500)
    cap = fileio.load(p)
    a = align.find(cap, "SYNC", "SYNC_A", threshold=1.65)
    aligned = align.apply(cap, a)
    before = [t.ticks(TICK) for t in cap.analog if t.name == "SYNC_A"][0]
    after = [t.ticks(TICK) for t in aligned.analog if t.name == "SYNC_A"][0]
    assert after[0] - before[0] == pytest.approx(200e-6 * TICK, abs=250)
    assert aligned.channels is cap.channels                          # logic is the reference
    align.save(p, a)
    align.save(p, a)                                                 # replaces
    assert align.load(p) == json.loads(json.dumps(a))
    assert fileformat.read(p)[1][1].t0_ticks == 1000                 # the stored samples and times are untouched


def test_a_periodic_signal_wider_than_its_period_is_ambiguous(tmp_path):
    cap = fileio.load(make(tmp_path, 200, 0, periodic=True, uncertainty_ns=5_000_000))   # 1 ms period, 15 ms window
    with pytest.raises(ValueError, match="ambiguous"):
        align.find(cap, "SYNC", "SYNC_A", threshold=1.65)
    a = align.find(cap, "SYNC", "SYNC_A", threshold=1.65, window_ticks=3_000)              # 300 us: one answer
    c = a["channels"]["SYNC_A"]
    assert c["offset_ticks"] + c["scale"] * 1000 == pytest.approx(200e-6 * TICK + 1000, abs=100)


def test_cli(tmp_path):
    p = make(tmp_path, 200, 0)
    r = subprocess.run([sys.executable, "-m", "wireskein", "align", str(p), "--reference", "SYNC", "--via", "SYNC_A",
                        "--threshold", "1.0,2.3", "--save"], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert "SYNC_A" in r.stdout and "start +" in r.stdout
    assert align.load(p)["channels"]["VBUS"]["via"] == "SYNC_A"
