"""wireskein.capture: a WireSkein file as one value, parts and lost with it, channels as stored, read as changes."""

import os
import subprocess
import sys
from pathlib import Path
from fractions import Fraction

import numpy as np
import pytest

from wireskein import analyze, capture


def _file(path):
    a = capture.Channel("CLK", capture.pack(bytes([0, 1] * 50)), 100)
    b = capture.Channel("CLK", capture.pack(bytes([1] * 40 + [0] * 60)), 100)
    parts = [capture.Part(100, [a], {"start_ns": 1000}, 0), capture.Part(100, [b], {"start_ns": 500_000}, 400)]
    lost = [{"after_part": 0, "reason": "probe dropped a segment (no room)", "ticks": 300}]
    return capture.write(path, capture.Capture(1_000_000, parts, lost=lost))


def test_read_gives_every_part_with_the_loss(tmp_path):
    cap = capture.read(_file(tmp_path / "c.wireskein"))
    assert cap.tick_hz == Fraction(1_000_000) and cap.layout == "timeline" and not cap.complete
    assert cap.lost[0]["ticks"] == 300 and [p.tick0 for p in cap.parts] == [0, 400]
    p0, p1 = cap.parts
    assert (p0.ticks, p0.meta["start_ns"], p1.meta["start_ns"]) == (100, 1000, 500_000)
    assert p0._channels is None and p0.names == ["CLK"]                 # read when first asked for
    initial, edges = p0.channel("CLK").changes()
    assert initial == 0 and edges[:3].tolist() == [1, 2, 3] and len(edges) == 99
    assert p1.channel("CLK").changes()[1].tolist() == [40]
    with pytest.raises(KeyError):
        p0.channel("SDA")
    v = p1.view()                                                        # what analyze and verify read
    assert v.rate == 1e6 and v.n_samples == 100 and v.meta["part"] == 1 and v.meta["complete"] is False


def test_a_line_from_its_changes_is_kept_in_the_smaller_form(tmp_path):
    quiet = capture.logic("DRDY", 0, [500], n=100_000)
    busy = capture.logic("CLK", 0, np.arange(1, 1000), n=1000)
    assert isinstance(quiet, capture.Events) and isinstance(busy, capture.Channel)
    cap = capture.Capture(Fraction(8_000_000, 3), [capture.Part(100_000, [quiet, busy], {"start_ns": 7})])
    out = capture.read(capture.write(tmp_path / "w.wireskein", cap))
    assert out.tick_hz == Fraction(8_000_000, 3) and out.complete and out.parts[0].meta == {"start_ns": 7}
    assert out.parts[0].channel("DRDY").changes()[1].tolist() == [500]
    assert out.parts[0].channel("CLK").changes()[1].tolist() == list(range(1, 1000))
    v = cap.parts[0].view()                                              # a part in memory, at its capture's rate
    assert v.rate == 8e6 / 3 and v.channel("DRDY").edges.tolist() == [500]
    copy = capture.read(capture.write(tmp_path / "copy.wireskein", capture.read(_file(tmp_path / "c.wireskein"))))
    assert copy.lost[0]["ticks"] == 300 and copy.parts[1].channel("CLK").changes()[1].tolist() == [40]
    assert analyze.load(tmp_path / "copy.wireskein", part=0).channel("CLK").edges.tolist()[:2] == [1, 2]


def test_events_whose_quanta_hide_changes_are_read_with_decode():
    e = capture.events("X", [3, 7], [0, 1], [2, 1], n=10, step=4)
    with pytest.raises(ValueError, match="more than one change"):
        e.changes()
    assert e.decode() == ([3, 7], [0, 1], [2, 1])


def test_reading_and_writing_need_only_the_standard_library(tmp_path):
    p = _file(tmp_path / "c.wireskein")
    code = ("import sys; sys.modules['numpy'] = None\n"
            "from wireskein import capture\n"
            f"cap = capture.read({str(p)!r})\n"
            "ch = cap.parts[1].channel('CLK'); assert ch.n == 100\n"
            f"capture.write({str(tmp_path / 'o.wireskein')!r}, cap)\n")
    src = str(Path(__file__).parents[1] / "src")
    subprocess.run([sys.executable, "-c", code], check=True, cwd=tmp_path, env={**os.environ, "PYTHONPATH": src})
