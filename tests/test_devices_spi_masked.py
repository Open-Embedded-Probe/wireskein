"""SPI device definitions with masked commands (the register's number in the first byte's low bits) and a register
read that names the chip: a CC1101 read as such by its VERSION (0x14); the same traffic without that read names
nothing (the table takes every first byte: fitting it tells nothing). Waveforms made here (gen.spi)."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def _devices(frames):
    rng = np.random.default_rng(17)
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    t = gen.spi(clk, mosi, miso, cs, 20e-6, frames, 1e6, gap=30e-6)
    cap = gen.quantize({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 20e6, t + 20e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, depth="device")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "spi"]
    return [(d.get("device"), d.get("level")) for d in c.get("devices") or []]


def test_cc1101_named_by_its_version():
    frames = [(b"\x30", b"\x0f"), (b"\xf1\x00", b"\x0f\x14"), (b"\xf0\x00", b"\x0f\x00"),
              (b"\x02\x06", b"\x0f\x0f"), (b"\x80\x00", b"\x0f\x29"), (b"\x36", b"\x0f")] * 3
    assert ("CC1101", "identified") in _devices(frames)


def test_no_name_without_the_read():
    frames = [(b"\x30", b"\x0f"), (b"\x02\x06", b"\x0f\x0f"), (b"\x80\x00", b"\x0f\x29"), (b"\x36", b"\x0f")] * 4
    assert not any(d == "CC1101" for d, _ in _devices(frames))
