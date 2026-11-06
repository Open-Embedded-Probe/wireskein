"""spi(edge_sample="before"): a data line changing in the same sample as a sampling edge, taken as changing after it
(a part whose output hold is shorter than a sample: CH32V103's SPI slave, th(SO) 15 ns at 40 MS/s). Lines made here."""

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import spi
from wireskein.verify import check_spi

HALF = 10                                  # SCK 1 MHz at 20 ticks a period (20 MS/s)


def lines(miso_bytes: bytes, late: int):
    """Mode 0 (CPOL 0, sampled on the rise): MOSI changes on the fall (mid-low); MISO changes `late` ticks after the
    fall that ends each bit, at the byte's end after the rise (0: in the rise's very sample)."""
    cs, clk, mosi, miso = [100], [], [], []
    t = 120
    lvm = lvs = 0
    out = 0x5A
    last_rise = None
    for k, b in enumerate(miso_bytes):
        for i in range(8):
            bm, bs = (out >> (7 - i)) & 1, (b >> (7 - i)) & 1
            if bm != lvm:
                mosi.append(t - HALF // 2)
                lvm = bm
            if bs != lvs:
                # a byte's first bit is put out at the last rise of the byte before, `late` ticks after it (the very
                # first one before the clock starts)
                miso.append(last_rise + late if i == 0 and last_rise is not None else t - HALF // 2)
                lvs = bs
            clk += [t, t + HALF]
            last_rise = t
            t += 2 * HALF
        t += 4 * HALF
    cs.append(t + 50)
    n = t + 200
    return Capture(20e6, n, [Channel("SCK", 0, np.asarray(clk)), Channel("MOSI", 0, np.asarray(sorted(mosi))),
                             Channel("MISO", 0, np.asarray(sorted(miso))), Channel("CS", 1, np.asarray(cs))])


def test_a_change_in_the_edge_s_sample_fails_by_default_and_reads_before_when_asked():
    data = bytes([0x81, 0x7E, 0x81, 0x7E])                 # each byte's first bit differs from the last one's
    c = lines(data, late=0)
    ok, got, why = check_spi(c, spi("SCK", "MOSI", "MISO", "CS", mode=0, miso_bytes=data.hex()))
    assert ok is False and "does not read it" in why
    ok, got, why = check_spi(c, spi("SCK", "MOSI", "MISO", "CS", mode=0, miso_bytes=data.hex(), edge_sample="before"))
    assert ok, why
    mg = got["margins"]["MISO"]
    assert mg["hold_s"] == 0 and mg["hold_under_sample"] and mg["sample_s"] == 1 / 20e6


def test_a_hold_of_a_sample_or_more_needs_nothing():
    data = bytes([0x81, 0x7E])
    ok, got, why = check_spi(lines(data, late=1), spi("SCK", "MOSI", "MISO", "CS", mode=0, miso_bytes=data.hex()))
    assert ok, why and "hold_under_sample" not in got["margins"]["MISO"]


def test_only_before():
    with pytest.raises(ValueError):
        spi("SCK", edge_sample="after")
