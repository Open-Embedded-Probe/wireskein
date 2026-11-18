"""A select line's pulses of a single sample (crosstalk) cut none of its selections, where the bus clock is too fast for
a quarter of its period to reach two samples (8 MHz at 50 MS/s: about 6 a period) and the gate merges nothing: the
bus is read with its CS. Lines made here."""

import numpy as np

from wireskein import decode
from wireskein._engine.model import Capture, Channel

HALF = 3


def lines(transfers, spikes=True):
    rng = np.random.default_rng(7)
    cs, clk, mosi = [], [], []
    t, lv = 200, 0
    for data in transfers:
        cs.append(t - 4)
        bits = [(b >> (7 - i)) & 1 for b in data for i in range(8)]
        start = t
        for bit in bits:
            if bit != lv:
                mosi.append(t - 1)
                lv = bit
            clk += [t, t + HALF]
            t += 2 * HALF
        end = t
        if spikes:                                       # a 1-sample pulse inside the selection, now and then
            s = int(rng.integers(start + 2, end - 2))
            cs += [end + 4] if s % 3 else [s, s + 1, end + 4]
        else:
            cs.append(end + 4)
        cs.sort()
        t += 60 + int(rng.integers(0, 40))
    return Capture(50e6, t + 200, [Channel("CS", 1, np.asarray(cs)), Channel("SCK", 0, np.asarray(clk)),
                                   Channel("MOSI", 0, np.asarray(mosi))])


def test_spiked_select_still_frames_the_transfers():
    rng = np.random.default_rng(1)
    transfers = [bytes(rng.integers(0, 256, int(rng.integers(2, 7))).tolist()) for _ in range(14)]
    cap = lines(transfers)
    assert np.diff(cap.channel("CS").edges).min() == 1          # the spikes are there
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    spi = [c for c in claims if c["protocol"] == "spi"]
    assert spi and spi[0]["roles"].get("cs") == "CS", [(c["protocol"], c["roles"], c["verdict"]) for c in claims]
