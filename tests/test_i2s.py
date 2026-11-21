"""I2S and its justified kin (wireskein._engine.i2s): WS is one of the bit clock's data lines to the survey, tried as
WS beside SD; the word's place in its slot told by the padding. Waveforms made here (gen.i2s)."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen


@pytest.mark.parametrize("fmt,bits", [("i2s", 16), ("left", 16), ("right", 16), ("i2s", 32), ("i2s", 24)])
def test_frames_read_in_their_format(fmt, bits):
    rng = np.random.default_rng(0)
    b, w, s = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    lim = 1 << (bits - 1)
    frames = [(int(x), int(y)) for x, y in rng.integers(-lim, lim, (120, 2))]
    end = gen.i2s(b, w, s, 10e-6, frames, 48000, slot=32, bits=bits, fmt=fmt)
    cap = gen.quantize({"BCLK": b, "WS": w, "SD": s}, 25e6, end + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "i2s" and c["verdict"] == "confirmed"
    assert c["roles"] == {"sck": "BCLK", "ws": "WS", "sd": "SD"}
    assert c["params"]["format"] == fmt and c["params"]["word_bits"] == bits
    assert c["params"].get("format_assumed", False) == (bits == 32)        # a word filling its slot tells no format
    # the slot before WS first changes is no whole one: from the second frame
    assert [(f["left"], f.get("right")) for f in c["result"]["frames"]] == frames[1:]


def test_a_64_bit_slot_does_not_overflow():
    """WS changing every 64 bit clocks, SD filling the slot: a word of 64 bits overflowed the two's complement step
    (sigrok-dumps' signature/7791). Audio words are 32 bits at most: no such reading, and no error."""
    rng = np.random.default_rng(64)
    b, w, s = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    frames = [(int(x), int(y)) for x, y in rng.integers(-(1 << 31), 1 << 31, (40, 2))]
    end = gen.i2s(b, w, s, 10e-6, frames, 8000, slot=64, bits=64, fmt="left")
    cap = gen.quantize({"BCLK": b, "WS": w, "SD": s}, 25e6, end + 10e-6, rng)
    decode.export(decode.analyze(cap), cap)
