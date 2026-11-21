"""SAE J1850 VPW: SOF, alternating passive / active symbols of 64 / 128 us, bytes MSB first, CRC8; and a comparator's
offset (the active level 30 us longer, the passive shorter). The CRC computed here bit by bit; waveforms made here."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen


def crc8(data):
    reg = 0xFF
    for x in data:
        for k in range(7, -1, -1):
            top = ((reg >> 7) & 1) ^ ((x >> k) & 1)
            reg = (reg << 1) & 0xFF
            if top:
                reg ^= 0x1D
    return reg ^ 0xFF


@pytest.mark.parametrize("offset", [0.0, 30e-6])
def test_frames(offset):
    rng = np.random.default_rng(16)
    w = gen.Wave(0)
    t, want = 100e-6, []
    for _ in range(6):
        body = bytes(rng.integers(0, 256, int(rng.integers(3, 8))).tolist())
        data = body + bytes([crc8(body)])
        want.append(data.hex())
        w.set(t, 1)
        t += 200e-6 + offset
        active = False
        for x in data:
            for k in range(7, -1, -1):
                b = (x >> k) & 1
                long_ = (b == 1) if not active else (b == 0)
                w.set(t, int(active))
                t += (128e-6 if long_ else 64e-6) + (offset if active else -offset)
                active = not active
        w.set(t, 0)
        t += 2e-3
    cap = gen.quantize({"VPW": w}, 1e6, t, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "j1850"
    assert [f["bytes"] for f in c["result"]["frames"] if f["ok"]] == want
