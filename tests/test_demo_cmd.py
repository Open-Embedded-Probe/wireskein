"""A text command protocol on two UART lines (bus "uart_pair"), with the example demo_cmd (examples/protocols): the
lines made here, independently of any device under test, are the truth."""

import importlib.util
from pathlib import Path

import numpy as np
import pytest

from wireskein import capture as wc, protocols
from wireskein._engine import gen
from wireskein.decode import analyze_file

SPEC = importlib.util.spec_from_file_location("demo_cmd", Path(__file__).parents[1] / "examples/protocols/demo_cmd.py")
demo = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(demo)
RATE, BAUD = 2e6, 115200


@pytest.fixture
def demo_cmd():
    protocols.register(demo.PROTOCOL)
    yield demo
    protocols.unregister("demo_cmd")


def lines_of(talk, gap=300e-6):
    """talk: (side, text) in order, side "host" (on TX) or "dev" (on RX): the two lines, one after the other."""
    tx, rx, t = gen.Wave(1), gen.Wave(1), 100e-6
    for side, text in talk:
        t = gen.uart(tx if side == "host" else rx, t, text.encode(), BAUD) + gap
    return {"TX": tx, "RX": rx}, t


def write(tmp_path, waves, end):
    cap = gen.quantize(waves, RATE, end, np.random.default_rng(0))
    lv = np.zeros(cap.n_samples, np.uint8)
    for k, ch in enumerate(cap.channels):
        lv |= ch.level_at(np.arange(cap.n_samples)).astype(np.uint8) << k
    p = tmp_path / "c.wireskein"
    wc.write(p, wc.Capture(int(RATE), [wc.Part(cap.n_samples, wc.from_interleaved(lv.tobytes(),
                                                                                [c.name for c in cap.channels]))]))
    return p, cap


TALK = [("host", "#1e PING\n"), ("dev", "#1e OK\n"), ("host", "#1f LED 1\n"), ("dev", "#1f OK\n"),
        ("dev", "!BUTTON 1\n"), ("host", "#20 READ TEMP\n"), ("dev", "#20 OK 23.5\n"), ("host", "#21 MOTOR 9\n"),
        ("dev", "#21 ERR 3\n")]


def test_commands_and_answers_are_read_as_a_pair(tmp_path, demo_cmd):
    waves, end = lines_of(TALK)
    p, _ = write(tmp_path, waves, end + 200e-6)
    doc = analyze_file(p)
    (r,) = [r for r in doc["parts"][0].get("relations", []) if r["protocol"] == "demo_cmd"]
    assert r["checks_passed"] == 4 and r["checks_failed"] == 0 and set(r["roles"].values()) == {"TX", "RX"}, r
    got = [(m.get("verb") or m.get("event"), m.get("status"), m.get("values")) for m in r["messages"]]
    assert got == [("PING", "OK", []), ("LED", "OK", []), ("BUTTON", None, ["1"]), ("READ", "OK", ["23.5"]),
                   ("MOTOR", "ERR", ["3"])], got
    assert r["messages"][4]["level"] == "warn"


def test_other_text_shows_nothing(tmp_path, demo_cmd):
    waves, end = lines_of([("host", "hello\n"), ("dev", "world\n")] * 3)
    p, _ = write(tmp_path, waves, end + 200e-6)
    doc = analyze_file(p)
    assert not [r for r in doc["parts"][0].get("relations", []) if r["protocol"] == "demo_cmd"]


def test_a_wrong_answer_fails_its_check(tmp_path, demo_cmd):
    waves, end = lines_of([("host", "#01 PING\n"), ("dev", "#02 OK\n"), ("host", "#02 PING\n"), ("dev", "#02 OK\n")])
    from wireskein.verify import check_protocol
    from wireskein.runlog import protocol
    _, cap = write(tmp_path, waves, end + 200e-6)
    ok, got, why = check_protocol(cap, protocol("demo_cmd", "uart_pair", {"a": "TX", "b": "RX"}, baud=BAUD))
    assert ok is False and got["checks_failed"] == 2 and got["checks_passed"] == 1, (why, got)


def test_a_test_checks_the_exchange(tmp_path, demo_cmd):
    from wireskein.verify import check_protocol
    from wireskein.runlog import protocol
    waves, end = lines_of(TALK)
    _, cap = write(tmp_path, waves, end + 200e-6)
    want = [{"verb": "PING", "status": "OK"}, {"verb": "LED", "args": ["1"], "status": "OK"}, {"event": "BUTTON"},
            {"verb": "READ", "values": ["23.5"]}, {"verb": "MOTOR", "status": "ERR"}]
    ok, got, why = check_protocol(cap, protocol("demo_cmd", "uart_pair", {"a": "TX", "b": "RX"}, messages=want,
                                                baud=BAUD))
    assert ok is True, (why, got)
    want[3] = {"verb": "READ", "values": ["24.0"]}
    ok, _, why = check_protocol(cap, protocol("demo_cmd", "uart_pair", {"a": "TX", "b": "RX"}, messages=want, baud=BAUD))
    assert ok is False and "message #3" in why
