"""Channels kept by interval (interval-any / interval-latch, OEP multirate's any_active / edge_latch): the file form,
and the checks, which say only what the summaries fix."""

import numpy as np
import pytest

from wireskein import fileformat
from wireskein.analyze import load, save
from wireskein.runlog import Recorder, ends, level, only_moving, pulses, square, starts
from wireskein.verify import verify


def summarize(line, d, kind, active):
    """The summaries of a line (levels by tick from 0), as OEP's multirate defines them."""
    out = []
    for k in range(len(line) // d):
        span = range(k * d, (k + 1) * d)
        if kind == "any":
            out.append(active if any(line[n] == active for n in span) else 1 - active)
        else:
            edge = any(n >= 1 and line[n - 1] != active and line[n] == active for n in span)
            out.append(line[(k + 1) * d - 1] | (edge << 1))
    return out


def iv(name, line, d, kind="any", active=1):
    return fileformat.interval(name, summarize(line, d, kind, active), d, 0, f"interval-{kind}", active)


def test_the_file_keeps_them(tmp_path):
    line = [0] * 10 + [1] * 3 + [0] * 19 + [1] * 32
    chans = [fileformat.Channel("CLK", fileformat.pack(bytes(64)), 64),
             iv("CS", line, 8, "any", 0), iv("IRQ", line, 8, "latch", 1)]
    p = fileformat.write(tmp_path / "a.wireskein", 1_000_000, chans, trigger_tick=10)
    head, back = fileformat.read(p)
    assert [c["encoding"] for c in head["channels"]] == ["bits", "interval-any", "interval-latch"]
    assert head["channels"][1]["active"] == 0 and head["ticks"] == 64 and head["meta"]["trigger_tick"] == 10
    assert back[1].values() == summarize(line, 8, "any", 0) and back[2].values() == summarize(line, 8, "latch", 1)
    assert back[2].values()[1] == 0b10                   # the pulse at 10-12 inside interval 1: a change to active, low at its end
    save(tmp_path / "b.wireskein", load(p))              # through the engine and back
    assert [c.values() for c in fileformat.read(tmp_path / "b.wireskein")[1][1:]] == [back[1].values(), back[2].values()]
    with pytest.raises(ValueError, match="no form for the channels kept by interval: CS"):
        save(tmp_path / "c.sr", load(p))


def run(tmp_path, expect, channels):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=expect):
        rec.capture(rec.armed(), 1_000_000, channels=channels)
    rec.close()
    return [(r["check"], r["ok"], r["reason"]) for r in verify(tmp_path)["results"]]


def test_any_says_what_it_fixes(tmp_path):
    idle = [1] * 64                                          # an active-low CS that never moved
    busy = [1] * 20 + [0] * 4 + [1] * 8 + [0] * 2 + [1] * 30   # two selects, in intervals 2 and 4 (d 8)
    got = run(tmp_path, [level("IDLE", 1), level("BUSY", 1), level("BUSY", 0), starts({"BUSY": 1}), ends({"BUSY": 1}),
                         pulses("BUSY", count=2), pulses("BUSY", count=1), only_moving(["BUSY"]), square("BUSY", 1000)],
              [iv("IDLE", idle, 8, "any", 0), iv("BUSY", busy, 8, "any", 0)])
    ok = [o for _, o, _ in got]
    assert ok == [True, False, False, True, True, None, False, True, None], got
    assert "at least 2 rising edges" in got[5][2] and "needs every edge" in got[8][2]


def test_any_active_cannot_say_it_stayed_active(tmp_path):
    on = [1] * 64                                            # active-high, active in every interval: not proof it never fell
    got = run(tmp_path, [level("EN", 1), starts({"EN": 1}), only_moving([])], [iv("EN", on, 8, "any", 1)])
    assert [o for _, o, _ in got] == [None, None, None], got


def test_latch_keeps_the_end_levels_and_the_changes(tmp_path):
    line = [0] * 9 + [1] * 2 + [0] * 13 + [1] * 40          # rises at 9 and 24 (intervals 1 and 3), high at the end
    on = [1] * 64
    got = run(tmp_path, [pulses("IRQ", count=1), ends({"IRQ": 1}), starts({"IRQ": 0}), level("ON", 1), level("IRQ", 0),
                         only_moving(["IRQ"])],
              [iv("IRQ", line, 8, "latch", 1), iv("ON", on, 8, "latch", 1)])
    assert [o for _, o, _ in got] == [False, True, None, True, False, True], got
    assert "at least 2 rising edges, want 1" in got[0][2]


def test_latch_low_idle_line_is_not_sure_at_its_first_tick(tmp_path):
    idle = [1] * 64                                          # active-low, high all through: tick 0 compared with nothing
    got = run(tmp_path, [level("CS", 1), ends({"CS": 1})], [iv("CS", idle, 8, "latch", 0)])
    assert [o for _, o, _ in got] == [None, True], got


def test_info_names_them(tmp_path):
    import subprocess
    import sys
    p = fileformat.write(tmp_path / "i.wireskein", 1_000_000, [iv("CS", [1] * 64, 32, "any", 0)], trigger_tick=5)
    out = subprocess.run([sys.executable, "-m", "wireskein", "info", str(p)], capture_output=True, text=True,
                         check=True).stdout
    assert "interval-any" in out and "active-low" in out and "32 ticks a value" in out
    assert "trigger_tick 5" in out


def test_the_command_line_names_reductions():
    from wireskein import sources
    text = "SDA=47,CLK=3/4,PH=3/4+1,CS=5:any-low/32,IRQ=7:latch-high/8,D0"
    assert sources.parse_channels(text) == [("SDA", "47"), ("CLK", "3"), ("PH", "3"), ("CS", "5"), ("IRQ", "7"),
                                            ("D0", "D0")]
    assert sources.parse_reduce(text) == {"CLK": ("sample", 4, 0), "PH": ("sample", 4, 1), "CS": ("any", 32, 0),
                                          "IRQ": ("latch", 8, 1)}
    with pytest.raises(ValueError, match="not a reduction"):
        sources.parse_reduce("CS=5:any/32")
    with pytest.raises(ValueError, match="phase below d"):
        sources.Request([("C", "3")], 1000, 100, reduce={"C": ("sample", 4, 4)})
