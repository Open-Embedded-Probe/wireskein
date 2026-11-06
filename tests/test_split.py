"""Cutting a long capture into pieces (wireskein.split): where it cuts on lines made here, and that a capture read in
pieces reads as it does whole (tests/data/rvswd_long_truth, real captures)."""

from pathlib import Path

import numpy as np
import pytest

from wireskein import split
from wireskein._engine.model import Capture, Channel
from wireskein.decode import analyze_file, decode_file

D = Path(__file__).parent / "data" / "rvswd_long_truth"
RVSWD = {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]}


def bursts(n_bursts=40, clocks=200, per=10, gap=5000, t0=1000):
    """A clock and a data line in bursts apart by `gap` ticks."""
    clk, dat, t = [], [], t0
    rng = np.random.default_rng(1)
    for _ in range(n_bursts):
        for i in range(clocks):
            clk += [t + i * per, t + i * per + per // 2]
            if rng.random() < 0.5:
                dat.append(t + i * per + 1)
        t += clocks * per + gap
    return np.asarray(clk, np.int64), np.unique(np.asarray(dat, np.int64)), t + gap


def cap_of(**lines):
    n = max(int(e[-1]) for e in lines.values() if len(e)) + 1000
    return Capture(1e6, n, [Channel(k, 0, np.asarray(v, np.int64)) for k, v in lines.items()])


def test_options():
    assert split.options("off") is None
    o = split.options("max_edges=1000,gap_min=8,ignore=X;Y,cuts=10;20")
    assert o["max_edges"] == 1000 and o["gap_min"] == 8.0 and o["ignore"] == ["X", "Y"] and o["cuts"] == [10, 20]
    with pytest.raises(ValueError, match="unknown option"):
        split.options("size=3")


def test_short_enough_is_read_whole():
    clk, dat, _ = bursts()
    (p,) = split.plan(cap_of(C=clk, D=dat), split.options(None))
    assert (p.s, p.cut) == (0, "end")


def test_bursts_are_cut_where_every_line_is_quiet():
    clk, dat, _ = bursts()
    c = cap_of(C=clk, D=dat)
    ps = split.plan(c, split.options("max_edges=3000,min_edges=500"))
    assert len(ps) > 3 and all(p.cut in ("quiet", "end") for p in ps)
    for p in ps[:-1]:                      # each cut inside a gap between bursts: no clock within 1000 ticks
        assert np.min(np.abs(clk - p.e)) > 1000
    assert sum(p.edges for p in ps) == len(clk) + len(dat)


def test_a_line_trembling_in_short_pulses_does_not_hide_the_quiet_and_is_told():
    clk, dat, _ = bursts()
    rng = np.random.default_rng(2)
    t = np.sort(rng.choice(int(clk[-1]), 3000, replace=False))
    noise = np.sort(np.concatenate((t, t + 1)))          # pulses of one tick: a floating input
    c = cap_of(C=clk, D=dat, F=noise)
    noisy = {}
    ps = split.plan(c, split.options("max_edges=4000,min_edges=500"), noisy=noisy)
    assert all(p.cut in ("quiet", "end") for p in ps) and "F" in noisy


def test_a_line_trembling_without_pause_is_taken_as_free_running_until_it_is_ignored():
    # a line trembling in pulses of 10 to 20 ticks, never still: it tells no frame's end, as a free-running clock;
    # the cuts are then "data_quiet" (read past, in case they are inside a frame). Ignored, they are "quiet"
    clk, dat, _ = bursts()
    rng = np.random.default_rng(3)
    noise = np.cumsum(rng.integers(10, 21, int(clk[-1]) // 15)) + 500
    c = cap_of(C=clk, D=dat, F=noise)
    ps = split.plan(c, split.options("max_edges=4000,min_edges=500"))
    assert {p.cut for p in ps[:-1]} == {"data_quiet"}
    ps = split.plan(c, split.options("max_edges=4000,min_edges=500,ignore=F"))
    assert all(p.cut in ("quiet", "end") for p in ps)


def test_only_a_free_running_clock_is_cut_at_the_limit_with_an_overlap():
    clk = np.arange(100, 200_000, 5, dtype=np.int64)
    ps = split.plan(cap_of(C=clk), split.options("max_edges=10000,overlap_edges=100"))
    assert [p.cut for p in ps[:-1]] == ["forced"] * (len(ps) - 1)
    assert ps[0].b > ps[0].e and ps[1].a < ps[1].s          # read past the cut on both sides


def test_a_free_running_clock_and_data_pausing_cut_as_data_quiet():
    clk = np.arange(100, 400_000, 5, dtype=np.int64)
    dat = []
    for k in range(0, 400_000, 20_000):                      # data busy 10000 ticks, then still 10000
        dat += list(range(100 + k, 10_100 + k, 15))
    c = cap_of(C=clk, D=np.asarray(dat, np.int64))
    ps = split.plan(c, split.options("max_edges=30000,min_edges=1000,overlap_edges=100"))
    assert {p.cut for p in ps[:-1]} == {"data_quiet"}
    assert all(p.b > p.e for p in ps[:-1])


def test_given_cuts():
    clk, dat, _ = bursts()
    ps = split.plan(cap_of(C=clk, D=dat), split.options("cuts=30000;60000"))
    assert [(p.s, p.e) for p in ps[:2]] == [(0, 30000), (30000, 60000)] and ps[0].cut == "given"


@pytest.mark.parametrize("name", ["linke_l103_target_info", "linke_v203_100mhz_target_info",
                                  "linke_l103_repeat1_target_info"])
def test_a_capture_read_in_pieces_reads_as_it_does_whole(name):
    whole = decode_file(D / f"{name}.sr", RVSWD, split="off")["parts"][0]["buses"][0]["result"]
    doc = decode_file(D / f"{name}.sr", RVSWD, split="max_edges=20000,min_edges=2000,overlap_edges=500")
    part = doc["parts"][0]
    assert len(part["split"]["pieces"]) > 2
    got = part["buses"][0]["result"]
    assert got["dmi"] == whole["dmi"]
    assert got.get("dm") == whole.get("dm")


def test_analyze_in_pieces_takes_the_roles_from_the_first():
    doc = analyze_file(D / "linke_l103_target_info.sr", split="max_edges=20000,min_edges=2000,overlap_edges=500")
    part = doc["parts"][0]
    (c,) = [c for c in part["claims"] if c["protocol"] == "rvswd"]
    assert c["roles"] == {"clk": "SWCLK", "dio": "SWDIO"} and len(c["result"]["dmi"]) == 334
    assert part["split"]["roles_from"][0] == 0


def test_a_wireskein_file_is_read_from_the_file_as_the_pieces_go(tmp_path):
    # the .sr written as a WireSkein file, then decoded in pieces straight from it (split.FileSource): the same
    import argparse
    from wireskein import cli
    out = tmp_path / "l103.wireskein"
    cli.convert(argparse.Namespace(input=D / "linke_l103_target_info.sr", output=out, part=None, from_s=None,
                                   to_s=None, pins=None))
    whole = decode_file(D / "linke_l103_target_info.sr", RVSWD, split="off")["parts"][0]["buses"][0]["result"]
    opts = "max_edges=20000,min_edges=2000,overlap_edges=500"
    src = split.FileSource(out, block=1 << 16)
    pieces = [p for p, _ in split.run(src, split.options(opts))]
    assert len(pieces) > 2
    doc = decode_file(out, RVSWD, split=opts)
    got = doc["parts"][0]["buses"][0]["result"]
    assert got["dmi"] == whole["dmi"] and got.get("dm") == whole.get("dm")
    a = analyze_file(out, split=opts)["parts"][0]
    (c,) = [c for c in a["claims"] if c["protocol"] == "rvswd"]
    assert c["result"]["dmi"] == whole["dmi"]


def test_items_written_out_as_the_pieces_go(tmp_path):
    import json
    whole = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split="off")["parts"][0]["buses"][0]
    out = tmp_path / "items.ndjson"
    doc = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD,
                      split="max_edges=20000,min_edges=2000,overlap_edges=500", items=out)
    b = doc["parts"][0]["buses"][0]
    rows = [json.loads(x) for x in out.read_text().splitlines()]
    by = {}
    for r in rows:
        by.setdefault(r.pop("layer"), []).append({k: v for k, v in r.items() if k != "bus"})
    assert b["result"]["items"]["counts"]["dmi"] == len(by["dmi"])
    whole_s = {x["s"] for x in whole["result"]["dmi"]}
    assert whole_s <= {x["s"] for x in by["dmi"]}            # every frame the whole reads (pieces may read more)
    mem = {e["addr"]: e["value"] for e in by["dm"] if e["op"] == "MEMR"}
    want = {e["addr"]: e["value"] for e in whole["result"]["dm"] if e["op"] == "MEMR"}
    assert {a: mem.get(a) for a in want} == want              # the Debug Module across the pieces: the same reads


def test_a_sr_is_read_as_the_pieces_go():
    src = split.SrSource(D / "linke_l103_target_info.sr")
    assert src.names == ["SWCLK", "SWDIO"] and src.n == 9399040
    whole = decode_file(D / "linke_l103_target_info.sr", RVSWD, split="off")["parts"][0]["buses"][0]["result"]
    pieces = [p for p, _ in split.run(src, split.options("max_edges=20000,min_edges=2000,overlap_edges=500"))]
    assert len(pieces) > 2
    got = decode_file(D / "linke_l103_target_info.sr", RVSWD,
                      split="max_edges=20000,min_edges=2000,overlap_edges=500")["parts"][0]["buses"][0]
    assert got["result"]["dmi"] == whole["dmi"]


def test_unread_stretches_summed_up():
    u = split.Unread()
    u.add([{"s": 1, "e": 2, "edges": 2}, {"s": 5, "e": 9, "edges": 2}, {"s": 20, "e": 90, "edges": 7}])
    assert u.doc() == {"count": 3, "edges": 11, "sizes": {"2": 2, "5-8": 1}, "largest": {"s": 20, "e": 90, "edges": 7}}
    assert [split.size_bin(k) for k in (1, 2, 3, 4, 5, 8, 9, 16, 17)] == ["1", "2", "3-4", "3-4", "5-8", "5-8",
                                                                            "9-16", "9-16", "17-32"]


def test_workers_read_the_same():
    opts = "max_edges=20000,min_edges=2000,overlap_edges=500"
    one = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split=opts)["parts"][0]["buses"][0]["result"]
    two = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split=opts + ",workers=2")["parts"][0]["buses"][0]["result"]
    assert one == two


def test_a_piece_the_roles_read_badly_is_told():
    # the stretch before a LinkE connects (frames of 15 clocks, no RVSWD frame) fills a piece RVSWD does not read:
    # that piece is doubtful, with where it is, to analyse on its own
    doc = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split="max_edges=20000,min_edges=2000,overlap_edges=500")
    sp = doc["parts"][0]["split"]
    (d,) = sp["doubtful"]
    assert (d["s"], d["e"]) in {(p["s"], p["e"]) for p in sp["pieces"]} and "bus 0 (rvswd)" in d["why"]
    assert 0.08 < d["seconds"][0] < 0.11


def test_a_data_line_ringing_at_a_slow_enough_clock_still_pairs_with_it(tmp_path):
    # 0.1 s of a LinkE's SDI monitor (25 MHz: SWCLK's period 10 samples, SWDIO ringing 1-2 samples at its edges):
    # the ringing scattered SWDIO's changes over the period and the two never formed a bus; read as they settle,
    # analyze finds RVSWD. (An excerpt of wch-protocols' more/l103/monitor_sdi.sr, made here from a piece of it.)
    import argparse
    from wireskein import cli
    src = Path("/home/mt/dev_wch/wch-protocols/captures/fixtures/wire-linke-p4-2026-09-25/more/l103/monitor_sdi.sr")
    if not src.exists():
        pytest.skip("wch-protocols' monitor_sdi.sr is not here")
    out = tmp_path / "m.wireskein"
    cli.convert(argparse.Namespace(input=src, output=out, part=None, from_s=5.0, to_s=5.05, pins=None))
    (c,) = [c for c in analyze_file(out)["parts"][0]["claims"] if c["protocol"] == "rvswd"]
    assert c["roles"] == {"clk": "SWCLK", "dio": "SWDIO"} and c["verdict"] == "confirmed"


def test_doubtful_is_the_same_with_items_written_out(tmp_path):
    opts = "max_edges=20000,min_edges=2000,overlap_edges=500"
    a = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split=opts)["parts"][0]["split"].get("doubtful")
    b = decode_file(D / "linke_v203_read_flash_4k.sr", RVSWD, split=opts,
                    items=tmp_path / "i.ndjson")["parts"][0]["split"].get("doubtful")
    assert a == b and a


def test_a_capture_changing_on_the_way_is_told_with_what_the_doubtful_piece_holds():
    # UART on TX for the first half, then I2C on two other lines: the roles from the start (uart on TX) read nothing
    # later, those pieces are doubtful, and analysed on their own they hold I2C (lines made here, numbers known)
    from wireskein._engine import gen
    rng = np.random.default_rng(5)
    tx, scl, sda = gen.Wave(1), gen.Wave(1), gen.Wave(1)
    t = 1e-3
    for _ in range(300):
        t = gen.uart(tx, t, bytes(rng.integers(0, 256, 8).tolist()), 115200) + 200e-6
    t2 = t + 5e-3
    for k in range(400):
        t2 = gen.i2c(scl, sda, t2, [{"addr": 0x50, "rw": "write", "bytes": bytes([k & 0xFF, 1, 2])}], 400e3) + 300e-6
    c = gen.quantize({"TX": tx, "SCL": scl, "SDA": sda}, 4e6, t2 + 1e-3, rng)
    from wireskein import capture
    import tempfile
    from fractions import Fraction
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "c.wireskein"
        capture.write(p, capture.Capture(Fraction(4_000_000), [capture.Part(
            c.n_samples, [capture.logic(ch.name, ch.initial, ch.edges, c.n_samples) for ch in c.channels])]))
        doc = analyze_file(p, split="max_edges=8000,min_edges=1000,overlap_edges=200")
    part = doc["parts"][0]
    assert {c["protocol"] for c in part["claims"]} == {"uart"}               # the roles from the start
    doubtful = part["split"]["doubtful"]
    assert doubtful and all(d["seconds"][1] > t for d in doubtful)             # none in the UART half
    late = [d for d in doubtful if d["seconds"][0] > t + 1e-3]
    assert late and all(any(h["protocol"] == "i2c" and set(h["roles"].values()) == {"SCL", "SDA"}
                            for h in d["holds"]) for d in late)
