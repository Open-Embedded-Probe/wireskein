"""spi() with a CS sampled coarser than the clock (a slower step on a slow line, to save a probe's bandwidth): its
change to active is seen up to a CS sample late, and the window is taken to start that much earlier. Lines made
here: SCK 10 MHz at 80 MS/s, CS at 10, 5 and 1.25 MS/s."""

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import spi
from wireskein.verify import check_spi

RATE, PER = 80e6, 8
DATA = bytes([0xA5, 0x3C, 0x81])


def lines(lead: int, cs_step: int, gap: int = 400, phase: int = 3):
    """Four frames of DATA, CS active `lead` ticks before the first clock; CS seen at its next sample of cs_step."""
    clk, mosi, cs = [], [], []
    t, lv = 1000, 0
    for _ in range(4):
        cs.append(t - lead)
        for b in DATA:
            for i in range(8):
                bit = (b >> (7 - i)) & 1
                if bit != lv:
                    mosi.append(t - PER // 2)
                    lv = bit
                clk += [t, t + PER // 2]
                t += PER
        cs.append(t)
        t += gap
    cs = np.asarray(cs)
    cs = phase + np.ceil((cs - phase) / cs_step).astype(int) * cs_step
    keep = np.ones(len(cs), bool)                  # a rise and a fall in one CS sample: not seen
    for i in range(1, len(cs)):
        if keep[i - 1] and cs[i] == cs[i - 1]:
            keep[i - 1] = keep[i] = False
    return Capture(RATE, t + 500, [Channel("SCK", 0, np.asarray(clk)), Channel("MOSI", 0, np.asarray(mosi)),
                                   Channel("CS", 1, cs[keep], cs_step, phase)])


@pytest.mark.parametrize("cs_step", [8, 16, 64])
@pytest.mark.parametrize("lead", [40, 4, 1])
def test_a_coarse_cs_still_holds_every_clock(cs_step, lead):
    ok, got, why = check_spi(lines(lead, cs_step), spi("SCK", "MOSI", None, "CS", mode=0,
                                                       mosi_bytes=(DATA * 4).hex(), bits=24))
    assert ok, why
    assert got["frame_bits"] == [24] * 4 and got["cs_sample_s"] == cs_step / RATE


def test_frames_closer_than_a_cs_sample_run_together():
    # CS inactive for less than a CS sample between frames: the rise and the fall are not seen, one window
    ok, got, why = check_spi(lines(4, 64, gap=8), spi("SCK", "MOSI", None, "CS", mode=0, bits=24))
    assert ok is False and len(got["frame_bits"]) < 4
