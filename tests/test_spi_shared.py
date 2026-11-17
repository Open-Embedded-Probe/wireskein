"""One SPI bus with a target on each CS (CLK, MOSI, MISO shared), decoded with each target's roles given: every target
reads its own transfers, the other targets' time is theirs (`shared`), not unread, and no CS is told as stopped while
the clock goes on for another target. The select line's active level is the one whose windows hold the transfers one
by one, tightly, not the one holding more of the clock (the other targets' transfers fall in a CS's idle level)."""

import numpy as np

from wireskein import decode
from wireskein._engine import gen


def _bus(order="msb", n_cs=2, seed=1):
    rng = np.random.default_rng(seed)
    w = {"CLK": gen.Wave(0), "MOSI": gen.Wave(0), "MISO": gen.Wave(0)}
    css = [w.setdefault(f"CS{k}", gen.Wave(1)) for k in range(n_cs)]
    sent = [[] for _ in range(n_cs)]
    t = 1e-4
    # the last target gets most of the transfers: its traffic falls in the others' idle level
    for k in [0, 1, 1, 1, 1, 0, 1, 1, *range(n_cs)]:
        fr = (bytes(rng.integers(0, 256, 3, dtype=np.uint8)), bytes(rng.integers(0, 256, 3, dtype=np.uint8)))
        sent[k].append(fr)
        t = gen.spi(w["CLK"], w["MOSI"], w["MISO"], css[k], t, [fr], 1e6, 0, order, gap=20e-6)
    cap = gen.quantize(w, 20e6, t + 1e-4, rng)
    spec = {"buses": [{"protocol": "spi", "pins": {"clk": "CLK", "mosi": "MOSI", "miso": "MISO", "cs": f"CS{k}"},
                       "params": {"mode": 0, "bit_order": order}} for k in range(n_cs)]}
    return cap, spec, sent


def _read(cap, spec):
    return decode._part(cap, decode._spec(spec), {})["buses"]


def test_each_target_reads_its_own():
    cap, spec, sent = _bus(n_cs=3)
    for k, b in enumerate(_read(cap, spec)):
        assert [x["MOSI"] for x in b["result"]["transfers"]] == [f[0].hex() for f in sent[k]]
        assert [x["MISO"] for x in b["result"]["transfers"]] == [f[1].hex() for f in sent[k]]


def test_the_other_targets_time_is_shared_not_unread():
    cap, spec, sent = _bus()
    for k, b in enumerate(_read(cap, spec)):
        assert b["uninterpreted"] == []
        assert b["reading"]["unread"] == 0 and b["reading"]["shared"] > 0
        assert {x["bus"] for x in b["shared"]} == {1 - k}
        assert "stopped" not in b["reading"]


def test_a_target_left_out_of_the_spec_is_unread():
    cap, spec, sent = _bus()
    b = _read(cap, {"buses": spec["buses"][:1]})[0]
    assert b["reading"]["unread"] > 0 and "shared" not in b


def test_lsb_first_as_the_spec_says():
    cap, spec, sent = _bus(order="lsb")
    for k, b in enumerate(_read(cap, spec)):
        assert [x["MOSI"] for x in b["result"]["transfers"]] == [f[0].hex() for f in sent[k]]
        assert b["params"]["bit_order"] == "lsb"


def test_analyze_tells_the_targets_of_one_bus():
    # two targets on their own CS: one claim each, on the shared lines and their own CS, told together by `bus`
    cap, spec, sent = _bus(n_cs=2)
    res = decode.analyze(cap)
    out = decode.export(res, cap)["claims"]
    spi = [c for c in out if c["protocol"] == "spi" and c["verdict"] in ("confirmed", "likely")]
    assert sorted(c["roles"].get("cs") for c in spi) == ["CS0", "CS1"]
    assert len({c["bus"] for c in spi}) == 1


def test_a_line_that_never_moves_is_no_cs():
    rng = np.random.default_rng(3)
    w = {"CLK": gen.Wave(0), "MOSI": gen.Wave(0), "TIED": gen.Wave(0)}
    t = 1e-4
    for _ in range(8):
        t = gen.spi(w["CLK"], w["MOSI"], None, None, t, [(bytes(rng.integers(0, 256, 2, dtype=np.uint8)), b"")], 1e6,
                    0, "msb", gap=30e-6)
    cap = gen.quantize(w, 20e6, t + 1e-4, rng)
    out = decode.export(decode.analyze(cap), cap)["claims"]
    spi = [c for c in out if c["protocol"] == "spi" and c["verdict"] == "confirmed"]
    assert spi and all("cs" not in c["roles"] for c in spi)
