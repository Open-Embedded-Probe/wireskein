"""A protocol on CAN and LIN frames (and on a protocol of one's own definition), with the example demo_telemetry
(examples/protocols): the lines are made here by encoders apart from the readers."""

import importlib.util
from fractions import Fraction
from pathlib import Path

import numpy as np
import pytest

from wireskein import capture as wc, protocols
from wireskein.decode import analyze_file

SPEC = importlib.util.spec_from_file_location("demo_telemetry",
                                              Path(__file__).parents[1] / "examples/protocols/demo_telemetry.py")
demo = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(demo)


@pytest.fixture
def telemetry():
    protocols.register(demo.PROTOCOL)
    yield demo
    protocols.unregister("demo_telemetry")


def write(tmp_path, rate, lines: dict, n: int):
    """lines: name -> (initial, edges in ticks)."""
    chans = [wc.logic(name, init, edges, n) for name, (init, edges) in lines.items()]
    return wc.write(tmp_path / "c.wireskein", wc.Capture(Fraction(int(rate)), [wc.Part(n, chans)]))


def edges_of(segs, rate):
    t, edges, lvl = 0.0, [], 1
    for level, d in segs:
        if level != lvl:
            edges.append(int(round(t * rate)))
            lvl = level
        t += d
    return edges, int(round(t * rate))


# CAN: the test's own encoder (tests/test_can.py)
from test_can import Bus  # noqa: E402

READINGS = [(0x321, [b"\x10\x20", b"\x11\x20", b"\x12\x21"]), (0x18FF0042, [b"\x00\x00\x01\x00", b"\x00\x00\x01\x01"])]


def can_line(skip=False):
    b = Bus(500e3)
    for k in range(3):
        for ident, rs in READINGS:
            if k < len(rs):
                b.frame(ident, demo.frame(ident, k + (1 if skip and k == 2 else 0), rs[k]), ifs=20)
    b.frame(0x7FF, b"\x01\x02\x03")                          # another node's frame, not this protocol's
    cap = b.line()
    return {"CAN": (1, cap.channels[0].edges.tolist())}, cap.n_samples


def test_on_can_frames(tmp_path, telemetry):
    lines, n = can_line()
    doc = analyze_file(write(tmp_path, 20e6, lines, n))
    (c,) = [c for c in doc["parts"][0]["claims"] if c["protocol"] == "can"]
    got = c["result"]["demo_telemetry"]
    assert (got["checks_passed"], got["checks_failed"]) == (5, 1), got     # 0x7FF's last byte is no check
    assert [(m["id"], m["seq"], m["readings"]) for m in got["messages"]] == [
        (0x321, 0, "1020"), (0x18FF0042, 0, "00000100"), (0x321, 1, "1120"), (0x18FF0042, 1, "00000101"),
        (0x321, 2, "1221")]


def test_a_skipped_seq_is_shown(tmp_path, telemetry):
    lines, n = can_line(skip=True)
    doc = analyze_file(write(tmp_path, 20e6, lines, n))
    (c,) = [c for c in doc["parts"][0]["claims"] if c["protocol"] == "can"]
    assert [m["level"] for m in c["result"]["demo_telemetry"]["messages"]][-1] == "warn"


# LIN: an encoder of this test's own (break, delimiter, sync, protected identifier, data, enhanced checksum)

def lin_pid(i):
    p0 = (i ^ (i >> 1) ^ (i >> 2) ^ (i >> 4)) & 1
    p1 = 1 - (((i >> 1) ^ (i >> 3) ^ (i >> 4) ^ (i >> 5)) & 1)
    return i | (p0 << 6) | (p1 << 7)


def lin_frame(ident, data, baud=19200):
    bt = 1 / baud
    pid = lin_pid(ident)
    s = pid + sum(data)
    while s > 0xFF:
        s = (s & 0xFF) + (s >> 8)
    out = [(0, 13 * bt), (1, bt)]
    for b in [0x55, pid, *data, 0xFF ^ s]:
        out += [(0, bt)] + [((b >> k) & 1, bt) for k in range(8)] + [(1, bt)]
    return out + [(1, 20 * bt)]


def test_on_lin_frames(tmp_path, telemetry):
    segs = [(1, 1e-3)]
    for k in range(3):
        segs += lin_frame(0x21, demo.frame(0x21, k, bytes([k, 0x40])))
    edges, n = edges_of(segs + [(1, 1e-3)], 2e6)
    doc = analyze_file(write(tmp_path, 2e6, {"LIN": (1, edges)}, n))
    (c,) = [c for c in doc["parts"][0]["claims"] if c["protocol"] == "lin"]
    got = c["result"]["demo_telemetry"]
    assert got["checks_passed"] == 3 and [m["seq"] for m in got["messages"]] == [0, 1, 2], got


def test_a_test_checks_it_on_can(tmp_path, telemetry):
    from wireskein.verify import check_protocol
    from wireskein.runlog import protocol
    lines, n = can_line()
    cap = wc.read(write(tmp_path, 20e6, lines, n)).parts[0].view()
    x = protocol("demo_telemetry", "can", {"data": "CAN"}, checks_failed_max=1,
                 messages=[{"id": 0x321, "seq": 0}, {"id": 0x18FF0042, "seq": 0}, {"id": 0x321, "seq": 1},
                           {"id": 0x18FF0042, "seq": 1}, {"id": 0x321, "seq": 2, "readings": "1221"}])
    ok, got, why = check_protocol(cap, x)
    assert ok is True, (why, got)
