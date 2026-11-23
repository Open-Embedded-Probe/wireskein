"""Engines added beside WireSkein's own (wireskein.plugins.register_engine): their claims join on lines the main
engine left unexplained, first where a hint names their protocol, and protocols of others ride on them. Lines made
here."""

from fractions import Fraction

import pytest

from wireskein import capture, decode, plugins


def uart_and_quiet(rate=2_000_000, baud=115_200):
    """A UART on U (the main engine reads it) and a line X of a few slow pulses (nothing the main engine reads)."""
    bit = rate / baud
    edges, lv, t = [], 1, 200.0
    for b in b"hello engines " * 8:
        for x in [0] + [(b >> i) & 1 for i in range(8)] + [1, 1]:
            if x != lv:
                edges.append(int(round(t)))
                lv = x
            t += bit
        t += 3 * bit
    n = int(t) + 200
    x = [k for p in range(1000, n - 2000, n // 12) for k in (p, p + 700)]
    return capture.Capture(Fraction(rate), [capture.Part(n, [capture.logic("U", 1, edges, n),
                                                             capture.logic("X", 0, x, n)])])


def blip(view, hints):
    """tests.blip: every line of at least 8 changes is one (it reads the UART's line too, to see it refused)."""
    out = []
    for ch in view.channels:
        if len(ch.edges) >= 8:
            out.append({"protocol": "tests.blip", "roles": {"data": ch.name}, "verdict": "confirmed", "score": 0.9,
                        "margin": 0.9, "params": {}, "layers": {"pulses": [{"s": int(t)} for t in ch.edges[::2]]}})
    return out


@pytest.fixture
def engine():
    plugins.register_engine("tests.blip_engine", blip, ["tests.blip"])
    yield
    plugins.unregister_engine("tests.blip_engine")


def claims(cap, hints=None):
    return decode.export(decode.analyze(cap, hints), cap)["claims"]


def test_an_added_engine_claims_the_lines_left_unexplained(engine):
    cs = claims(uart_and_quiet())
    by = {(c["protocol"], tuple(c["roles"].values())) for c in cs}
    assert ("uart", ("U",)) in by and ("tests.blip", ("X",)) in by
    assert ("tests.blip", ("U",)) not in by                   # the main engine explained U


def test_a_hint_naming_its_protocol_puts_it_first(engine):
    cs = claims(uart_and_quiet(), {"protocols": ["tests.blip"]})
    assert {tuple(c["roles"].values()) for c in cs if c["protocol"] == "tests.blip"} == {("U",), ("X",)}
    assert not [c for c in cs if c["protocol"] == "uart"]


def test_a_protocol_rides_on_an_added_engines_protocol(engine):
    class Count:
        name, over = "tests.count", ("tests.blip",)

        def decode(self, bus):
            n = len(bus.get("pulses", []))
            return {"messages": [{"pulses": n}], "checks_passed": 3, "checks_failed": 0}
    plugins.register_protocol(Count())
    try:
        cap = uart_and_quiet()
        (c,) = [c for c in claims(cap) if c["protocol"] == "tests.blip"]
        rises = len(cap.parts[0].view().channel("X").edges[::2])
        assert c["result"]["tests.count"]["messages"] == [{"pulses": rises}] and c["score"] > 0.9
    finally:
        plugins.unregister_protocol("tests.count")


def test_an_engine_and_its_protocols_take_an_owners_namespace():
    with pytest.raises(ValueError, match="owner.name"):
        plugins.register_engine("blip", blip, ["tests.blip"])
    with pytest.raises(ValueError, match="owner.name"):
        plugins.register_engine("tests.blip_engine", blip, ["blip"])


def test_a_test_checks_an_added_engines_protocol(engine, tmp_path):
    """protocol(NAME, None, roles, messages=...) on a protocol an added engine reads: that engine's claim is checked
    (its layer's items as the messages)."""
    from wireskein import verify
    from wireskein.runlog import Recorder, protocol
    cap = uart_and_quiet()
    rises = [int(t) for t in cap.parts[0].view().channel("X").edges[::2]]
    rec = Recorder(tmp_path)
    good = protocol("tests.blip", None, {"data": "X"}, messages=[{"s": t} for t in rises])
    bad = protocol("tests.blip", None, {"data": "X"}, messages=[{"s": t + 1} for t in rises])
    with rec.section(1, "t", expect=[good, bad]):
        rec.capture(rec.armed(), 2_000_000, channels=list(cap.parts[0].channels))
    rec.close()
    ok, wrong = verify.verify(tmp_path)["results"]
    assert ok["status"] == "ok", ok
    assert wrong["status"] == "ng", wrong


def test_a_spec_names_an_added_engines_protocol(engine, tmp_path):
    """wireskein decode's spec: a bus of an added engine's protocol is read by it, the others by WireSkein's."""
    cap = uart_and_quiet()
    p = capture.write(tmp_path / "c.wireskein", cap)
    doc = decode.decode_file(p, {"buses": [{"protocol": "tests.blip", "pins": {"data": "X"}},
                                           {"protocol": "uart", "pins": {"data": "U"}}]})
    blip_bus, uart_bus = doc["parts"][0]["buses"]
    assert blip_bus["decoded"] and blip_bus["fit"]["verdict"] == "confirmed" and blip_bus["result"]["pulses"]
    assert uart_bus["decoded"] and bytes.fromhex(uart_bus["result"]["bytes"]["hex"]).startswith(b"hello engines")
