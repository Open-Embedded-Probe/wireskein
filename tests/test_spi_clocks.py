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
    assert [n for n in r["notices"] if n["kind"] == "sck_outside_cs"] == [
        {"kind": "sck_outside_cs", "tick": 40, "count": 2, "part": 0}], r["notices"]
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
    assert rep["summary"]["notices"] == 2 and "2 notices (" in verify.summary_line(rep)   # and mode_ambiguous
    assert [(m["t"], m["label"], m["by"]) for m in ff.markers(tmp_path / "c0001.wireskein")
            if m["label"].startswith("sck")] == [(40, "sck_outside_cs: t", "verify")]
    assert any("notice sck_outside_cs at tick 40: count 2" in x for x in verify.lines(rep, notices=True))
    verify.verify(tmp_path, mark=True)                   # again: its own markers replaced, not doubled
    assert len(ff.markers(tmp_path / "c0001.wireskein")) == 2


def test_the_modes_a_receiver_could_read_it_in(tmp_path):
    # the generator puts MOSI out a tenth of a half period after the trailing edge: in mode 0 it is stable around
    # both edges, so a receiver of mode 0 or mode 1 reads the same bytes; the mode the changes say is still 0
    r = run(tmp_path, 0)
    assert r["measured"]["mode"] == 0 and r["measured"]["modes_ok"] == [0, 1], r["measured"]
    m = r["measured"]["margins"]["MOSI"]
    assert m["setup_s"] > 0 and m["hold_s"] > 0 and m["other_setup_s"] > 0 and m["other_hold_s"] > 0
    assert abs(m["setup_s"] - 0.45e-6) < 0.06e-6 and abs(m["other_hold_s"] - 0.05e-6) < 0.06e-6
    r = run(tmp_path / "strict", 0, setup_min_s=0.5e-6)       # MOSI is set up 0.45 us before the sampling edge
    assert r["status"] == "ng" and "MOSI setup" in r["reason"], r["reason"]


def test_modes_ok_comes_from_the_lines_alone_in_the_analysis(tmp_path):
    from wireskein._engine import export
    clk, mosi, cs = gen.Wave(0), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, None, cs, 10e-6, [(OUT, b"")], 1e6, mode=1)
    cap = gen.quantize({"CS": cs, "SCK": clk, "MOSI": mosi}, RATE, end + 20e-6, np.random.default_rng(0))
    res = verify._decode(cap, {"protocols": ["spi"]})         # no pins, no bytes, no mode given
    (c,) = [c for c in res.claims if c.protocol == "spi"]
    L = export.layers_of(c.node)
    # the generator shifts a sample after the leading edge: a receiver of mode 0 reads it too, one bit later
    assert L["modes_ok"] == [0, 1] and [m["mode"] for m in L["modes"]] == [0, 1]
    by = {m["mode"]: m["bytes"]["MOSI"] for m in L["modes"]}
    assert by[1] == OUT.hex() and by[0] != by[1]             # the modes read different bytes: the analysis says both


def test_mode_is_one_the_bytes_read_in(tmp_path):
    # the generator's mode 1 reads other bytes in mode 0: with the bytes given, mode 1 is the one candidate
    r = run(tmp_path / "given", 1)
    assert r["status"] == "ok" and r["measured"]["mode_candidates"] == [1] and not r["notices"], r
    # its mode 0 reads the same bytes in mode 1 too: both candidates, a notice, and mode=0 still passes
    r = run(tmp_path / "both", 0)
    assert r["status"] == "ok" and r["measured"]["mode_candidates"] == [0, 1]
    assert [n["kind"] for n in r["notices"]] == ["mode_ambiguous"]
    # all zero bytes read the same in both: mode 1 is a candidate too, a notice says so, and mode=1 passes
    clk, mosi, cs = gen.Wave(0), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, None, cs, 10e-6, [(bytes(5), b"")], 1e6, mode=0)
    cap = gen.quantize({"CS": cs, "SCK": clk, "MOSI": mosi}, RATE, end + 20e-6, np.random.default_rng(0))
    lv = np.zeros(cap.n_samples, np.uint8)
    for k, ch in enumerate(cap.channels):
        lv |= ch.level_at(np.arange(cap.n_samples)).astype(np.uint8) << k
    for want, status in ((1, "ok"), (2, "ng")):
        rec = Recorder(tmp_path / f"zeros{want}")
        with rec.section(1, "t", expect=[spi("SCK", mosi="MOSI", cs="CS", mode=want, mosi_bytes="00" * 5)]):
            rec.capture(rec.armed(), RATE, interleaved=lv.tobytes(), names=[c.name for c in cap.channels])
        rec.close()
        (r,) = verify.verify(tmp_path / f"zeros{want}")["results"]
        assert r["status"] == status and r["measured"]["mode_candidates"] == [0, 1], r
        assert [n["kind"] for n in r["notices"]] == ["mode_ambiguous"] and r["notices"][0]["candidates"] == [0, 1]
