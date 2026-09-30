import json
import subprocess
import sys

from wireskein import fileformat
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
    assert c["file"] == "c0001.wireskein" and c["channels"] == ["P0"] and 0 <= c["t0"] < 5
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
