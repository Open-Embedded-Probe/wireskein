"""Capture files of other tools WireSkein reads itself (.sr, VCD), and convert: excerpts. Files written here."""


import pytest

BAUD = 115200


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
    from wireskein.decode import _load as load
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


def test_a_sr_of_sigrok_0_2_keeps_its_names_without_the_triggers(tmp_path):
    # sigrok 0.2 wrote each probe's trigger on an indented line; it is no part of the name
    import zipfile
    import numpy as np
    from wireskein._engine.srio import read_sr, read_sr_changes
    p = tmp_path / "old.sr"
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("version", "1")
        z.writestr("metadata", "[global]\nsigrok version = 0.2.1\n[device 1]\ndriver = ols\ncapturefile = logic-1\n"
                               "unitsize = 1\ntotal probes = 8\nsamplerate = 200 kHz\nprobe1 = TX\n trigger1 = 0\n"
                               "probe2 = RX\n trigger2 = 1\n")
        z.writestr("logic-1", np.array([0, 1, 3, 2], np.uint8).tobytes())
    assert [c.name for c in read_sr(p).channels] == [c.name for c in read_sr_changes(p).channels] == ["TX", "RX"]


def test_convert_an_excerpt(tmp_path):
    import argparse
    from wireskein import capture, cli
    tx, end = uart_changes(b"abcdef")
    p = tmp_path / "in.wireskein"
    from wireskein._engine.model import Capture, Channel
    from wireskein.decode import _save as save
    import numpy as np
    e = np.array([round(t * 1e6) for t, _ in tx], np.int64)
    save(p, Capture(1e6, round(end * 1e6) + 10, [Channel("TX", 1, e)]))
    out = tmp_path / "x.wireskein"
    cli.convert(argparse.Namespace(input=p, output=out, part=None, from_s=2e-4, to_s=5e-4))
    cap = capture.read(out)
    assert cap.meta["excerpt"] == {"from_tick": 200, "to_tick": 500} and cap.parts[0].ticks == 300


def test_an_excerpt_keeps_its_own_first_sample_on_the_probe_clock(tmp_path):
    # start_ns is the probe clock of the first sample (wireskein-format): an excerpt's is later by where it starts,
    # so that a run's markers still fall on the same changes
    import argparse
    from wireskein import capture, cli
    from wireskein._engine.model import Capture, Channel
    from wireskein.decode import _save as save
    import numpy as np
    p = tmp_path / "in.wireskein"
    save(p, Capture(1e6, 1000, [Channel("TX", 1, np.array([300, 600], np.int64))]), start_ns=5_000_000,
         start_uncertainty_ns=5000)
    out = tmp_path / "x.wireskein"
    cli.convert(argparse.Namespace(input=p, output=out, part=None, from_s=2e-4, to_s=5e-4))
    part = capture.read(out).parts[0]
    assert part.meta["start_ns"] == 5_200_000 and part.meta["start_uncertainty_ns"] == 5000
