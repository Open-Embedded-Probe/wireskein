"""An SWD target drives its ACK and read data on the rising edge of SWCLK that clocks them out, and the host reads
each bit on the next rising edge; captured at a few samples a bit, the target's change falls on the rising edge's
own sample. The value latched there is the one before (an STLink's SWD: 47 of 183 requests read, its ACKs and
parities off by a bit). Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

RATE = 8e6


def test_target_bits_changing_on_the_rising_edge_are_read_before_it():
    rng = np.random.default_rng(5)
    packets = [{"apndp": int(rng.integers(0, 2)), "rnw": int(rng.integers(0, 2)), "a": int(rng.integers(0, 4)),
                "ack": 1, "data": int(rng.integers(0, 2 ** 32))} for _ in range(40)]
    clk, dio = gen.Wave(0), gen.Wave(1)
    end = gen.swd(clk, dio, 10e-6, packets, 1e6, target_on_rise=True)
    cap = gen.quantize({"SWCLK": clk, "SWDIO": dio}, RATE, end + 20e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "swd"]
    got = [(p["rnw"], p["ack"], p.get("data")) for p in c["result"]["packets"]]
    assert got == [("R" if p["rnw"] else "W", "OK", p["data"]) for p in packets]
