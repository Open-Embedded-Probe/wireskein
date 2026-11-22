"""wireskein.capture reads every capture file as one type (a WireSkein file, a .sr, a VCD) and writes by the name;
wireskein.decode analyses what it read, a part or a one-part capture. Lines made here."""

import numpy as np
import pytest

from wireskein import capture, decode
from fractions import Fraction


def uart(byte_values, baud=115_200, rate=2_000_000, idle=200):
    bit = rate / baud
    edges, lv, t = [], 1, float(idle)
    for b in byte_values:
        bits = [0] + [(b >> i) & 1 for i in range(8)] + [1, 1]
        for x in bits:
            if x != lv:
                edges.append(int(round(t)))
                lv = x
            t += bit
        t += 3 * bit
    n = int(t) + idle
    return capture.Capture(Fraction(rate), [capture.Part(n, [capture.logic("TX", 1, edges, n)])])


@pytest.mark.parametrize("suffix", [".wireskein", ".sr", ".vcd"])
def test_each_kind_reads_as_a_capture_and_analyses_alike(tmp_path, suffix):
    cap = uart(b"hello, wireskein " * 8)
    out = capture.write(tmp_path / f"c{suffix}", cap)
    back = capture.read(out)
    assert isinstance(back, capture.Capture) and len(back.parts) == 1
    (p,) = back.parts
    v = p.view()
    assert v.rate_hz == 2_000_000 and v.n_samples >= cap.parts[0].ticks
    initial, edges = p.channel("TX").changes()
    assert initial == 1 and np.array_equal(edges, v.channel("TX").edges)
    doc = decode.export(decode.analyze(back), back)
    (claim,) = [c for c in doc["claims"] if c["protocol"] == "uart"]
    assert claim["verdict"] == "confirmed" and claim["params"]["baud"] == pytest.approx(115_200, rel=0.02)


def test_a_part_and_its_view_are_analysed_the_same(tmp_path):
    cap = uart(b"abc" * 30)
    a = decode.export(decode.analyze(cap.parts[0]), cap.parts[0])
    b = decode.export(decode.analyze(cap.parts[0].view()), cap)
    assert [c["protocol"] for c in a["claims"]] == [c["protocol"] for c in b["claims"]] == ["uart"]


def test_a_vcd_holds_one_part(tmp_path):
    two = capture.Capture(Fraction(1000), [capture.Part(10, [capture.logic("A", 0, [3], 10)], tick0=0),
                                           capture.Part(10, [capture.logic("A", 0, [5], 10)], tick0=20)])
    with pytest.raises(ValueError, match="one part"):
        capture.write(tmp_path / "x.vcd", two)


def test_decode_shows_no_types_of_its_own():
    assert not {"Capture", "Channel", "load", "save"} & set(decode.__all__)


def test_states_and_annotations_through_the_public_modules(tmp_path):
    cap = uart(b"states " * 20)
    out = capture.write(tmp_path / "s.wireskein", cap)
    lines = decode.states(capture.read(out))
    assert lines["TX"] and all("t0" in x and "t1" in x for x in lines["TX"])        # a UART: a stretch of activity
    whole = decode.states_file(out)
    assert whole["parts"][0]["lines"]["TX"] == lines["TX"]
    doc = decode.annotations_file(out)
    assert doc["format"] == "wireskein-annotations/1" and doc["rows"]
    assert capture.annotations(out) is None
    capture.write_annotations(out, doc)
    assert capture.annotations(out) == doc


def test_a_long_part_analysed_whole_says_analyze_file_splits(capsys, monkeypatch):
    monkeypatch.setattr(decode, "WHOLE_EDGES", 100)
    decode.analyze(uart(b"long " * 10))
    assert "analyze_file(path) reads a long capture in pieces" in capsys.readouterr().err
    monkeypatch.setattr(decode, "WHOLE_EDGES", 10**9)
    decode.analyze(uart(b"long " * 10))
    assert "in pieces" not in capsys.readouterr().err
