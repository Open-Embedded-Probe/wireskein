"""wireskein-run/3 and the check statuses (docs/run-format.ja.md)."""

import json
import subprocess
import sys

import pytest

from wireskein import fileformat
from wireskein.runlog import FORMAT, Recorder, level, pulses, uart, voltage
from wireskein.verify import failed, junit, verify


def cap(rec, armed, n=100):
    rec.capture(armed, 1_000_000, channels=[fileformat.Channel("P0", fileformat.pack(bytes(n)), n)])


def test_a_capture_belongs_to_the_section_it_was_taken_in(tmp_path):
    rec = Recorder(tmp_path)
    armed = rec.armed()                                  # armed before the section opened
    with rec.section(1, "t", expect=[level("P0", 0)]):
        cap(rec, armed)
    rec.close()
    doc = json.loads((tmp_path / "run.json").read_text())
    assert doc["format"] == FORMAT == "wireskein-run/3"
    assert doc["captures"][0]["path"] == "t"
    assert [r["status"] for r in verify(tmp_path)["results"]] == ["ok"]      # by time it would have missed


def test_adding_a_repeat_keeps_the_earlier_keys(tmp_path):
    def run(d, repeats):
        rec = Recorder(d)
        for k in range(repeats):
            with rec.section(1, "duty=64", expect=[level("P0", 0)]):
                cap(rec, rec.armed())
        rec.close()
        return list(json.loads((d / "run.json").read_text())["expect"])
    assert run(tmp_path / "a", 1) == ["duty=64"]
    assert run(tmp_path / "b", 3) == ["duty=64", "duty=64[1]", "duty=64[2]"]


def test_unchecked_fails_unless_allowed_measured_does_not(tmp_path):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[level("P0", 0), level("NOPE", 0)]):
        cap(rec, rec.armed())
    rec.close()
    rep = verify(tmp_path)
    assert [r["status"] for r in rep["results"]] == ["ok", "unchecked"]
    assert rep["summary"]["unchecked"] == 1
    assert failed(rep) and not failed(rep, allow_unchecked=True)
    x = junit(rep)
    assert 'failures="1"' in x and "unchecked: " in x
    assert 'skipped="1"' in junit(rep, allow_unchecked=True)
    r = subprocess.run([sys.executable, "-m", "wireskein", "verify", str(tmp_path)], capture_output=True, text=True)
    assert r.returncode == 1 and "1 unchecked" in r.stdout
    r = subprocess.run([sys.executable, "-m", "wireskein", "verify", str(tmp_path), "--allow-unchecked"],
                       capture_output=True, text=True)
    assert r.returncode == 0


def test_old_names_fail_loudly():
    with pytest.raises(TypeError):
        pulses("P0", tol=0.1)
    with pytest.raises(TypeError):
        voltage("V", 3.3, tol=0.1)
    rec_dir = None
    with pytest.raises(TypeError):
        with Recorder(__import__("tempfile").mkdtemp()).section(1, "t", expect=[], deadline=5):
            pass
    assert rec_dir is None


def test_is_empty(tmp_path):
    rec = Recorder(tmp_path)
    assert rec.is_empty
    with rec.section(1, "t", expect=[level("P0", 0)]):
        pass
    assert not rec.is_empty


def test_measure_only_is_measured_and_passes(tmp_path):
    import numpy as np
    rate, baud = 1_000_000, 115_200
    bits = [1] * 300
    for byte in b"measure me\n" * 4:
        bits += [0] + [(byte >> k) & 1 for k in range(8)] + [1, 1]
    bits += [1] * 300
    per = rate / baud
    lv = np.array(bits, np.uint8)[(np.arange(int(len(bits) * per)) / per).astype(int)]
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[uart("TX", None)]):
        rec.capture(rec.armed(), rate, channels=[fileformat.Channel("TX", fileformat.pack(lv.tobytes()), len(lv))])
    rec.close()
    rep = verify(tmp_path)
    (r,) = rep["results"]
    assert r["status"] == "measured" and r["ok"] is None and abs(r["measured"]["baud"] / baud - 1) < 0.02
    assert not failed(rep) and "<system-out>" in junit(rep)
