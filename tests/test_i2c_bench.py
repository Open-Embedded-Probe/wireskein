"""I2C runs from the bench whose lines bounce (tests/data/i2c_bench/README.md): read as an I2C input sees them, the
transactions are those the test asked for; the one that cannot be read stays failing, said so."""

from pathlib import Path

import pytest

from wireskein.verify import verify

D = Path(__file__).parent / "data" / "i2c_bench"


@pytest.mark.parametrize("run", ["x035_100k", "x035_12k5", "v205_contract", "h417_a"])
def test_bouncing_lines_read_as_the_device_read_them(run):
    r = verify(D / run)
    bad = [(x["path"], x["reason"]) for x in r["results"] if x["status"] != "ok"]
    assert not bad
    assert any(n["kind"] == "bus_glitch" or n["kind"] == "slow_edge" for x in r["results"] for n in x["notices"])


def test_a_half_of_the_clock_captured_at_the_other_level_does_not_read():
    r = verify(D / "h417_b")
    bad = [x for x in r["results"] if x["status"] != "ok"]
    assert [(x["path"], x["check"]) for x in bad] == [("wire output only PB11 PB10/address 0x13, no target", "i2c")]
    assert "0 decoded" in bad[0]["reason"]


def test_the_automatic_decode_reads_them_too():
    from wireskein.decode import decode_file
    spec = {"buses": [{"protocol": "i2c", "pins": {"scl": "PA10", "sda": "PA11"}}]}
    for run in ("x035_100k", "x035_12k5"):
        b = decode_file(D / run / "c0001.wireskein", spec)["parts"][0]["buses"][0]
        got = [(t["addr"], t["rw"], t["addr_ack"]) for t in b["result"]["transactions"]]
        assert got == [(0x42, "write", False), (0x13, "write", False)], run
