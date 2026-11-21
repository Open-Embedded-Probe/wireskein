"""S/PDIF: biphase mark subframes with their B / M / W preambles, 24-bit samples, V U C and the even parity.
The encoder written here."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

PRE = {"B": [1, 1, 1, 0, 1, 0, 0, 0], "M": [1, 1, 1, 0, 0, 0, 1, 0], "W": [1, 1, 1, 0, 0, 1, 0, 0]}


def subframe_halves(pre, value, last):
    """The subframe as half cells' levels, the preamble's pattern inverted after a cell ending low."""
    p = PRE[pre] if last == 0 else [1 - x for x in PRE[pre]]
    halves = list(p)
    bits = [(value >> k) & 1 for k in range(24)] + [0, 0, 0]
    bits.append(sum(bits) & 1)
    lvl = halves[-1]
    for b in bits:
        lvl ^= 1
        halves.append(lvl)
        if b:
            lvl ^= 1
        halves.append(lvl)
    return halves


@pytest.mark.parametrize("rate", [24e6, 16e6])
def test_samples(rate):
    rng = np.random.default_rng(int(rate))
    w = gen.Wave(0)
    t, half = 20e-6, 1 / (48000 * 128)
    want = []
    lvl = 0
    for f in range(40):
        for ch in ("A", "B"):
            pre = ("B" if f % 192 == 0 else "M") if ch == "A" else "W"
            v = int(rng.integers(0, 1 << 24))
            want.append((pre, v))
            for x in subframe_halves(pre, v, lvl):
                w.set(t, x)
                t += half
            lvl = x
    cap = gen.quantize({"SPDIF": w}, rate, t + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "spdif"
    got = [(x["preamble"], x["audio"]) for x in c["result"]["subframes"] if x["ok"]]
    # all of them in order but maybe the last (no edge closes it)
    k = want.index(got[0])
    assert got == want[k:k + len(got)] and len(got) >= len(want) - 1
