"""Two clocked buses at once on separate lines: a PS/2 keyboard (bursts of 11 clocks at 15.8 kHz) and an SWD probe at
10 MHz whose few packets fall inside one of the PS/2 clock's periods. SWD's edges then sit at about one phase of the
slow clock (all of them within 63 us) and looked like its data: the slow clock took the four lines as an unknown
bus and neither was read (2026-11-21, synth-protomix-12). Each is its own."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def test_ps2_and_a_short_swd_burst_read_apart():
    rng = np.random.default_rng(12)
    pc, pd = gen.Wave(1), gen.Wave(1)
    codes = []
    for k in rng.integers(1, 0x84, 3).tolist():
        codes += [k, 0xF0, k]
    end1 = gen.ps2(pc, pd, 234e-6, bytes(codes), freq=15.84e3,
                   gaps=[float(x) for x in rng.uniform(0.9e-3, 4e-3, len(codes))])
    sc, sd = gen.Wave(0), gen.Wave(0)
    packets = [{"apndp": int(rng.integers(0, 2)), "rnw": 1, "a": int(rng.integers(0, 4)), "ack": 1,
                "data": int(rng.integers(0, 2 ** 32))} for _ in range(4)]
    end2 = gen.swd(sc, sd, 469e-6, packets, 10e6, idle_clocks=8, gap=8.8e-6)
    cap = gen.quantize({"KCLK": pc, "KDAT": pd, "SWCLK": sc, "SWDIO": sd}, 200e6, max(end1, end2) + 1e-3, rng)
    ex = decode.export(decode.analyze(cap), cap)
    got = {c["protocol"]: (c["roles"], c["verdict"]) for c in ex["claims"]}
    assert got.get("ps2", [None])[0] == {"clk": "KCLK", "data": "KDAT"} and got["ps2"][1] in ("confirmed", "likely")
    # (four SWD packets are little evidence: the reading is SWD's, its verdict not yet sure)
    assert got.get("swd", [None])[0] == {"swclk": "SWCLK", "swdio": "SWDIO"}


def test_a_faster_bus_inside_one_period_is_no_data_of_the_slow_clock():
    """The survey's relation itself: SWD's lines change tens of times inside one period of the PS/2 clock, no data of
    it (a data line changes about once a period)."""
    from wireskein._engine import survey
    rng = np.random.default_rng(12)
    pc, pd = gen.Wave(1), gen.Wave(1)
    end1 = gen.ps2(pc, pd, 234e-6, bytes([0x1C, 0xF0, 0x1C, 0x32, 0xF0, 0x32]), freq=15.84e3, gaps=[1.3e-3] * 6)
    sc, sd = gen.Wave(0), gen.Wave(0)
    packets = [{"apndp": 0, "rnw": 1, "a": k, "ack": 1, "data": int(rng.integers(0, 2 ** 32))} for k in range(4)]
    gen.swd(sc, sd, 469e-6, packets, 10e6, idle_clocks=8, gap=8.8e-6)
    cap = gen.quantize({"KCLK": pc, "KDAT": pd, "SWCLK": sc, "SWDIO": sd}, 200e6, end1 + 1e-3, rng)
    sv = survey.survey(cap)
    assert sv.pairs[("KCLK", "KDAT")].data_score > 0.5
    assert sv.pairs[("KCLK", "SWCLK")].data_score < 0.2 and sv.pairs[("KCLK", "SWDIO")].data_score < 0.2
