"""Wiegand on D0 / D1 (low pulses taking turns): 26-bit frames with their parities, facility and card numbers.
Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def frame26(facility, card):
    body = [(facility >> (7 - i)) & 1 for i in range(8)] + [(card >> (15 - i)) & 1 for i in range(16)]
    return [sum(body[:12]) % 2] + body + [1 - sum(body[12:]) % 2]


def test_26_bit_frames():
    rng = np.random.default_rng(8)
    d0, d1 = gen.Wave(1), gen.Wave(1)
    t, want = 100e-6, []
    for _ in range(5):
        fac, card = int(rng.integers(0, 256)), int(rng.integers(0, 65536))
        want.append((fac, card))
        for b in frame26(fac, card):
            w = d1 if b else d0
            w.set(t, 0)
            w.set(t + 50e-6, 1)
            t += 1e-3
        t += 50e-3
    cap = gen.quantize({"D0": d0, "D1": d1}, 1e6, t, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "wiegand" and c["roles"] == {"d0": "D0", "d1": "D1"}
    assert [(f["facility"], f["card"], f["ok"]) for f in c["result"]["frames"]] == [(a, b, True) for a, b in want]
