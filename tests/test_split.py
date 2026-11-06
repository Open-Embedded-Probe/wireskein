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
