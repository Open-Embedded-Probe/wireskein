"""File formats added from outside (wireskein.formats), with the example saleae_csv (examples/formats): the file
is written here, from lines made apart from the reader."""

import importlib.util
from pathlib import Path

import pytest

from wireskein import formats
from wireskein.decode import analyze_file, load

SPEC = importlib.util.spec_from_file_location("saleae_csv", Path(__file__).parents[1] / "examples/formats/saleae_csv.py")
demo = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(demo)
BAUD = 115200


@pytest.fixture
def saleae():
    formats.register(demo.FORMAT)
    yield demo
    formats.unregister("saleae_csv")


def uart_changes(data, t0=200e-6):
    """(time, level) of a UART line sending data, 8N1 at BAUD, idle high."""
    bt, t, out, lv = 1 / BAUD, t0, [], 1
    for b in data:
        for bit in [0] + [(b >> k) & 1 for k in range(8)] + [1]:
            if bit != lv:
                out.append((t, bit))
                lv = bit
            t += bt
    return out, t + 20 * bt


def csv_text(lines: dict, end: float) -> str:
    """Saleae's digital CSV: a row at the start, a row at each change with every channel's level after it."""
    names = list(lines)
    ev = sorted({t for ch in lines.values() for t, _ in ch} | {end})
    lv = {n: 1 for n in names}
    rows = ["Time [s]," + ",".join(names), "0.000000000," + ",".join(str(lv[n]) for n in names)]
    for t in ev:
        for n in names:
            for tt, v in lines[n]:
                if tt == t:
                    lv[n] = v
        rows.append(f"{t:.9f}," + ",".join(str(lv[n]) for n in names))
    return "\n".join(rows) + "\n"


def test_a_csv_is_read_and_analysed(tmp_path, saleae):
    tx, end = uart_changes(b"hello, saleae\r\n" * 3)
    p = tmp_path / "export.csv"
    p.write_text(csv_text({"Channel 0": tx, "Channel 1": []}, end))
    assert formats.kind(p) == "saleae_csv"
    cap = load(p)
    assert cap.meta["format"] == "saleae_csv" and cap.rate == 1e9
    e = cap.channel("Channel 0").edges
    assert len(e) == len(tx) and abs(int(e[0]) - round(tx[0][0] * 1e9)) <= 1
    doc = analyze_file(p)
    (c,) = [c for c in doc["parts"][0]["claims"] if c["protocol"] == "uart"]
    assert c["roles"] == {"data": "Channel 0"} and "hello, saleae" in str(c["result"]), c


def test_without_the_format_the_file_is_not_read(tmp_path):
    p = tmp_path / "export.csv"
    p.write_text("Time [s],Channel 0\n0.0,1\n")
    assert formats.kind(p) is None
    with pytest.raises(ValueError, match="not a WireSkein file"):
        load(p)


def test_convert_keeps_it_as_a_wireskein_file(tmp_path, saleae):
    import argparse
    from wireskein import capture, cli
    tx, end = uart_changes(b"abc")
    p = tmp_path / "export.csv"
    p.write_text(csv_text({"Channel 0": tx}, end))
    cli.convert(argparse.Namespace(input=p, output=tmp_path / "out.wireskein", part=None))
    cap = capture.read(tmp_path / "out.wireskein")
    init, edges = cap.parts[0].channel("Channel 0").changes()
    assert init == 1 and len(edges) == len(tx)


def test_a_format_needs_a_name_sniff_and_read():
    class Bad:
        name = "vcd"
        def sniff(self, head, path): return False
        def read(self, path): return None
    with pytest.raises(ValueError):
        formats.register(Bad())
    Bad.name = "x"
    del Bad.read
    with pytest.raises(ValueError):
        formats.register(Bad())
