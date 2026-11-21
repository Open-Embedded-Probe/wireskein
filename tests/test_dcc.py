"""DCC on the track signal: preambles, bytes led by 0s, the error detection byte. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def put(w, t, bits):
    for b in bits:
        h = 58e-6 if b else 100e-6
        w.set(t, 1)
        w.set(t + h, 0)
        t += 2 * h
    return t


def packet(data):
    x = 0
    for v in data:
        x ^= v
    bits = [1] * 14
    for v in list(data) + [x]:
        bits += [0] + [(v >> (7 - i)) & 1 for i in range(8)]
    return bits + [1]


def test_packets():
    rng = np.random.default_rng(14)
    w = gen.Wave(0)
    t, want = 100e-6, []
    for _ in range(8):
        data = [int(rng.integers(1, 128)), int(rng.integers(0x60, 0x80))]
        t = put(w, t, packet(data))
        want.append((data[0], bytes(data + [data[0] ^ data[1]]).hex()))
    t = put(w, t, [1] * 4)                                  # the signal goes on (the next preamble)
    cap = gen.quantize({"TRACK": w}, 1e6, t + 1e-3, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "dcc"
    assert [(p["address"], p["bytes"]) for p in c["result"]["packets"] if p["ok"]] == want
