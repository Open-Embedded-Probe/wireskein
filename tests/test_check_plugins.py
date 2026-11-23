"""Checks from elsewhere (plugins.register_check, the entry point group wireskein.checks): judged, reported, their
notices limited and marked like wireskein's own; an unknown kind is an error of the expectation."""

import importlib.metadata as md

import numpy as np
import pytest

from wireskein import plugins, verify
from wireskein.runlog import Recorder


def run(tmp_path, checks):
    lv = np.zeros(10_000, np.uint8)
    for a in (1000, 3000, 5000):                         # three pulses of 100 ticks on P
        lv[a:a + 100] = 1
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=checks):
        rec.capture(rec.armed(), 1_000_000, interleaved=lv.tobytes(), names=["P"])
    rec.close()
    return verify.verify(tmp_path, mark=True)


def three_pulses(cap, x):
    ch = cap.channel(x["pins"][0])
    rises = ch.edges[0::2] if ch.initial == 0 else ch.edges[1::2]
    got = {"pulses": int(len(rises)), "notices": [{"kind": "late_pulse", "tick": int(t)} for t in rises if t > 4000]}
    return len(rises) == x["count"], got, "" if len(rises) == x["count"] else f"{len(rises)} pulses"


@pytest.fixture
def registered():
    plugins.register_check("tests.x_pulses", three_pulses, notices={"late_pulse": "a pulse after 4 ms"})
    yield
    plugins.unregister_check("tests.x_pulses")


def test_a_registered_check_is_judged_with_its_notices(tmp_path, registered):
    rep = run(tmp_path, [{"kind": "tests.x_pulses", "pins": ["P"], "count": 3, "notices": {"late_pulse": "ng"}}])
    (r,) = rep["results"]
    assert r["status"] == "ng" and "1 late_pulse" in r["reason"] and r["measured"]["pulses"] == 3, r
    assert r["notices"] == [{"kind": "late_pulse", "tick": 5000, "part": 0}]
    from wireskein import _fileformat as ff
    assert [m["label"] for m in ff.markers(tmp_path / "c0001.wireskein")] == ["late_pulse: t"]


def test_an_unknown_kind_is_an_error_of_the_expectation(tmp_path):
    (r,) = run(tmp_path, [{"kind": "no_such_check", "pins": ["P"]}])["results"]
    assert r["status"] == "ng" and "no check named 'no_such_check'" in r["reason"]


def test_a_kind_of_wireskein_is_not_replaced():
    with pytest.raises(ValueError):
        plugins.register_check("square", three_pulses)


def test_an_installed_package_adds_a_check(tmp_path, monkeypatch):
    class Plugin:                                          # what an entry point's object may be
        check = staticmethod(three_pulses)
        notices = {"late_pulse": "a pulse after 4 ms"}

    ep = md.EntryPoint("tests.y_pulses", "irrelevant:obj", "wireskein.checks")
    monkeypatch.setattr(md.EntryPoint, "load", lambda self: Plugin)
    monkeypatch.setattr(md, "entry_points", lambda group=None: [ep] if group == "wireskein.checks" else [])
    monkeypatch.setattr(plugins, "_LOADED", set())
    try:
        (r,) = run(tmp_path, [{"kind": "tests.y_pulses", "pins": ["P"], "count": 3}])["results"]
        assert r["status"] == "ok" and len(r["notices"]) == 1, r
    finally:
        plugins.unregister_check("tests.y_pulses")


# ---- the example examples/checks/demo_settle.py, on a contact bouncing then settling (made here) ----

def test_the_example_check_demo_settle(tmp_path):
    import importlib.util
    from pathlib import Path
    spec = importlib.util.spec_from_file_location("demo_settle", Path(__file__).parents[1] / "examples/checks/demo_settle.py")
    demo = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(demo)
    plugins.register_check("examples.demo_settle", demo.CHECK.check, notices=demo.CHECK.notices)
    try:
        lv = np.zeros(100_000, np.uint8)                     # 1 MS/s: bounces at 10 ms, settled high from 10.5 ms
        for a, b in ((10_000, 10_050), (10_120, 10_200), (10_300, 10_310), (10_500, 100_000)):
            lv[a:b] = 1
        rec = Recorder(tmp_path)
        x = {"kind": "examples.demo_settle", "pins": ["K"], "level": 1, "quiet_s": 0.05}
        with rec.section(1, "press", expect=[x, {**x, "changes_max": 3}, {**x, "notices": {"bounce": "ng"}}]):
            rec.capture(rec.armed(), 1_000_000, interleaved=lv.tobytes(), names=["K"])
        rec.close()
        ok, few, none = verify.verify(tmp_path)["results"]
        assert ok["status"] == "ok" and ok["measured"]["changes"] == 7 and len(ok["notices"]) == 6, ok
        assert few["status"] == "ng" and "7 changes, more than 3" in few["reason"], few
        assert none["status"] == "ng" and "bounce" in none["reason"], none
    finally:
        plugins.unregister_check("examples.demo_settle")
