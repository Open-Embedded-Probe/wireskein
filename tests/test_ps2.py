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
