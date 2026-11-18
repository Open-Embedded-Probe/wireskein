"""spi(mode=...) reads at the asked mode's sampling edge: a data line that changes a sample before the launching edge
(a probe's lines skewed by a sample: a CH32H417 seen at 40 MS/s, every bit's change 25 ns before SCK's fall in mode 0)
makes the lines' own changes say the other CPHA, but a receiver in the asked mode reads it, and so does the check.
The mode the changes say stays measured. Lines made here."""

import numpy as np

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import spi
from wireskein.verify import check_spi

HALF = 50                                  # SCK at 100 ticks a period


def lines(data: bytes, mode: int):
    """CPOL from the mode; MOSI changes one tick before each launching edge (the trailing one in CPHA 0, the leading
    one in CPHA 1), well clear of the sampling edge."""
    cpol, cpha = mode >> 1, mode & 1
    cs, clk, mosi = [100], [], []
    t, lv = 300, 0
    bits = [(b >> (7 - i)) & 1 for b in data for i in range(8)]
    if cpha == 0 and bits[0] != lv:
        mosi.append(200)                                   # the first bit set up before the first edge
        lv = bits[0]
    for k, bit in enumerate(bits):
        lead, trail = t, t + HALF
        if cpha == 1 and bit != lv:
            mosi.append(lead - 1)
            lv = bit
        clk += [lead, trail]
        nxt = bits[k + 1] if k + 1 < len(bits) else None
        if cpha == 0 and nxt is not None and nxt != lv:
            mosi.append(trail - 1)
            lv = nxt
        t += 2 * HALF
    cs.append(t + 100)
    return Capture(40e6, t + 400, [Channel("SCK", cpol, np.asarray(clk)), Channel("MOSI", 0, np.asarray(mosi)),
                                   Channel("CS", 1, np.asarray(cs))])


def test_each_mode_reads_as_asked_though_the_changes_say_the_other():
    data = bytes.fromhex("d0144fdb9c")
    for mode in range(4):
        ok, got, why = check_spi(lines(data, mode), spi("SCK", "MOSI", None, "CS", mode=mode, mosi_bytes=data.hex()))
        assert ok, (mode, why)
        assert got["bytes"]["MOSI"] == data.hex()
        assert got["mode"] == mode ^ 1                     # what the changes say: the other CPHA


def test_the_wrong_mode_still_fails():
    data = bytes.fromhex("d0144fdb9c")
    ok, got, why = check_spi(lines(data, 0), spi("SCK", "MOSI", None, "CS", mode=2, mosi_bytes=data.hex()))
    assert ok is False


def test_a_window_starting_with_cs_already_active_reads_in_the_asked_mode():
    # the section starts inside a selection: no whole CS window, so the lines do not say CPOL; the asked mode's is
    # taken (v0.0.29 raised KeyError: 'cpol' here)
    data = bytes.fromhex("0b4b9909")
    c = lines(data, 0)
    cs = c.channel("CS")
    cut = Capture(c.rate, c.n_samples, [ch if ch.name != "CS" else Channel("CS", 0, np.asarray(cs.edges[1:]))
                                        for ch in c.channels])
    ok, got, why = check_spi(cut, spi("SCK", "MOSI", None, "CS", mode=0, mosi_bytes=data.hex()))
    assert ok, why
    assert got["bytes"]["MOSI"] == data.hex()
