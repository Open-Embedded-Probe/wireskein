import json
import subprocess
import sys

import pytest

from wireskein import _fileformat as fileformat
from wireskein.runlog import FORMAT, Recorder, level


def test_runlog_imports_only_the_standard_library():
    code = "import sys, wireskein.runlog; print('numpy' in sys.modules)"
    out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True).stdout
    assert out.strip() == "False"


def test_run_json_layout(tmp_path):
    rec = Recorder(tmp_path, target="demo")
    with rec.section(1, "t", expect=[level("P0", 0)]):
        rec.command("PING")
        rec.reply("PONG")
        rec.note("hello")
        t = rec.armed()
        rec.capture(t, 1e6, interleaved=bytes(100), names=["P0"], start_us=12, time_base_slipped=True)
    doc = json.loads(rec.close().read_text())
    assert doc["format"] == FORMAT
    assert doc["meta"] == {"target": "demo"}
    assert [e["src"] for e in doc["log"]] == ["marker", "host", "dut", "note", "marker"]
    c = doc["captures"][0]
    assert c["file"] == "c0001.wireskein" and c["channels"] == ["P0"] and isinstance(c["host_ns"], int) and c["host_ns"] <= doc["log"][-1]["host_ns"]
    head, (ch,) = fileformat.read(tmp_path / "c0001.wireskein")
    assert head["meta"] == {"start_us": 12, "time_base_slipped": True}
    assert (ch.name, ch.n, ch.step, fileformat.unpack(ch)) == ("P0", 100, 1, bytes(100))
    assert list(doc["expect"]) == ["t"]


def test_repeated_headings_keep_their_own_expectations(tmp_path):
    rec = Recorder(tmp_path)
    with rec.section(1, "t"):
        for v in (0, 1, 0):
            with rec.section(2, "hold", expect=[level("P", v)]):
                pass
        with rec.section(2, "once", expect=[level("P", 1)]):
            pass
    with rec.section(1, "t"):
        with rec.section(2, "hold", expect=[level("P", 1)]):
            pass
    doc = json.loads(rec.close().read_text())
    assert list(doc["expect"]) == ["t/hold", "t/hold[1]", "t/hold[2]", "t/once", "t[1]/hold"]   # repeats: [1], [2], ...
    assert [spec["checks"][0]["value"] for spec in doc["expect"].values()] == [0, 1, 0, 1, 1]


def test_a_closed_run_refuses_more(tmp_path):
    from wireskein.runlog import Recorder
    rec = Recorder(tmp_path)
    rec.note("x")
    rec.close()
    for call in (lambda: rec.note("late"), lambda: rec.heading(1, "t"), lambda: rec.close(),
                 lambda: rec.capture(rec.armed(), 1000, interleaved=bytes(8), names=["P0"])):
        with pytest.raises(RuntimeError, match="closed"):
            call()


def test_a_whole_capture_is_added_with_its_parts_and_losses(tmp_path):
    from fractions import Fraction
    from wireskein import capture as wc, verify
    from wireskein.runlog import Recorder, pulses
    a = wc.Channel("P0", wc.pack(bytes([0, 1, 0, 0] * 4)), 16)
    cap = wc.Capture(Fraction(1000), [wc.Part(16, [a], {"start_ns": 1}, 0), wc.Part(16, [a], {"start_ns": 2}, 40)],
                     lost=[{"after_part": 0, "reason": "probe dropped a segment", "ticks": 24}])
    rec = Recorder(tmp_path / "run")
    with rec.section(1, "t", expect=[pulses("P0", count=4)]):
        name = rec.add(rec.armed(), cap)
    path = rec.close()
    assert wc.read(path.parent / name).lost[0]["ticks"] == 24
    rep = verify.verify(path.parent)
    assert any(r["status"] == "ng" and "lost" in r["reason"] for r in rep["results"])     # a loss fails the run
