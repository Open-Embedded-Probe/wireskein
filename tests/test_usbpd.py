"""USB Power Delivery on CC: BMC, the preamble, 4b5b symbols, SOP, the header and data objects, CRC32, EOP; and a
threshold off the middle (highs shorter than lows). The encoder written here (zlib's CRC32 is the standard one)."""
import zlib

import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

ENC = [0b11110, 0b01001, 0b10100, 0b10101, 0b01010, 0b01011, 0b01110, 0b01111, 0b10010, 0b10011, 0b10110, 0b10111,
       0b11010, 0b11011, 0b11100, 0b11101]
S1, S2, EOP = 0b11000, 0b10001, 0b01101


def sym_bits(v):
    return [(v >> i) & 1 for i in range(5)]


def packet_bits(header, objects):
    body = header.to_bytes(2, "little") + b"".join(o.to_bytes(4, "little") for o in objects)
    body += zlib.crc32(body).to_bytes(4, "little")
    bits = [i & 1 for i in range(64)]
    for k in (S1, S1, S1, S2):
        bits += sym_bits(k)
    for x in body:
        bits += sym_bits(ENC[x & 0xF]) + sym_bits(ENC[x >> 4])
    return bits + sym_bits(EOP)


def bmc(w, t, bits, ui, skew):
    lvl = w.level
    for b in bits:
        lvl ^= 1
        w.set(t + (skew if lvl == 0 else 0), lvl)
        if b:
            lvl ^= 1
            w.set(t + ui / 2 + (skew if lvl == 0 else 0), lvl)
        t += ui
    lvl ^= 1
    w.set(t + (skew if lvl == 0 else 0), lvl)
    if lvl:
        w.set(t + ui, 0)
    return t + 2 * ui


@pytest.mark.parametrize("skew", [0.0, -0.4e-6])
def test_messages(skew):
    rng = np.random.default_rng(10)
    msgs = []
    for k in range(6):
        nobj = int(rng.integers(0, 4))
        hdr = (nobj << 12) | ((k & 7) << 9) | 0x40 | (1 if nobj == 0 else 2)
        msgs.append((hdr, [int(x) for x in rng.integers(0, 2 ** 32, nobj, dtype=np.uint64)]))
    w = gen.Wave(0)
    t, ui = 50e-6, 1 / 300e3
    for hdr, objs in msgs:
        t = bmc(w, t, packet_bits(hdr, objs), ui, skew) + 300e-6
    cap = gen.quantize({"CC1": w}, 4e6, t, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "usb_pd"
    assert [(p["header"], p["objects"], p["ok"]) for p in c["result"]["packets"]] == [(h, o, True) for h, o in msgs]
