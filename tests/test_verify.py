import subprocess
import sys
import xml.etree.ElementTree as ET

import pytest

import demo_run
from wireskein.runlog import Recorder, level, square
from wireskein.verify import junit, verify


@pytest.fixture(scope="module")
def demo(tmp_path_factory):
    out = tmp_path_factory.mktemp("demo")
    demo_run.main(out)
    return out


def test_demo_run_finds_the_injected_bugs(demo):
    rep = verify(demo)
    assert rep["summary"] | {"segments": 0, "captures": 0} == {"ok": 13, "ng": 4, "unchecked": 0, "measured": 0, "segments": 0, "captures": 0}
    ng = sorted((r["path"], r["check"]) for r in rep["results"] if r["ok"] is False)
    assert ng == [("test_i2c_write/case1 addr=42", "i2c"), ("test_misc/orphan-phase", "markers"),
                  ("test_pwm/duty=192", "square"), ("test_tone/440Hz", "only_moving")]
    duty = next(r for r in rep["results"] if r["path"] == "test_pwm/duty=192")
    assert abs(duty["measured"]["duty"] - 0.70) < 0.01


def test_junit(demo):
    root = ET.fromstring(junit(verify(demo)))
    assert root.get("tests") == "17" and root.get("failures") == "4"


def test_cli_exit_code(demo, tmp_path):
    r = subprocess.run([sys.executable, "-m", "wireskein", "verify", str(demo), "--junit", str(tmp_path / "r.xml"),
                        "--json", str(tmp_path / "r.json")], capture_output=True, text=True)
    assert r.returncode == 1
    assert "13 ok, 4 ng, 0 unchecked" in r.stdout
    assert (tmp_path / "r.xml").exists() and (tmp_path / "r.json").exists()


def test_missing_pin_is_unchecked(tmp_path):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[square("NOPE", 1000), level("P0", 0)]):
        rec.capture(rec.armed(), 1e6, interleaved=bytes(1000), names=["P0"])
    rec.close()
    rep = verify(tmp_path)
    assert [(r["check"], r["ok"]) for r in rep["results"]] == [("square", None), ("level", True)]


def test_time_base_slip_is_named_on_failure_only(tmp_path):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[level("P0", 0), level("P0", 1)]):
        rec.capture(rec.armed(), 1e6, interleaved=bytes(1000), names=["P0"], time_base_slipped=True)
    rec.close()
    ok, ng = verify(tmp_path)["results"]
    assert ok["ok"] is True and ok["measured"]["time_base_slipped"] and ok["reason"] == ""
    assert ng["ok"] is False and "time base slip" in ng["reason"]


def test_other_format_is_refused(tmp_path):
    (tmp_path / "run.json").write_text('{"format": "wireskein-run/999"}')
    with pytest.raises(ValueError, match="format"):
        verify(tmp_path)
