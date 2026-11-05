"""becomes: a line ends a segment in a state, reached by at most one change from the state it was in (read from the
capture just before the segment); trim: any check without the ends of its segment. One capture cut into sections by
headings on the probe's clock (docs/run-format.ja.md §2.4), as a test that steps through conditions records it."""

import time

import numpy as np

from wireskein import verify
from wireskein.runlog import Recorder, becomes, square, trim

START_NS = 1_000_000_000
RATE = 1_000_000                       # a tick is 1 us


class _Reading:
    def __init__(self, tick):
        self.uptime_ns, self.boot_id, self.host_ns = START_NS + tick * 1000, 7, time.monotonic_ns()


def wave(parts) -> np.ndarray:
    """parts: ("level", v, ticks) or ("pulses", high, period, count), one after another."""
    out = []
    for p in parts:
        if p[0] == "level":
            out.append(np.full(p[2], p[1], np.uint8))
        else:
            _, high, period, count = p
            one = np.zeros(period, np.uint8)
            one[:high] = 1
            out.append(np.tile(one, count))
    return np.concatenate(out)


def run(tmp_path, lv: np.ndarray, sections: list[tuple[str, int, int, list]]):
    """sections: (name, first tick, end tick, expectations) on the one capture."""
    rec = Recorder(tmp_path)
    armed = rec.armed()
    ticks = iter(t for _, a, b, _ in sections for t in (a, b))
    rec.use_clock(lambda: _Reading(next(ticks)))
    for name, _, _, want in sections:
        with rec.section(1, name, expect=want):
            pass
    rec.use_clock(None)
    rec.capture(armed, RATE, interleaved=lv.tobytes(), names=["P"], start_ns=START_NS, boot_id=7)
    rec.close()
    rep = verify.verify(tmp_path)
    return {r["path"]: r for r in rep["results"]}


# a 1 kHz PWM going from 25 % to 50 %: the segment opens before the change
PWM = [("pulses", 250, 1000, 20), ("pulses", 500, 1000, 20)]


def test_a_change_at_a_period_boundary(tmp_path):
    r = run(tmp_path, wave(PWM), [("d50", 15_500, 40_000, [becomes("P", freq_hz=1000, duty=0.5, irregular_max=0)])])
    r = r["d50"]
    assert r["status"] == "ok", r
    m = r["measured"]
    assert m["change_s"] == 0.0045 and m["irregular"] == 0 and m["before"].startswith("period 1000.0 us, high 250.0")


def test_the_state_before_is_read_from_before_the_segment(tmp_path):
    # the segment opens after the last old pulse fell: the old state is only seen before it
    r = run(tmp_path, wave(PWM), [("d50", 19_300, 40_000, [becomes("P", freq_hz=1000, duty=0.5)])])["d50"]
    assert r["status"] == "ok" and r["measured"]["change_s"] == 0.0007, r


def test_already_in_the_state_is_no_change(tmp_path):
    r = run(tmp_path, wave(PWM), [("d50", 25_500, 40_000, [becomes("P", freq_hz=1000, duty=0.5)])])["d50"]
    assert r["status"] == "ok" and r["measured"]["change_s"] is None, r


def test_a_command_that_did_not_take_is_ng(tmp_path):
    lv = wave([("pulses", 250, 1000, 40)])
    r = run(tmp_path, lv, [("d50", 15_500, 40_000, [becomes("P", freq_hz=1000, duty=0.5)])])["d50"]
    assert r["status"] == "ng" and "0 whole periods in the state" in r["reason"], r


def test_a_cut_period_at_the_change(tmp_path):
    # the old wave cut short (a high of 100 us, a period of 400 us) where the new one starts
    lv = wave([("pulses", 250, 1000, 20), ("pulses", 100, 400, 1), ("pulses", 500, 1000, 20)])
    sec = [("d50", 15_500, 40_400, [becomes("P", freq_hz=1000, duty=0.5)])]
    r = run(tmp_path / "a", lv, sec)["d50"]
    assert r["status"] == "ok" and r["measured"]["irregular_pulses"] == [{"high_s": 0.0001, "period_s": 0.0004}], r
    sec = [("d50", 15_500, 40_400, [becomes("P", freq_hz=1000, duty=0.5, irregular_max=0)])]
    assert run(tmp_path / "b", lv, sec)["d50"]["status"] == "ng"
    sec = [("d50", 15_500, 40_400, [becomes("P", freq_hz=1000, duty=0.5, irregular_min=1)])]
    assert run(tmp_path / "c", lv, sec)["d50"]["status"] == "ok"           # a test that wants the cut period


def test_a_glitch_after_the_change_is_ng(tmp_path):
    lv = wave([("pulses", 250, 1000, 20), ("pulses", 500, 1000, 8), ("pulses", 20, 1000, 1), ("pulses", 500, 1000, 11)])
    r = run(tmp_path, lv, [("d50", 15_500, 40_000, [becomes("P", freq_hz=1000, duty=0.5)])])["d50"]
    assert r["status"] == "ng", r
    # square judges every period too: the one runt is found
    sq = run(tmp_path / "sq", lv, [("d50", 25_500, 40_000, [square("P", 1000, 0.5)])])["d50"]
    assert sq["status"] == "ng" and sq["measured"]["outliers"] == 1 and "1 of" in sq["reason"], sq


def test_two_changes_are_ng(tmp_path):
    lv = wave([("pulses", 250, 1000, 20), ("pulses", 750, 1000, 5), ("pulses", 500, 1000, 15)])
    r = run(tmp_path, lv, [("d50", 15_500, 40_000, [becomes("P", freq_hz=1000, duty=0.5)])])["d50"]
    assert r["status"] == "ng" and "more than one change" in r["reason"], r


def test_a_stop_to_a_level_with_its_last_pulse_cut(tmp_path):
    lv = wave([("pulses", 500, 1000, 20), ("pulses", 120, 120, 1), ("level", 0, 10_000)])
    r = run(tmp_path, lv, [("stop", 15_500, 30_000, [becomes("P", level=0)])])["stop"]
    assert r["status"] == "ok" and r["measured"]["irregular"] == 1 and r["measured"]["change_s"] == 0.00462, r
    r = run(tmp_path / "strict", lv, [("stop", 15_500, 30_000, [becomes("P", level=0, irregular_max=0)])])["stop"]
    assert r["status"] == "ng"


def test_a_level_it_does_not_reach_is_ng(tmp_path):
    lv = wave([("pulses", 500, 1000, 30)])
    r = run(tmp_path, lv, [("stop", 15_500, 29_700, [becomes("P", level=0)])])["stop"]
    assert r["status"] == "ng", r


def test_trim_leaves_the_middle_of_the_segment(tmp_path):
    # square on a segment holding the change fails; without its first 6 ms it holds only the new state
    lv = wave(PWM)
    sec = [("d50", 15_500, 40_000, [square("P", 1000, 0.5)])]
    assert run(tmp_path / "all", lv, sec)["d50"]["status"] == "ng"
    sec = [("d50", 15_500, 40_000, [trim(square("P", 1000, 0.5), head_s=0.006)])]
    r = run(tmp_path / "mid", lv, sec)["d50"]
    assert r["status"] == "ok" and r["measured"]["trimmed"] == [6000, 24_500], r


def test_trim_by_periods_of_the_checks_own_frequency(tmp_path):
    sec = [("d50", 15_500, 40_000, [trim(square("P", 1000, 0.5), head_periods=6)])]
    r = run(tmp_path, wave(PWM), sec)["d50"]
    assert r["status"] == "ok" and r["measured"]["trimmed"] == [6000, 24_500], r
