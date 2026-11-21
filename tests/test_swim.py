"""SWIM (STM8's single wire debug): frames of pulse-width bits (short low 1, long low 0) with even parity and the
receiver's acknowledge; a WOTF writing a byte and a ROTF reading bytes. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

CLK = 1 / 8e6                 # the SWIM clock (HSI 8 MHz), low speed: 22 clocks a bit


def put(w, t, bits):
    for b in bits:
        w.set(t, 0)
        w.set(t + (2 if b else 20) * CLK, 1)
        t += 22 * CLK
    return t


def frame(w, t, start, payload):
    bits = [start] + payload + [sum(payload) & 1, 1]          # parity even, acknowledged
    return put(w, t, bits) + 30 * CLK


def byte(v):
    return [(v >> (7 - i)) & 1 for i in range(8)]


def test_rotf_and_wotf():
    rng = np.random.default_rng(9)
    w = gen.Wave(1)
    t = 20e-6
    # WOTF 1 byte to 0x7F80
    t = frame(w, t, 0, [0, 1, 0])
    for v in (1, 0x00, 0x7F, 0x80, 0xA1):
        t = frame(w, t, 0, byte(v))
    t += 50e-6
    # ROTF 6 bytes from 0x004800, the target's answers
    data = bytes(rng.integers(0, 256, 6).tolist())
    t = frame(w, t, 0, [0, 0, 1])
    for v in (6, 0x00, 0x48, 0x00):
        t = frame(w, t, 0, byte(v))
    for v in data:
        t = frame(w, t, 1, byte(v))
    cap = gen.quantize({"SWIM": w}, 10e6, t + 50e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "swim"
    tr = [(x["cmd"], x.get("count"), x.get("addr"), x.get("data"), x["ok"]) for x in c["result"]["transactions"]]
    assert tr == [("WOTF", 1, 0x7F80, "a1", True), ("ROTF", 6, 0x4800, data.hex(), True)]
