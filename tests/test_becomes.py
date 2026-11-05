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
    assert m["change_s"] == 0.0045 and m["irregular"] == 0 and m["before"] == {"period_s": 0.001, "high_s": 0.00025}
    assert m["change_tick"] == 20_000 and m["period_s"] == 0.001


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


def test_a_high_time_tolerance_relative_to_it(tmp_path):
    # a 2400 us pulse 1 % short (2376 us): outside 20 us, inside 2 % of itself
    lv = wave([("level", 0, 30_000), ("pulses", 2376, 19_800, 25)])
    for name, kw, want in (("abs", {"high_abs_s": 20e-6}, "ng"), ("rel", {"high_rel": 0.02}, "ok")):
        sec = [("us", 20_000, 520_000, [becomes("P", period_s=0.02, high_s=2400e-6, min_periods=20, **kw)])]
        assert run(tmp_path / name, lv, sec)["us"]["status"] == want, name


def test_sections_give_each_segment_cut_to_it_for_analysis(tmp_path):
    from wireskein import states
    run(tmp_path, wave(PWM), [("d25", 2_000, 18_000, []), ("d50", 22_000, 39_000, [])])
    ss = {s["path"]: s for s in verify.sections(tmp_path)}
    assert ss["d25"]["window"] == [2_000, 18_000] and ss["d50"]["capture"].n_samples == 17_000
    doc = states.of_sections(tmp_path, "d5*")
    (sec,) = doc["sections"]
    (x,) = [x for x in sec["lines"]["P"] if "duty" in x]
    assert sec["path"] == "d50" and x["duty"] == 0.5 and x["periods"] >= 15
    assert states.text(doc)[0].startswith("== d50  (c0001.wireskein@22000:39000)")


def test_a_verdict_of_the_test_is_reported_and_marked(tmp_path):
    from wireskein import _fileformat as ff
    rec = Recorder(tmp_path)
    armed = rec.armed()
    ticks = iter((2_000, 18_000))
    with rec.section(1, "d25", clock=lambda: _Reading(next(ticks))):
        pass
    rec.capture(armed, RATE, interleaved=wave(PWM).tobytes(), names=["P"], start_ns=START_NS, boot_id=7)
    rec.result("d25", "my_rule", "ng", measured={"pulses": 16}, reason="16 pulses, want 15",
               notices=[{"kind": "extra_pulse", "tick": 17_000}], capture="c0001.wireskein")
    rec.result("d2", "typo", "ok")
    rec.close()
    rep = verify.verify(tmp_path, mark=True)
    mine, typo = rep["results"]
    assert (mine["check"], mine["status"], mine["reason"], mine["measured"]) == ("my_rule", "ng", "16 pulses, want 15",
                                                                                  {"pulses": 16})
    assert typo["status"] == "ng" and "no segment" in typo["reason"]
    assert verify.failed(rep) and rep["summary"]["notices"] == 1
    assert [(m["t"], m["label"]) for m in ff.markers(tmp_path / "c0001.wireskein") if m["by"] == "verify"] == [
        (17_000, "extra_pulse: d25")]


def test_a_recorder_gives_its_sections_before_it_closes(tmp_path):
    rec = Recorder(tmp_path)
    armed = rec.armed()
    ticks = iter((2_000, 18_000))
    with rec.section(1, "d25", clock=lambda: _Reading(next(ticks))):
        pass
    rec.capture(armed, RATE, interleaved=wave(PWM).tobytes(), names=["P"], start_ns=START_NS, boot_id=7)
    (s,) = [s for s in rec.sections() if s["path"] == "d25"]
    assert s["window"] == [2_000, 18_000] and s["capture"].n_samples == 16_000
    rec.result("d25", "mine", "ok")
    rec.close()
    assert verify.verify(tmp_path)["results"][0]["check"] == "mine"


def test_states_keeps_each_odd_pulse_when_asked(tmp_path):
    from wireskein import states
    e = np.array([100, 110, 250, 270, 520, 525, 610, 640, 900, 915], np.int64)   # five pulses, no wave
    assert any("activity" in x for x in states.of_line(e, 0, 2000, 1e6))
    xs = states.of_line(e, 0, 2000, 1e6, max_odd=None)
    assert [x["odd"] for x in xs if "odd" in x] == [100, 250, 520, 610, 900]
