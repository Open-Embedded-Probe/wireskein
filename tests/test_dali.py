"""DALI: forward frames (address byte, command) and backward frames (answers), Manchester at 1200 b/s. Waveforms
made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

HALF = 1 / 2400


def frame(w, t, value, nbits):
    bits = [1] + [(value >> (nbits - 1 - i)) & 1 for i in range(nbits)]
    for b in bits:
        w.set(t, 0 if b else 1)
        w.set(t + HALF, 1 if b else 0)
        t += 2 * HALF
    w.set(t, 1)
    return t + 6 * HALF


def test_queries_and_answers():
    rng = np.random.default_rng(13)
    w = gen.Wave(1)
    t, want = 1e-3, []
    for _ in range(5):
        a = int(rng.integers(0, 64)) << 1 | 1
        cmd, ans = int(rng.integers(144, 200)), int(rng.integers(0, 256))
        t = frame(w, t, a << 8 | cmd, 16) + 3e-3
        t = frame(w, t, ans, 8) + 20e-3
        want += [("forward", a, cmd), ("backward", None, ans)]
    cap = gen.quantize({"DALI": w}, 100e3, t, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "dali"
    assert [(f["kind"], f.get("addr_byte"), f["data"]) for f in c["result"]["frames"]] == want
