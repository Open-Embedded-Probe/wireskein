"""SENT: messages of a sync (56 ticks), a status nibble, data nibbles and CRC4 (2010 recommendation), then a pause.
The CRC computed here bit by bit; waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

TICK = 3e-6


def crc4_2010(nibbles):
    """Seed 5; for each nibble the register runs four steps (x^4 + x^3 + x^2 + 1), then takes the nibble in by XOR;
    a zero nibble after the data (the 2010 recommendation)."""
    def steps(reg):
        for _ in range(4):
            top = (reg >> 3) & 1
            reg = (reg << 1) & 0xF
            if top:
                reg ^= 0b1101
        return reg
    reg = 5
    for x in nibbles:
        reg = steps(reg) ^ x
    return steps(reg)


def test_messages():
    rng = np.random.default_rng(15)
    w = gen.Wave(1)
    t, want = 50e-6, []
    for _ in range(8):
        data = [int(x) for x in rng.integers(0, 16, 6)]
        ticks = [56, 12] + [12 + x for x in data] + [12 + crc4_2010(data), 80]
        for k in ticks:
            w.set(t, 0)
            w.set(t + 5 * TICK, 1)
            t += k * TICK
        want.append(data)
    w.set(t, 0)
    w.set(t + 5 * TICK, 1)
    cap = gen.quantize({"SENT": w}, 10e6, t + 100e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "sent" and c["params"]["data_nibbles"] == 6
    assert [m["data"] for m in c["result"]["messages"] if m["ok"]] == want
