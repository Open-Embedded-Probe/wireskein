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
    assert cap.meta["format"] == "saleae_csv"
    e = cap.channel("Channel 0").edges
    assert len(e) == len(tx) and abs(int(e[0]) / cap.rate - tx[0][0]) <= 1e-9
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


def test_the_tick_is_the_step_of_the_changes_and_the_last_row_is_the_end(tmp_path, saleae):
    # 10 MS/s: changes on a 100 ns grid; the end row (levels unchanged) off it, as Logic 2 writes it
    p = tmp_path / "digital.csv"
    p.write_text("Time [s],Channel 0,Channel 1\n0.000000000,0,0\n0.000151900,0,1\n0.000178400,1,1\n"
                 "0.001311200,0,1\n0.002697892,0,1\n")
    cap = load(p)
    assert cap.rate == 1e7
    assert cap.channel("Channel 0").edges.tolist() == [1784, 13112]
    assert cap.channel("Channel 1").edges.tolist() == [1519]
    assert cap.n_samples == 26979 + 1


def test_an_analog_export_is_read_as_volts(tmp_path, saleae):
    from wireskein import capture
    # 1.25 MS/s from 292 ns (Logic 2 shifts analog samples by its filter's delay)
    rows = [f"{(292 + 800 * i) / 1e9:.9f},{1.65 + (i % 2) * 1.6:.3f},{0.5:.3f}" for i in range(1000)]
    p = tmp_path / "analog.csv"
    p.write_text("Time [s],Channel 0,Channel 1\n" + "\n".join(rows) + "\n")
    assert formats.kind(p) == "saleae_csv"
    cap = formats.read(p)
    (part,) = cap.parts
    ch = {c.name: c for c in part.channels}
    a = ch["Channel 0"]
    assert isinstance(a, capture.Analog) and a.rate_hz == 1.25e6 and a.t0_ticks == 292
    assert abs(a.volts()[1] - 3.25) < 1e-6


def test_an_analog_export_not_at_one_rate_is_refused(tmp_path, saleae):
    p = tmp_path / "analog.csv"
    p.write_text("Time [s],Channel 0\n0.000000000,1.5\n0.000000800,1.6\n0.000002000,1.7\n")
    with pytest.raises(ValueError, match="not at one rate"):
        formats.read(p)


@pytest.mark.parametrize("block", [1 << 24, 101])
def test_a_sr_read_a_chunk_at_a_time_and_an_excerpt(tmp_path, monkeypatch, block):
    # a logic .sr in several chunks (as sigrok writes a long one): its changes across the seams, the excerpt's
    # ticks and its first level; the same as the whole read at once
    import zipfile
    import numpy as np
    from wireskein._engine import srio
    from wireskein._engine.srio import read_sr, read_sr_changes
    monkeypatch.setattr(srio, "BLOCK", block)     # blocks smaller than a chunk: their seams too
    rng = np.random.default_rng(7)
    data = np.repeat(rng.integers(0, 4, 3000).astype(np.uint8), rng.integers(1, 9, 3000))
    p = tmp_path / "long.sr"
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("version", "2")
        z.writestr("metadata", "[device 1]\ncapturefile=logic-1\ntotal probes=2\nsamplerate=1 MHz\n"
                               "total analog=0\nprobe1=A\nprobe2=B\nunitsize=1\n")
        for i, k in enumerate(range(0, len(data), 997)):
            z.writestr(f"logic-1-{i + 1}", data[k:k + 997].tobytes())
    whole, chunked = read_sr(p), read_sr_changes(p)
    assert chunked.n_samples == whole.n_samples == len(data)
    for x, y in zip(whole.channels, chunked.channels):
        assert x.name == y.name and x.initial == y.initial and np.array_equal(x.edges, y.edges)
    s0, s1 = 1234, 9876
    part = read_sr_changes(p, s0, s1)
    assert part.n_samples == s1 - s0
    for x, y in zip(whole.channels, part.channels):
        e = x.edges[(x.edges > s0) & (x.edges < s1)] - s0
        assert np.array_equal(e, y.edges) and y.initial == int(x.level_at(np.array([s0]))[0])
    # written straight through, a block at a time: the same lines
    from wireskein.decode import load
    out, n, names, rate = srio.convert_sr(p, tmp_path / "long.wireskein")
    back = load(out)
    assert n == len(data) and names == ["A", "B"]
    for x, y in zip(whole.channels, back.channels):
        assert x.initial == y.initial and np.array_equal(x.edges, y.edges)
    out, n, _, _ = srio.convert_sr(p, tmp_path / "ex.wireskein", s0, s1, excerpt=True)
    from wireskein import capture
    assert n == s1 - s0 and capture.read(out).meta["excerpt"] == {"from_tick": s0, "to_tick": s1}
    for x, y in zip(whole.channels, load(out).channels):
        e = x.edges[(x.edges > s0) & (x.edges < s1)] - s0
        assert np.array_equal(e, y.edges)


def test_convert_an_excerpt(tmp_path):
    import argparse
    from wireskein import capture, cli
    tx, end = uart_changes(b"abcdef")
    p = tmp_path / "in.wireskein"
    from wireskein._engine.model import Capture, Channel
    from wireskein.decode import save
    import numpy as np
    e = np.array([round(t * 1e6) for t, _ in tx], np.int64)
    save(p, Capture(1e6, round(end * 1e6) + 10, [Channel("TX", 1, e)]))
    out = tmp_path / "x.wireskein"
    cli.convert(argparse.Namespace(input=p, output=out, part=None, from_s=2e-4, to_s=5e-4))
    cap = capture.read(out)
    assert cap.meta["excerpt"] == {"from_tick": 200, "to_tick": 500} and cap.parts[0].ticks == 300
