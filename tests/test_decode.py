"""`ws decode`: the declared buses decoded with exactly their roles, however weak the evidence, every part on its
own, with what was not read (uninterpreted, undeclared) and what was not seen (lost) beside them."""

import json
import subprocess
import sys

import numpy as np
import pytest

from wireskein import fileformat
from wireskein.decode import FORMAT, decode_file
from wireskein._engine import fileio, gen

SPEC = {"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}}], "lines": ["DRDY"]}


def _part(tx, junk=False, drdy=None, other=None):
    scl, sda, d, x = gen.Wave(1), gen.Wave(1), gen.Wave(0), gen.Wave(0)
    gen.i2c(scl, sda, 50e-6, tx, 100e3)
    if junk:                                       # SCL clocked with SDA high: no START, nothing to read
        for k in range(8):
            scl.set(600e-6 + k * 10e-6, 0)
            scl.set(605e-6 + k * 10e-6, 1)
    if drdy:
        d.set(drdy, 1)
        d.set(drdy + 20e-6, 0)
    if other:
        x.set(other, 1)
        x.set(other + 5e-6, 0)
    cap = gen.quantize({"SCL": scl, "SDA": sda, "DRDY": d, "X": x}, 1e6, 1e-3, np.random.default_rng(0))
    return [fileio.to_channel(c, cap.n_samples) for c in cap.channels]


def _file(path, lost=None):
    parts = [fileformat.Part(_part([{"addr": 0x44, "rw": "write", "bytes": [0x24, 0x00]}], junk=True, drdy=900e-6,
                                   other=950e-6), {"start_ns": 5_000_000}, 0),
             fileformat.Part(_part([{"addr": 0x44, "rw": "read", "bytes": [0x66, 0x21]}]), {"start_ns": 9_000_000},
                             3000)]
    return fileformat.write(path, 1_000_000, parts=parts, layout="timeline", lost=lost)


def test_declared_buses_every_part_and_what_was_not_read(tmp_path):
    lost = [{"after_part": 0, "reason": "probe dropped a segment (no room)", "ticks": 2000}]
    doc = decode_file(_file(tmp_path / "c.wireskein", lost), SPEC)
    assert doc["format"] == FORMAT and doc["spec"]["buses"][0]["pins"] == {"scl": "SCL", "sda": "SDA"}
    c = doc["capture"]
    assert c["complete"] is False and c["lost"][0]["ticks"] == 2000 and c["parts"] == 2 and len(c["sha256"]) == 64
    p0, p1 = doc["parts"]
    assert (p0["tick0"], p0["start_ns"], p1["tick0"], p1["start_ns"]) == (0, 5_000_000, 3000, 9_000_000)
    b0, b1 = p0["buses"][0], p1["buses"][0]
    assert b0["decoded"] and b0["fit"]["claimed"] is False             # one transfer: too little for analyze
    t = b0["result"]["transactions"]
    assert [(x["addr"], x["rw"], x["bytes"], x["start_cond"], x["end_cond"]) for x in t] == [(0x44, "write", "2400", "S", "P")]
    assert b1["result"]["transactions"][0]["bytes"] == "6621"
    # the clocks without a START: activity on the bus's pins that no item covers
    assert [(u["s"], u["edges"]) for u in b0["uninterpreted"]] == [(600, 16)] and b1["uninterpreted"] == []
    assert p0["lines"] == [{"pin": "DRDY", "initial": 0, "edges": [900, 920]}]
    assert p0["undeclared"] == [{"pin": "X", "edges": 2, "s": 950, "e": 955, "bursts": 1}] and p1["undeclared"] == []


def test_a_bus_that_is_not_there(tmp_path):
    p = _file(tmp_path / "c.wireskein")
    with pytest.raises(ValueError, match="pins not in the capture: CS"):
        decode_file(p, {"buses": [{"protocol": "spi", "pins": {"clk": "SCL", "mosi": "SDA", "cs": "CS"}}]})
    # pins there but no such bus on them: said, not left out
    doc = decode_file(p, {"buses": [{"protocol": "spi", "pins": {"clk": "DRDY", "mosi": "X"}}]})
    b = doc["parts"][1]["buses"][0]
    assert b["decoded"] is False and b["result"] == {}


def test_cli(tmp_path):
    p = _file(tmp_path / "c.wireskein")
    (tmp_path / "spec.json").write_text(json.dumps(SPEC))
    out = tmp_path / "analysis.json"
    subprocess.run([sys.executable, "-m", "wireskein", "decode", str(p), "--spec", str(tmp_path / "spec.json"),
                    "--out", str(out)], check=True)
    doc = json.loads(out.read_text())
    assert doc["format"] == FORMAT and doc["capture"]["complete"] is True and "lost" not in doc["capture"]
