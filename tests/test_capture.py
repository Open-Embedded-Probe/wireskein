"""wireskein.capture: a WireSkein file as one value, parts and lost with it, channels read when asked for."""

from fractions import Fraction

import numpy as np
import pytest

from wireskein import analyze, capture, fileformat


def _file(path):
    a = fileformat.Channel("CLK", fileformat.pack(bytes([0, 1] * 50)), 100)
    b = fileformat.Channel("CLK", fileformat.pack(bytes([1] * 40 + [0] * 60)), 100)
    parts = [fileformat.Part([a], {"start_ns": 1000}, 0), fileformat.Part([b], {"start_ns": 500_000}, 400)]
    return fileformat.write(path, 1_000_000, parts=parts,
                            lost=[{"after_part": 0, "reason": "probe dropped a segment (no room)", "ticks": 300}])


def test_read_gives_every_part_with_the_loss(tmp_path):
    cap = capture.read(_file(tmp_path / "c.wireskein"))
    assert cap.tick_hz == Fraction(1_000_000) and cap.layout == "timeline" and not cap.complete
    assert cap.lost[0]["ticks"] == 300 and [p.tick0 for p in cap.parts] == [0, 400]
    p0, p1 = cap.parts
    assert (p0.ticks, p0.meta["start_ns"], p1.meta["start_ns"]) == (100, 1000, 500_000)
    assert p0.names == ["CLK"] and p0._file[3] is None             # nothing read yet
    clk = p0.channel("CLK")
    assert clk.initial == 0 and clk.edges[:3].tolist() == [1, 2, 3] and len(clk.edges) == 99
    assert p1.channel("CLK").edges.tolist() == [40]
    with pytest.raises(KeyError):
        p0.channel("SDA")
    v = p1.view()                                                    # what analyze and verify read
    assert v.rate == 1e6 and v.n_samples == 100 and v.meta["part"] == 1 and v.meta["complete"] is False


def test_write_from_values_and_copy(tmp_path):
    clk = capture.Channel("CLK", 0, np.arange(10, 1000, 10, dtype=np.int64))
    cap = capture.Capture(Fraction(8_000_000, 3), [capture.Part(1000, [clk], {"start_ns": 7})])
    out = capture.read(capture.write(tmp_path / "w.wireskein", cap))
    assert out.tick_hz == Fraction(8_000_000, 3) and out.complete and out.parts[0].meta == {"start_ns": 7}
    assert out.parts[0].channel("CLK").edges.tolist() == clk.edges.tolist()
    src = capture.read(_file(tmp_path / "c.wireskein"))
    again = capture.read(capture.write(tmp_path / "copy.wireskein", src))      # stored channels copied as they are
    assert again.lost == src.lost and again.id == src.id
    assert again.parts[1].channel("CLK").edges.tolist() == [40]
    assert analyze.load(tmp_path / "copy.wireskein", part=0).channel("CLK").edges.tolist()[:2] == [1, 2]


def test_a_part_in_memory_is_viewed_at_its_captures_rate():
    clk = capture.Channel("CLK", 0, np.array([5, 9], np.int64))
    cap = capture.Capture(Fraction(2_000_000), [capture.Part(20, [clk])])
    v = cap.parts[0].view()
    assert v.rate == 2e6 and v.n_samples == 20 and v.channel("CLK").edges.tolist() == [5, 9]
