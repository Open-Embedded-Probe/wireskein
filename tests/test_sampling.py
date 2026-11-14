"""How finely a check sees what it looks at (verify._sampling), and I2C read as an input sees it: spikes left out,
a slow edge's crossings as one. Synthetic captures (gen.i2c), the truth being what was generated."""

import numpy as np

from wireskein._engine import gen
from wireskein.runlog import i2c
from wireskein.verify import CHECKS, _notices, _sampling

F = 100e3                      # SCL at 100 kHz: gen.i2c's halves are 5 us
TX = [{"addr": 0x42, "rw": "write", "bytes": [0x5A, 0x01]}, {"addr": 0x42, "rw": "read", "bytes": [0x10]}]


def waves(data_at=0.5):
    scl, sda = gen.Wave(1), gen.Wave(1)
    end = gen.i2c(scl, sda, 20e-6, TX, F, data_at=data_at)
    return scl, sda, end


def capture(rate, scl, sda, end, phase=0.0, meta=None):
    cap = gen.quantize({"SCL": scl, "SDA": sda}, rate, end + 20e-6, np.random.default_rng(0), t_start=-phase)
    if meta:
        cap.meta.update(meta)
    return cap


def judge(cap, **k):
    x = i2c("SCL", "SDA", TX, **k)
    ok, got, why = CHECKS["i2c"](cap, x)
    ok, why = _sampling(ok, why, got, x, cap)
    ok, why, found = _notices(ok, why, got, x, cap)
    return ok, got, why, found


def test_a_clock_under_five_samples_a_half_is_still_read_and_told_with_the_rate_to_take():
    scl, sda, end = waves()
    ok, got, why, found = judge(capture(600e3, scl, sda, end))       # 3 samples a half
    assert ok is True, why                                           # read all the same
    assert got["sampling"]["lengths"][0]["samples"] == 3.0
    u = [n for n in found if n["kind"] == "under_rate" and n["whole"]]
    assert len(u) == 1 and u[0]["least_rate_hz"] == 1e6 and u[0]["best_rate_hz"] == 2e6


def test_a_test_that_wants_it_strict_limits_under_rate():
    scl, sda, end = waves()
    ok, _, why, _ = judge(capture(600e3, scl, sda, end), notices={"under_rate": "ng"})
    assert ok is False and "under_rate" in why


def test_at_ten_samples_a_half_it_answers_and_says_how_finely():
    scl, sda, end = waves()
    ok, got, why, found = judge(capture(2e6, scl, sda, end))          # 10 samples a half: 500 ns
    assert ok is True, why
    assert got["sampling"]["interval_s"] == 5e-7 and got["sampling"]["lengths"][0]["samples"] == 10.0
    # the spikes it would tell (50 ns) are a tenth of a sample: that part is not answered, and said so
    under = [n for n in found if n["kind"] == "under_rate"]
    assert len(under) == 1 and under[0]["what"].startswith("a spike") and under[0]["samples"] == 0.1


def test_a_glitch_filter_as_wide_as_the_spikes_leaves_them_unseen():
    scl, sda, end = waves()
    cap = capture(200e6, scl, sda, end, meta={"probe": {"glitch_filter_ns": 100}})
    ok, _, why, found = judge(cap)
    under = [n for n in found if n["kind"] == "under_rate"]
    assert ok is True and len(under) == 1 and abs(under[0]["glitch_filter_s"] - 1e-7) < 1e-15


def test_a_spike_on_sda_while_scl_is_high_is_left_out_as_an_input_does():
    # 20 ns low on SDA in the middle of a high SCL: a START and a STOP to a reader without a spike filter
    scl, sda, end = waves()
    at = scl.times[3] + 1e-6                                          # a while after an SCL rise inside the address
    sda.times = sorted(sda.times + [at, at + 20e-9])
    ok, got, why, found = judge(capture(200e6, scl, sda, end))
    assert ok is True, why
    assert [n["width_ticks"] for n in found if n["kind"] == "bus_glitch"] == [4]


def test_a_slow_edge_crossing_three_times_reads_as_one_and_is_told():
    # SCL's rise crossing the threshold three times over 150 ns (a weak pull-up with noise on it)
    scl, sda, end = waves(data_at=0.1)
    rise = scl.times[3]
    scl.times = sorted(scl.times + [rise + 60e-9, rise + 150e-9])     # up at rise, down 60 ns on, up 150 ns on
    ok, got, why, found = judge(capture(100e6, scl, sda, end))
    assert ok is True, why
    slow = [n for n in found if n["kind"] == "slow_edge"]
    assert len(slow) == 1 and slow[0]["line"] == "scl" and slow[0]["changes"] == 3 and slow[0]["to"] == 1


def test_five_samples_a_half_reads_at_every_phase_of_the_sample_grid():
    # the data changing midway in SCL's low is 2 samples from one edge and 3 from the other, either way round by
    # where the grid falls: both edges are tried, and I2C reads on the rising one
    scl, sda, end = waves()
    for phase in np.linspace(0, 1e-6, 7, endpoint=False):
        ok, _, why, _ = judge(capture(1e6, scl, sda, end, phase))
        assert ok is True, (phase, why)
