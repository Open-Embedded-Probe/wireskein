"""pulses() with widths (a pulse a watchdog's reset or a wake ends) and square()'s grid (a wave and a capture on
one clock source), on lines written here."""

import numpy as np

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import pulses, square
from wireskein.verify import check_pulses, check_square

RATE = 1e6


def cap(edges, initial=0, n=1_000_000):
    return Capture(RATE, n, [Channel("P", initial, np.asarray(edges, np.int64))])


def test_one_high_pulse_of_a_width():
    c = cap([100_000, 350_000])                                   # high 0.25 s, inside the window
    ok, got, why = check_pulses(c, pulses("P", count=1, width_min_s=0.233, width_max_s=0.258))
    assert ok and got["widths_s"] == [0.25], why
    ok, got, why = check_pulses(c, pulses("P", count=1, width_min_s=0.1, width_max_s=0.2))
    assert ok is False and "outside" in why


def test_a_pulse_cut_by_the_window_is_none():
    c = cap([600_000])                                            # rises, never falls inside
    ok, got, why = check_pulses(c, pulses("P", count=1, width_min_s=0.1, width_max_s=2.0))
    assert ok is False and got["pulses"] == 0


def test_two_pulses_where_one_is_wanted():
    c = cap([100_000, 300_000, 500_000, 700_000])
    ok, got, why = check_pulses(c, pulses("P", count=1, width_min_s=0.1, width_max_s=0.3))
    assert ok is False and got["pulses"] == 2


def test_low_pulses():
    c = cap([200_000, 400_000], initial=1)
    ok, got, _ = check_pulses(c, pulses("P", count=1, level=0, width_min_s=0.19, width_max_s=0.21))
    assert ok and got["widths_s"] == [0.2]


def test_square_on_its_grid_and_a_sample_off():
    # 1 kHz from tick 37: every rising edge on the grid of exactly 1000 ticks, then one a tick late, then two
    rises = 37 + 1000 * np.arange(200)
    e = np.sort(np.concatenate((rises, rises + 500)))
    ok, got, why = check_square(cap(e), square("P", 1000, 0.5, grid_abs_s=1 / RATE))
    assert ok and got["grid_max_s"] == 0, why
    late = rises.copy()
    late[100:] += 1
    e = np.sort(np.concatenate((late, late + 500)))
    ok, got, _ = check_square(cap(e), square("P", 1000, 0.5, grid_abs_s=1 / RATE))
    assert ok and abs(got["grid_max_s"] - 1e-6) < 1e-12
    late[150:] += 1
    e = np.sort(np.concatenate((late, late + 500)))
    ok, got, why = check_square(cap(e), square("P", 1000, 0.5, grid_abs_s=1 / RATE))
    assert ok is False and "off the grid" in why


def test_square_from_another_source_fits_its_own_grid():
    # 100 ppm fast: off the nominal grid by 20 ticks at the end, on the fitted one within a tick
    rises = np.round(37 + 999.9 * np.arange(200)).astype(np.int64)
    e = np.sort(np.concatenate((rises, rises + 500)))
    ok, got, _ = check_square(cap(e), square("P", 1000, 0.5))
    assert got["grid_max_s"] > 15e-6 and got["grid_fit_max_s"] <= 1e-6
    assert abs(got["grid_fit_period_s"] - 999.9e-6) < 1e-9
