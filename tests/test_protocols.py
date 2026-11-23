"""Protocols of one's own on a bus WireSkein decodes, declared in a definitions directory (protocols/*.toml), with
the example demo_link (examples/protocols/demo_link.toml): the signals made here, by an encoder apart from the
declaration that reads them, are the truth."""

import json
from pathlib import Path

import numpy as np
import pytest

from wireskein import capture as wc, plugins
from wireskein._engine import gen
from wireskein.decode import analyze_file

EXAMPLES = Path(__file__).parents[1] / "examples"
RATE = 2e6


def crc8(data: bytes) -> int:
    crc = 0
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = ((crc << 1) ^ 0x07) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


def frame(seq: int, cmd: int, payload: bytes = b"") -> bytes:
    """demo_link's frame as its sender makes it: 0xA5 | len | seq | cmd | payload | crc8."""
    body = bytes([len(payload), seq & 0xFF, cmd & 0xFF]) + payload
    return bytes([0xA5]) + body + bytes([crc8(body)])


def sent_frames(data: bytes) -> list[tuple[int, int, str]]:
    """(seq, cmd, payload) of the frames a sender put one after another (stray bytes before 0xA5 left out): the truth
    of a capture."""
    out, i = [], 0
    while i < len(data):
        if data[i] != 0xA5:
            i += 1
            continue
        n = data[i + 1]
        out.append((data[i + 2], data[i + 3] & 0x7F, data[i + 4:i + 4 + n].hex()))
        i += 5 + n
    return out


@pytest.fixture
def demo_link():
    plugins.add_definitions(EXAMPLES)
    yield
    plugins.remove_definitions(EXAMPLES)


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


REQS = [frame(1, 0x10, b"\x01\x02"), frame(2, 0x11), frame(3, 0x12, bytes(range(8))), frame(4, 0x10, b"\xff")]


def test_frames_over_a_uart_are_read_with_their_crcs(tmp_path, demo_link):
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, b"\x00\x13" + b"".join(REQS), 115200)      # two stray bytes before: it resynchronizes
    doc = analyze_file(write(tmp_path, {"TX": tx}, t + 200e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    got = c["result"]["examples.demo_link"]
    assert got["checks_passed"] == 4 and got["checks_failed"] == 0, got
    assert [(m["seq"], m["cmd"], m["payload"]) for m in got["messages"]] == [
        (1, 0x10, "0102"), (2, 0x11, ""), (3, 0x12, "0001020304050607"), (4, 0x10, "ff")]
    assert all(isinstance(m["s"], int) and m["s"] > 0 for m in got["messages"])


def test_frames_over_spi_on_both_lines(tmp_path, demo_link):
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    rep = [frame(1, 0x90, b"\x00"), frame(2, 0x91)]   # each no longer than its request
    frames = [(REQS[0], rep[0].ljust(len(REQS[0]), b"\xff")), (REQS[1], rep[1].ljust(len(REQS[1]), b"\xff"))]
    t = gen.spi(clk, mosi, miso, cs, 10e-6, frames, 250e3, mode=0)
    doc = analyze_file(write(tmp_path, {"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, t + 50e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "spi"]
    got = c["result"]["examples.demo_link"]
    assert got["checks_passed"] == 4, got
    assert sorted((m["line"], m["seq"], m["reply"]) for m in got["messages"]) == [
        ("MISO", 1, True), ("MISO", 2, True), ("MOSI", 1, False), ("MOSI", 2, False)]


def test_bytes_that_are_not_its_frames_show_nothing(tmp_path, demo_link):
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, b"hello, world\r\n" * 4, 115200)
    doc = analyze_file(write(tmp_path, {"TX": tx}, t + 200e-6))
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    assert "examples.demo_link" not in json.dumps(c)                  # no CRC held: not shown, not the final reading


DECL = """format = "wireskein-protocol/1"
name = "{name}"
over = {over}

[frame]
fields = [{{ name = "sof", equals = 0x7E }}, {{ name = "v" }}, {{ name = "c", check = {{ kind = "{kind}", over = ["v"] }} }}]
"""


@pytest.mark.parametrize("name, over, kind, why", [
    ("lines", '["uart"]', "xor8", "owner.name"),                 # a name of WireSkein's own
    ("tests.x", '["not_a_bus"]', "xor8", "over names buses"),
    ("tests.x", '["uart_pair"]', "xor8", "not uart_pair"),
    ("tests.x", '["uart"]', "crc7_unknown", "check kind"),
])
def test_a_declaration_in_error_is_refused_when_added(tmp_path, name, over, kind, why):
    (tmp_path / "protocols").mkdir()
    (tmp_path / "protocols/x.toml").write_text(DECL.format(name=name, over=over, kind=kind))
    with pytest.raises(ValueError, match=why):
        plugins.add_definitions(tmp_path)
    plugins.remove_definitions(tmp_path)


def test_a_protocol_declared_twice_is_refused(tmp_path):
    (tmp_path / "protocols").mkdir()
    for f in ("a", "b"):
        (tmp_path / f"protocols/{f}.toml").write_text(DECL.format(name="tests.x", over='["uart"]', kind="xor8"))
    with pytest.raises(ValueError, match="declared twice"):
        plugins.add_definitions(tmp_path)
    plugins.remove_definitions(tmp_path)


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
        with rec.section(1, "t", expect=[protocol("examples.demo_link", "uart", {"data": "TX"}, messages=want, baud=115200)]):
            rec.capture(rec.armed(), RATE, interleaved=lv.tobytes(), names=["TX"])
        rec.close()
        (r,) = verify.verify(tmp_path / name)["results"]
        assert r["status"] == status, (r["reason"], r["measured"])
    assert "message #1: want {'seq': 2, 'cmd': 18}" in r["reason"], r["reason"]


# ---- a protocol table the current engine reads through its internal hook (no extension point: docs/protocols.md
# §5): tests/data/protocol_tables/demo_wire.toml ----

DEFS = Path(__file__).parent / "data/protocol_tables"


@pytest.fixture
def demo_wire():
    from wireskein._engine import staged
    staged.PROTOCOL_TABLES.append(DEFS)
    yield
    staged.PROTOCOL_TABLES.remove(DEFS)


def test_a_definitions_directory_with_protocol_tables_is_refused():
    with pytest.raises(ValueError, match="not an extension point"):
        plugins.add_definitions(DEFS)


def wire_bits(addr: int, cmd: int, data: bytes) -> list[int]:
    """demo_wire's frame as its sender puts it on the line, written here apart from the definition that reads it."""
    msb = lambda v, n: [(v >> (n - 1 - i)) & 1 for i in range(n)]      # noqa: E731
    body = msb(addr, 7) + msb(cmd, 4) + msb(len(data), 4) + [b for x in data for b in msb(x, 8)]
    crc = 0
    for b in body:                                       # CRC-8, polynomial 0x07, initial 0, bit by bit
        top = (crc >> 7) & 1
        crc = (crc << 1) & 0xFF
        if top ^ b:
            crc ^= 0x07
    raw, out, run, last = [0] + body + msb(crc, 8), [], 0, None
    for b in raw:                                        # stuffing after five equal bits, start bit to CRC
        out.append(b)
        run = run + 1 if b == last else 1
        last = b
        if run == 5:
            out.append(1 - b)
            last, run = 1 - b, 1
    return out + [1]                                     # the stop bit


WIRE = [(0x21, 0x3, b"\x00\x00\x00"), (0x21, 0x4, b""), (0x7F, 0xF, bytes([0xFF] * 4)), (0x05, 0x1, b"\x5a\xa5")]


def test_a_declared_protocol_from_outside_is_read(tmp_path, demo_wire):
    w, bt = gen.Wave(1), 1 / 50e3
    t = 20 * bt                                          # the line idles before the first frame, as between frames
    for f in WIRE:
        for b in wire_bits(*f):
            w.set(t, b)
            t += bt
        t += 14 * bt                                     # idle between frames
    doc = analyze_file(write(tmp_path, {"W": w}, t + 20 * bt))
    (c,) = [c for c in claims(doc) if c["protocol"] == "tests.demo_wire"]
    frames = c["result"]["frames"]
    assert [(f["addr"], f["cmd"], f["data"]) for f in frames] == [
        (0x21, 3, "000000"), (0x21, 4, ""), (0x7F, 15, "ffffffff"), (0x05, 1, "5aa5")], frames
    assert c["verdict"] in ("confirmed", "likely"), c


def test_a_declared_protocol_is_not_found_without_its_definitions(tmp_path):
    w, t, bt = gen.Wave(1), 40e-6, 1 / 50e3
    for b in wire_bits(0x21, 0x3, b"\x01"):
        w.set(t, b)
        t += bt
    doc = analyze_file(write(tmp_path, {"W": w}, t + 20 * bt))
    assert not [c for c in claims(doc) if c["protocol"] == "tests.demo_wire"]


def test_a_test_checks_the_frames_of_a_declared_protocol(tmp_path, demo_wire):
    from wireskein import verify
    from wireskein.runlog import Recorder, protocol
    w, bt = gen.Wave(1), 1 / 50e3
    t = 20 * bt
    for f in WIRE[:2]:
        for b in wire_bits(*f):
            w.set(t, b)
            t += bt
        t += 14 * bt
    cap = gen.quantize({"W": w}, RATE, t + 20 * bt, np.random.default_rng(0))
    lv = cap.channels[0].level_at(np.arange(cap.n_samples)).astype(np.uint8)
    for name, want, status in (("right", [{"addr": 0x21, "cmd": 3, "data": "000000"}, {"cmd": 4}], "ok"),
                               ("wrong", [{"addr": 0x21, "cmd": 3}, {"cmd": 5}], "ng")):
        rec = Recorder(tmp_path / name)
        with rec.section(1, "t", expect=[protocol("tests.demo_wire", None, {"data": "W"}, messages=want)]):
            rec.capture(rec.armed(), RATE, interleaved=lv.tobytes(), names=["W"])
        rec.close()
        (r,) = verify.verify(tmp_path / name)["results"]
        assert r["status"] == status, (r["reason"], r["measured"])
        assert r["measured"]["checks_passed"] == 2


def test_device_packs_and_marker_dialects_from_a_definitions_directory(tmp_path):
    import shutil
    from wireskein._engine import markers
    root = Path(__file__).parents[1] / "src/wireskein/decl"
    pack = next(iter(sorted((root / "devices/i2c").rglob("*.toml"))))
    (tmp_path / "devices/i2c/mine").mkdir(parents=True)
    shutil.copy(pack, tmp_path / "devices/i2c/mine/copy.toml")
    (tmp_path / "markers").mkdir()
    (tmp_path / "markers/mine.toml").write_text(
        'format = "wireskein-marker-dialect/1"\n\n[dialect]\nname = "tests.mine"\nlevels = ["case"]\n\n[[marker]]\npattern = \'^BEGIN (?P<name>.+)$\'\naction = "begin"\n')
    from wireskein import _upper
    before = len(_upper._device_packs(("**",))[0])
    plugins.add_definitions(tmp_path)
    try:
        assert len(_upper._device_packs(("**",))[0]) == before + 1
        assert "tests.mine" in markers.load_dialects() and "heading" in markers.load_dialects()
    finally:
        plugins.remove_definitions(tmp_path)
    assert len(_upper._device_packs(("**",))[0]) == before


def test_the_example_on_hardware_sent_by_a_probe(demo_link):
    # an OEP probe (ESP32-P4) sent the frames with ESP-IDF's UART driver and captured its own pin: the truth is
    # what it sent (demo_link_p4.truth.json), not a device under test. The same capture's SPI comes from the probe's
    # own pin-driving controller, not an established library, so it is no truth and is not checked here
    d = Path(__file__).parents[1] / "examples/protocols"
    truth = json.loads((d / "demo_link_p4.truth.json").read_text())
    assert truth["uart"]["interface"].startswith("oep.fixture.uart (ESP-IDF UART driver)")
    doc = analyze_file(d / "demo_link_p4.wireskein")
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    want = sent_frames(bytes.fromhex(truth["uart"]["bytes"]))
    got = c["result"]["examples.demo_link"]
    assert c["verdict"] == "confirmed" and got["checks_failed"] == 0, got
    assert [(m["seq"], m["cmd"], m["payload"]) for m in got["messages"]] == want


def test_its_messages_are_annotations_and_markers_for_the_viewer(tmp_path, demo_link):
    from wireskein import annotate, capture as wc2
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, b"".join(REQS[:2]), 115200)
    p = write(tmp_path, {"TX": tx}, t + 200e-6)
    rows = annotate.build_file(p)["rows"]
    (row,) = [r for r in rows if r["name"] == "uart examples.demo_link"]
    assert [it["detail"]["seq"] for it in row["items"]] == [1, 2] and row["near"] == "TX"
    wc2.add_markers(p, [{"t": it["s"], "label": f"seq {it['detail']['seq']}"} for it in row["items"]], by="my_tool")
    wc2.add_markers(p, [{"t": row["items"][0]["s"], "label": "again"}], by="my_tool")      # replaces its own
    assert [(m["label"], m["by"]) for m in wc2.markers(p)] == [("again", "my_tool")]
