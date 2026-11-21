"""SD mode's CMD line (declared in decl/sdmmc.toml): commands and responses with their CRC7, an R3 without one, an
R2 of 136 bits. The CRC7 computed here (a register of bits); waveforms made here. And a .sr of 40 channels (samples
of 5 bytes, as sigrok-dumps' SD captures) read."""
import io
import zipfile

import numpy as np

from wireskein import decode
from wireskein._engine import gen


def crc7(bits):
    reg = 0
    for b in bits:
        fb = ((reg >> 6) & 1) ^ b
        reg = (reg << 1) & 0x7F
        if fb:
            reg ^= 0x09
    return reg


def msb(v, n):
    return [(v >> (n - 1 - i)) & 1 for i in range(n)]


def frame48(direction, index, arg):
    body = [0, direction] + msb(index, 6) + msb(arg, 32)
    return body + msb(crc7(body), 7) + [1]


def r3(ocr):
    return [0, 0] + [1] * 6 + msb(ocr, 32) + [1] * 7 + [1]


def r2(reg):
    body = msb(reg >> 8, 120)
    return [0, 0] + [1] * 6 + body + msb(crc7(body), 7) + [1]


def wave(frames, f=400e3):
    """CLK running throughout; CMD changes after the clock's fall, idles high; a few idle clocks between frames."""
    clk, cmd = gen.Wave(0), gen.Wave(1)
    h, t = 0.5 / f, 10e-6
    bits = []
    for fr in frames:
        bits += [1] * 8 + fr
    bits += [1] * 8
    for b in bits:
        cmd.set(t + 0.2 * h, b)
        clk.set(t + h, 1)
        clk.set(t + 2 * h, 0)
        t += 2 * h
    return {"CLK": clk, "CMD": cmd}, t


def test_commands_and_responses():
    rng = np.random.default_rng(6)
    cid = int.from_bytes(rng.bytes(16), "big")
    frames = [frame48(1, 0, 0), frame48(1, 8, 0x1AA), frame48(0, 8, 0x1AA), frame48(1, 55, 0), frame48(0, 55, 0x120),
              frame48(1, 41, 0x40FF8000), r3(0xC0FF8000), frame48(1, 2, 0), r2(cid), frame48(1, 3, 0),
              frame48(0, 3, 0xAAAA0500)]
    waves, t = wave(frames)
    cap = gen.quantize(waves, 4e6, t + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "sdmmc" and c["roles"] == {"clk": "CLK", "cmd": "CMD"}
    got = [(f["dir"], f.get("index"), f.get("arg", f.get("ocr"))) for f in c["result"]["frames"]]
    assert got == [("CMD", 0, 0), ("CMD", 8, 0x1AA), ("RSP", 8, 0x1AA), ("CMD", 55, 0), ("RSP", 55, 0x120),
                   ("CMD", 41, 0x40FF8000), ("R3", None, 0xC0FF8000), ("CMD", 2, 0), ("R2", None, None),
                   ("CMD", 3, 0), ("RSP", 3, 0xAAAA0500)]
    assert all(f["ok"] for f in c["result"]["frames"])


def test_sr_of_five_byte_samples(tmp_path):
    """A .sr of 40 channels: unitsize 5; channel 38 toggling, the others low."""
    n = 1000
    lv = (np.arange(n) // 10) & 1
    samples = np.zeros((n, 5), np.uint8)
    samples[:, 4] = (lv << 6).astype(np.uint8)            # bit 38: byte 4, bit 6
    meta = "[global]\nsigrok version=0.5.2\n[device 1]\ncapturefile=logic-1\ntotal probes=40\nsamplerate=1 MHz\n"
    meta += "".join(f"probe{i + 1}=D{i}\n" for i in range(40)) + "unitsize=5\n"
    p = tmp_path / "forty.sr"
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("version", "2")
        z.writestr("metadata", meta)
        z.writestr("logic-1-1", samples.tobytes())
    cap = decode.load(p)
    ch = cap.channel("D38")
    assert ch.initial == 0 and list(ch.edges[:3]) == [10, 20, 30]
    assert len(cap.channel("D0").edges) == 0
