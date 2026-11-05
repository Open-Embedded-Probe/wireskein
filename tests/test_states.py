"""wireskein.states: a line as its stretches (levels, waves) and the odd pulses between them."""

import numpy as np

from wireskein import states


def edges_of(lv: np.ndarray) -> tuple[np.ndarray, int]:
    return np.flatnonzero(np.diff(lv)) + 1, int(lv[0])


def wave(parts) -> np.ndarray:
    out = []
    for p in parts:
        if p[0] == "level":
            out.append(np.full(p[2], p[1], np.uint8))
        else:
            one = np.zeros(p[2], np.uint8)
            one[:p[1]] = 1
            out.append(np.tile(one, p[3]))
    return np.concatenate(out)


def test_a_tone_that_changes_and_stops():
    lv = wave([("level", 0, 5000), ("pulse", 1130, 2261, 10), ("pulse", 846, 1172, 1), ("pulse", 248, 497, 30),
               ("pulse", 151, 151, 1), ("level", 0, 5000)])
    e, init = edges_of(lv)
    xs = states.of_line(e, init, len(lv), 1e6)
    kinds = [("level" if "level" in x else "odd" if "odd" in x else "wave") for x in xs]
    assert kinds == ["level", "wave", "odd", "wave", "odd", "level"], xs
    assert xs[1]["periods"] == 10 and abs(xs[1]["freq_hz"] - 1e6 / 2261) < 1e-6
    assert xs[2] == {"odd": 5000 + 22610, "high_s": 846e-6, "period_s": 1172e-6, "end": 5000 + 22610 + 1172}
    assert xs[4]["high_s"] == 151e-6 and xs[4]["period_s"] is None
    assert xs[5]["level"] == 0 and xs[5]["t1"] == len(lv)


def test_a_pwm_to_full_and_off():
    lv = wave([("pulse", 250, 1000, 20), ("pulse", 500, 1000, 20), ("level", 1, 30_000), ("level", 0, 20_000)])
    e, init = edges_of(lv)
    xs = states.of_line(e, init, len(lv), 1e6)
    assert [(x.get("duty"), x.get("level")) for x in xs] == [(0.25, None), (0.5, None), (None, 1), (None, 0)], xs


def test_a_line_that_never_moves():
    assert states.of_line(np.zeros(0, np.int64), 1, 100, 1e6) == [{"t0": 0, "t1": 100, "level": 1}]


def test_a_burst_without_a_wave_is_activity():
    lv = np.zeros(10_000, np.uint8)
    for a, w in ((1000, 30), (1100, 70), (1300, 20), (1350, 90), (1500, 10), (1600, 40)):
        lv[a:a + w] = 1
    e, init = edges_of(lv)
    xs = states.of_line(e, init, len(lv), 1e6)
    assert any(x.get("activity") == 6 for x in xs), xs
