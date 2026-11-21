"""A select line toggling with the clock for a while (lines floating as a board powers down: sigrok-dumps' AT25SF041
at its end) frames a "transfer" of one bit at each clock: no transfer. Those windows are left out of the select's
frames, so the bus is read with its select. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def test_one_bit_windows_are_no_frames():
    rng = np.random.default_rng(5)
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    frames = [(bytes(rng.integers(0, 256, 5).tolist()), bytes(rng.integers(0, 256, 5).tolist())) for _ in range(10)]
    t = gen.spi(clk, mosi, miso, cs, 20e-6, frames, 2e6, gap=50e-6)
    # at the end the select falls and rises around each of 16 slow clocks
    for _ in range(16):
        cs.set(t, 0)
        clk.set(t + 0.2e-6, 1)
        clk.set(t + 0.6e-6, 0)
        cs.set(t + 0.7e-6, 1)
        t += 1.3e-6
    cap = gen.quantize({"CLK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 50e6, t + 20e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "spi"]
    assert c["verdict"] == "confirmed" and c["roles"].get("cs") == "CS"
    assert [(x["MOSI"], x["MISO"]) for x in c["result"]["transfers"]] == [(a.hex(), b.hex()) for a, b in frames]
