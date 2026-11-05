"""Protocols of one's own on a bus WireSkein decodes (wireskein.protocols), with the example demo_link
(examples/protocols): the signals made here, independently of any device under test, are the truth."""

import importlib.util
import json
from pathlib import Path

import numpy as np
import pytest

from wireskein import capture as wc, protocols
from wireskein._engine import gen
from wireskein.decode import analyze_file

SPEC = importlib.util.spec_from_file_location("demo_link", Path(__file__).parents[1] / "examples/protocols/demo_link.py")
demo = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(demo)
RATE = 2e6


@pytest.fixture
def demo_link():
    protocols.register(demo.PROTOCOL)
    yield demo
    protocols.unregister("demo_link")


def write(tmp_path, waves, end):
    cap = gen.quantize(waves, RATE, end, np.random.default_rng(0))
    lv = np.zeros(cap.n_samples, np.uint8)
    for k, ch in enumerate(cap.channels):
        lv |= ch.level_at(np.arange(cap.n_samples)).astype(np.uint8) << k
    p = tmp_path / "c.wireskein"
    wc.write(p, wc.Capture(int(RATE), [wc.Part(cap.n_samples, wc.from_interleaved(lv.tobytes(),
                                                                                [c.name for c in cap.channels]))]))
    return p


def claims(doc):
    (part,) = doc["parts"]
    return part["claims"]


REQS = [demo.frame(1, 0x10, b"\x01\x02"), demo.frame(2, 0x11), demo.frame(3, 0x12, bytes(range(8))),
        demo.frame(4, 0x10, b"\xff")]


def test_frames_over_a_uart_are_read_with_their_crcs(tmp_path, demo_link):
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, b"\x00\x13" + b"".join(REQS), 115200)      # two stray bytes before: it resynchronizes
    doc = analyze_file(write(tmp_path, {"TX": tx}, t + 200e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    got = c["result"]["demo_link"]
    assert got["checks_passed"] == 4 and got["checks_failed"] == 0, got
    assert [(m["seq"], m["cmd"], m["payload"]) for m in got["messages"]] == [
        (1, 0x10, "0102"), (2, 0x11, ""), (3, 0x12, "0001020304050607"), (4, 0x10, "ff")]
    assert all(isinstance(m["s"], int) and m["s"] > 0 for m in got["messages"])


def test_frames_over_spi_on_both_lines(tmp_path, demo_link):
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    rep = [demo.frame(1, 0x90, b"\x00"), demo.frame(2, 0x91)]   # each no longer than its request
    frames = [(REQS[0], rep[0].ljust(len(REQS[0]), b"\xff")), (REQS[1], rep[1].ljust(len(REQS[1]), b"\xff"))]
    t = gen.spi(clk, mosi, miso, cs, 10e-6, frames, 250e3, mode=0)
    doc = analyze_file(write(tmp_path, {"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, t + 50e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "spi"]
    got = c["result"]["demo_link"]
    assert got["checks_passed"] == 4, got
    assert sorted((m["line"], m["seq"], m["reply"]) for m in got["messages"]) == [
        ("MISO", 1, True), ("MISO", 2, True), ("MOSI", 1, False), ("MOSI", 2, False)]


def test_bytes_that_are_not_its_frames_show_nothing(tmp_path, demo_link):
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, b"hello, world\r\n" * 4, 115200)
    doc = analyze_file(write(tmp_path, {"TX": tx}, t + 200e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    assert "demo_link" not in json.dumps(c)                  # no CRC held: not shown, not the final reading


def test_a_protocol_needs_a_name_buses_and_decode():
    class Bad:
        name, over = "lines", ("uart",)
        def decode(self, bus): return {}
    with pytest.raises(ValueError):
        protocols.register(Bad())                            # a layer of WireSkein's own
    Bad.name, Bad.over = "x", ("can",)
    with pytest.raises(ValueError):
        protocols.register(Bad())


def test_a_test_checks_the_messages_of_its_protocol(tmp_path, demo_link):
    from wireskein import verify
    from wireskein.runlog import Recorder, protocol
    tx = gen.Wave(1)
    end = gen.uart(tx, 50e-6, b"".join(REQS[:2]), 115200) + 200e-6
    cap = gen.quantize({"TX": tx}, RATE, end, np.random.default_rng(0))
    lv = cap.channels[0].level_at(np.arange(cap.n_samples)).astype(np.uint8)
    for name, want, status in (("right", [{"seq": 1, "cmd": 0x10, "payload": "0102"}, {"seq": 2, "cmd": 0x11}], "ok"),
                               ("wrong", [{"seq": 1, "cmd": 0x10}, {"seq": 2, "cmd": 0x12}], "ng")):
        rec = Recorder(tmp_path / name)
        with rec.section(1, "t", expect=[protocol("demo_link", "uart", {"data": "TX"}, messages=want, baud=115200)]):
            rec.capture(rec.armed(), RATE, interleaved=lv.tobytes(), names=["TX"])
        rec.close()
        (r,) = verify.verify(tmp_path / name)["results"]
        assert r["status"] == status, r
    assert "message #1: want {'seq': 2, 'cmd': 18}" in r["reason"], r["reason"]
