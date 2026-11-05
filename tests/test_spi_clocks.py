"""spi with CS: the clocks in each CS window (frame_bits, bits=), a glitch on SCK inside a window, SCK moving while
CS is inactive and a window with no clock as notices (each at its tick; notices={kind: n or "ng"} limits them) - what
a test of an SPI controller checks besides the bytes and the mode."""

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
    r = run(tmp_path, mode, bits=40, notices={"sck_outside_cs": "ng", "empty_cs_window": "ng"})
    assert r["status"] == "ok" and r["measured"]["frame_bits"] == [40], r


def test_a_glitch_on_sck_while_cs_is_active(tmp_path):
    # a 100 ns pulse inside a low half period of mode 0 (bits start at 10.5 us, SCK high from 0.45 to 0.95 us of each)
    r = run(tmp_path, 0, extra=(int(13.6e-6 * RATE), int(13.7e-6 * RATE)))
    assert r["status"] == "ng" and "a glitch" in r["reason"], r


def test_sck_moving_while_cs_is_inactive(tmp_path):
    before = (int(2e-6 * RATE), int(3e-6 * RATE))       # a pulse on SCK before CS falls at 10 us
    r = run(tmp_path / "found", 0, extra=before)
    assert r["status"] == "ok" and r["measured"]["sck_edges_outside_cs"] == 2
    assert r["notices"] == [{"kind": "sck_outside_cs", "tick": 40, "count": 2, "part": 0}], r["notices"]
    assert run(tmp_path / "one", 0, extra=before, notices={"sck_outside_cs": 2})["status"] == "ok"
    r = run(tmp_path / "ng", 0, extra=before, notices={"sck_outside_cs": "ng"})
    assert r["status"] == "ng" and "2 sck_outside_cs, first at tick 40" in r["reason"], r
    r = run(tmp_path / "typo", 0, notices={"sck_outside": 1})
    assert r["status"] == "ng" and "no notice sck_outside in spi" in r["reason"], r


def test_a_count_of_clocks_that_differs(tmp_path):
    r = run(tmp_path, 0, bits=32)
    assert r["status"] == "ng" and "[40], want [32]" in r["reason"], r


def test_verify_marks_the_notices_in_the_capture(tmp_path):
    from wireskein import _fileformat as ff
    run(tmp_path, 0, extra=(int(2e-6 * RATE), int(3e-6 * RATE)))
    rep = verify.verify(tmp_path, mark=True)
    assert rep["summary"]["notices"] == 1 and "1 notice (" in verify.summary_line(rep)
    assert [(m["t"], m["label"], m["by"]) for m in ff.markers(tmp_path / "c0001.wireskein")] == [
        (40, "sck_outside_cs: t", "verify")]
    assert any("notice sck_outside_cs at tick 40: count 2" in x for x in verify.lines(rep, notices=True))
    verify.verify(tmp_path, mark=True)                   # again: its own markers replaced, not doubled
    assert len(ff.markers(tmp_path / "c0001.wireskein")) == 1
