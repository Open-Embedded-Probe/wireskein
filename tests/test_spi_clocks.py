"""spi with CS: the clocks in each CS window (frame_bits, bits=), a glitch on SCK inside a window, SCK moving while
CS is inactive (sck_idle_outside_cs=True) - what a test of an SPI controller checks besides the bytes and the mode."""

import numpy as np
import pytest

from wireskein import verify
from wireskein._engine import gen
from wireskein.runlog import Recorder, spi

RATE = 20e6
OUT = bytes.fromhex("a55a0f1234")


def run(tmp_path, mode, extra=None, **kw):
    clk, mosi, cs = gen.Wave(mode >> 1), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, None, cs, 10e-6, [(OUT, b"")], 1e6, mode=mode)
    cap = gen.quantize({"CS": cs, "SCK": clk, "MOSI": mosi}, RATE, end + 20e-6, np.random.default_rng(0))
    lv = np.zeros(cap.n_samples, np.uint8)
    for k, ch in enumerate(cap.channels):
        lv |= ch.level_at(np.arange(cap.n_samples)).astype(np.uint8) << k
    if extra:                                            # SCK is bit 1; set it high over [a, b) samples
        a, b = extra
        lv[a:b] |= 2
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[spi("SCK", mosi="MOSI", cs="CS", mode=mode, mosi_bytes=OUT.hex(), **kw)]):
        rec.capture(rec.armed(), RATE, interleaved=lv.tobytes(), names=[c.name for c in cap.channels])
    rec.close()
    (r,) = verify.verify(tmp_path)["results"]
    return r


@pytest.mark.parametrize("mode", range(4))
def test_forty_clocks_in_each_mode(tmp_path, mode):
    r = run(tmp_path, mode, bits=40, sck_idle_outside_cs=True)
    assert r["status"] == "ok" and r["measured"]["frame_bits"] == [40], r


def test_a_glitch_on_sck_while_cs_is_active(tmp_path):
    # a 100 ns pulse inside a low half period of mode 0 (bits start at 10.5 us, SCK high from 0.45 to 0.95 us of each)
    r = run(tmp_path, 0, extra=(int(13.6e-6 * RATE), int(13.7e-6 * RATE)))
    assert r["status"] == "ng" and "a glitch" in r["reason"], r


def test_sck_moving_while_cs_is_inactive(tmp_path):
    before = (int(2e-6 * RATE), int(3e-6 * RATE))       # a pulse on SCK before CS falls at 10 us
    assert run(tmp_path / "measured", 0, extra=before)["measured"]["sck_edges_outside_cs"] == 2
    r = run(tmp_path / "judged", 0, extra=before, sck_idle_outside_cs=True)
    assert r["status"] == "ng" and "while CS was inactive" in r["reason"], r


def test_a_count_of_clocks_that_differs(tmp_path):
    r = run(tmp_path, 0, bits=32)
    assert r["status"] == "ng" and "[40], want [32]" in r["reason"], r
