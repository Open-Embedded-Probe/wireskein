"""PS/2 (declared in decl/ps2.toml): a keyboard's scan codes, device to host, 11 clocks a byte with odd parity, the
device's clock at 12.5 kHz in bursts. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def frame(clk, dat, t, byte, f=12.5e3):
    """One byte device to host: data set while the clock is high, read on its fall; the line idles high."""
    bits = [0] + [(byte >> i) & 1 for i in range(8)] + [1 - (bin(byte).count("1") & 1), 1]
    h = 0.5 / f
    for b in bits:
        dat.set(t, b)
        clk.set(t + 0.5 * h, 0)
        clk.set(t + 1.5 * h, 1)
        t += 2 * h
    dat.set(t, 1)
    return t


def test_scan_codes():
    rng = np.random.default_rng(4)
    codes = []
    for k in rng.integers(1, 0x84, 12).tolist():
        codes += [k, 0xF0, k]                       # make, break
    clk, dat = gen.Wave(1), gen.Wave(1)
    t = 100e-6
    for x in codes:
        t = frame(clk, dat, t, x) + float(rng.uniform(0.5e-3, 3e-3))
    cap = gen.quantize({"CLK": clk, "DATA": dat}, 1e6, t + 1e-3, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "ps2" and c["roles"] == {"clk": "CLK", "data": "DATA"}
    assert [f["data"] for f in c["result"]["frames"]] == codes


def test_a_few_scan_codes_beat_the_unknown_reading():
    """Nine bytes (three keys): the same lines read as an unknown clocked bus of 11-bit frames scored a little above
    PS/2, whose start, parity and stop hold in every frame (2026-11-21, synth-protocols-277)."""
    rng = np.random.default_rng(9)
    codes = []
    for k in rng.integers(1, 0x84, 3).tolist():
        codes += [k, 0xF0, k]
    clk, dat = gen.Wave(1), gen.Wave(1)
    f = float(rng.uniform(10e3, 16.7e3))
    t = gen.ps2(clk, dat, 100e-6, bytes(codes), freq=f, gaps=[float(x) for x in rng.uniform(0.3e-3, 5e-3, 9)])
    cap = gen.quantize({"CLK": clk, "DATA": dat}, 1e6, t + 1e-3, rng, jitter_s=0.1e-6)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = ex["claims"]
    assert c["protocol"] == "ps2" and [f["data"] for f in c["result"]["frames"]] == codes
