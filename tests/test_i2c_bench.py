"""I2C runs from the bench whose lines bounce (tests/data/i2c_bench/README.md): read as an I2C input sees them, the
transactions are those the test asked for; the one that cannot be read stays failing, said so."""

from pathlib import Path

import pytest

from wireskein.verify import verify

D = Path(__file__).parent / "data" / "i2c_bench"


@pytest.mark.parametrize("run", ["x035_100k", "x035_12k5", "v205_contract", "h417_a", "h417_b"])
def test_bouncing_lines_read_as_the_device_read_them(run):
    r = verify(D / run)
    bad = [(x["path"], x["reason"]) for x in r["results"] if x["status"] != "ok"]
    assert not bad
    assert any(n["kind"] == "bus_glitch" or n["kind"] == "slow_edge" for x in r["results"] for n in x["notices"])


def test_a_clock_clipped_to_a_moment_and_one_not_captured_at_all_read_right():
    # h417_b's "address 0x13": SCL high for 30 ns where a clock was (a controller counting its high from releasing
    # SCL through a slow rise), and in that transaction one clock not in the samples at all, restored
    r = verify(D / "h417_b")
    assert [x for x in r["results"] if x["status"] != "ok"] == []
    seg = next(x for x in r["results"] if x["path"].endswith("0x13, no target") and x["check"] == "i2c")
    assert [n["kind"] for n in seg["notices"]].count("clock_restored") == 1


def test_the_automatic_decode_reads_them_too():
    from wireskein.decode import decode_file
    spec = {"buses": [{"protocol": "i2c", "pins": {"scl": "PA10", "sda": "PA11"}}]}
    for run in ("x035_100k", "x035_12k5"):
        b = decode_file(D / run / "c0001.wireskein", spec)["parts"][0]["buses"][0]
        got = [(t["addr"], t["rw"], t["addr_ack"]) for t in b["result"]["transactions"]]
        assert got == [(0x42, "write", False), (0x13, "write", False)], run


SLOW = D / "h417_slow"
SPEC = {"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}}]}


def _bus(name):
    from wireskein.decode import decode_file
    f = SLOW / "stream" / f"{name}.wireskein"
    if not f.exists():
        pytest.skip("tests/data/i2c_bench/h417_slow is in git, not in the sdist")
    return decode_file(f, SPEC)["parts"][0]["buses"][0]


def test_a_line_a_glitch_filter_locked_is_told_stopped():
    # 100 MS/s, the P4's flex filter at 100 ns: SCL changes no more from about 1.45 s, SDA goes on
    r = _bus("nohys-100000000-g100")["reading"]
    stopped = r["stopped"]
    assert [s["line"] for s in stopped] == ["SCL"] and stopped[0]["others_after"] >= 100
    assert r["glitch_filter_ns"] == 100 and r["line_changes"]["SCL"]["last"] == stopped[0]["tick"]


def test_a_line_a_glitch_filter_set_swinging_is_told_oscillating():
    r = _bus("fexp2-50000000-g75")["reading"]
    osc = [o for o in r["oscillating"] if o["line"] == "SCL"]
    assert osc and sum(o["changes"] for o in osc) > 500_000


def test_without_a_filter_neither_is_told():
    r = _bus("nohys-50000000-g0")["reading"]
    assert "stopped" not in r and "oscillating" not in r and "glitch_filter_ns" not in r
