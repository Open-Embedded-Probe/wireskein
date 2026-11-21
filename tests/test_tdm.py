"""TDM (a frame sync once a frame, three slots or more on one data line) read as the I2S plugin's `format: "tdm"`:
the frame sync a pulse of a bit (DSP mode A: the first slot's MSB a bit after it), a pulse of a slot (mode B: at it)
or 50 % duty; the slot width and the word's place told by the padding. Waveforms made here (not gen.i2s)."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen


def tdm(frames, slot, bits, offset, fs_width, rate_hz=48000):
    """BCLK, FS and SD: each frame `len(frame)` slots of `slot` bits, the words MSB first at a slot's start (zeros
    after); FS high for `fs_width` bits from `offset` bits before the frame's first bit. SD and FS change on BCLK's
    falling edge."""
    n = len(frames[0])
    h = 0.5 / (rate_hz * n * slot)
    bclk, fs, sd = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    dat = []
    for f in frames:
        for v in f:
            v &= (1 << bits) - 1
            dat += [(v >> (bits - 1 - j)) & 1 for j in range(bits)] + [0] * (slot - bits)
    total = len(dat)
    sync = [0] * total
    for k in range(len(frames)):
        for j in range(fs_width):
            i = k * n * slot - offset + j
            if 0 <= i < total:
                sync[i] = 1
    t = 10e-6
    for g in range(total):
        bclk.set(t, 0)
        fs.set(t, sync[g])
        sd.set(t, dat[g])
        bclk.set(t + h, 1)
        t += 2 * h
    bclk.set(t, 0)
    return {"BCLK": bclk, "FS": fs, "SD": sd}, t


@pytest.mark.parametrize("n,slot,bits,offset,width", [
    (8, 32, 24, 1, 1),          # DSP mode A, 24-bit words in slots of 32
    (4, 32, 16, 0, 32),         # mode B, a slot-long pulse
    (6, 16, 12, 1, 1),          # six slots of 16
    (8, 32, 32, 1, 128),        # 50 % duty, words filling their slots (the slot taken as 32)
])
def test_tdm_frames(n, slot, bits, offset, width):
    rng = np.random.default_rng(n * slot + bits)
    lim = 1 << (bits - 1)
    frames = [[int(x) for x in row] for row in rng.integers(-lim, lim, (60, n))]
    waves, end = tdm(frames, slot, bits, offset, width)
    cap = gen.quantize(waves, 50e6, end + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "i2s" and c["roles"] == {"sck": "BCLK", "ws": "FS", "sd": "SD"}
    p = c["params"]
    assert (p["format"], p["slots_per_frame"], p["slot_bits"], p["word_bits"]) == ("tdm", n, slot, bits)
    assert p.get("format_assumed", False) == (bits == slot)
    got = [f["slots"] for f in c["result"]["frames"]]
    # whole frames from the first frame sync on (the one FS leads by a bit has no sync before it)
    k = frames.index(got[0])
    assert k <= 1 and got == frames[k:k + len(got)] and len(got) >= len(frames) - 2


def test_two_audio_buses_at_twice_the_bit_clock():
    """A TDM bus (8 kHz, 4 slots of 16) and an I2S one (16 kHz, 32-bit slots): the second's bit clock is twice the
    first's, so its WS, a square wave in step with the first's frames, read as the first's SD (its words 0x00, 0xff,
    0x07, 0xf8: steps). Each bus is read with its own lines (2026-11-21, synth-protomix-145)."""
    rng = np.random.default_rng(145)
    b1, f1, s1 = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    b2, w2, s2 = gen.Wave(0), gen.Wave(0), gen.Wave(0)
    tdm_frames = [[int(x) for x in row] for row in rng.integers(-2048, 2048, (40, 4))]
    end1 = gen.tdm(b1, f1, s1, 100e-6, tdm_frames, fs=8000, slot=16, bits=12, offset=1, fs_width=1)
    i2s_frames = [(int(a), int(b)) for a, b in rng.integers(-(1 << 23), 1 << 23, (80, 2))]
    end2 = gen.i2s(b2, w2, s2, 137.3e-6, i2s_frames, 16000, slot=32, bits=24, fmt="left")   # (a phase of its own)
    cap = gen.quantize({"BCLK1": b1, "FS1": f1, "SD1": s1, "BCLK2": b2, "WS2": w2, "SD2": s2}, 200e6,
                       max(end1, end2) + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    got = sorted(tuple(sorted(c["roles"].items())) for c in ex["claims"]
                 if c["protocol"] == "i2s" and c["verdict"] in ("confirmed", "likely"))
    assert got == [(("sck", "BCLK1"), ("sd", "SD1"), ("ws", "FS1")), (("sck", "BCLK2"), ("sd", "SD2"), ("ws", "WS2"))]
