"""Lines the probe kept on its own clock (oep_client.probe_log.rows()): sections placed in a capture by probe_ns
(docs/run-format.ja.md §2.4)."""

import json
import time

from wireskein import fileformat, verify
from wireskein.runlog import Recorder, pulses

START_NS = 5_000_000_000              # the capture's first sample on the probe's clock
RATE = 1_000_000                      # 1 MHz: a tick is 1000 ns


def _wave(n: int, highs: list[tuple[int, int]]) -> bytes:
    b = bytearray(n)
    for a, z in highs:
        b[a:z] = b"\x01" * (z - a)
    return bytes(b)


def _rows(lines: list[tuple[int, str]], armed: float, boot=7, probe="usb:p4x"):
    """(tick, text) as probe_log.rows() gives them: t on time.monotonic() (the capture's start at armed)."""
    return [{"t": armed + tick * 1e-6, "src": "marker" if text.startswith("#") else "dut", "text": text,
             "probe_ns": START_NS + tick * 1000, "boot_id": boot, "probe": probe} for tick, text in lines]


def _run(tmp_path, lines, highs, unc_ns=0, expect=None, lost=(), **meta):
    rec = Recorder(tmp_path)
    with rec.section(1, "test"):
        time.sleep(0.002)
        armed = rec.armed()
        rec.capture(armed, RATE, interleaved=_wave(1000, highs), names=["P0"], start_ns=START_NS,
                    start_uncertainty_ns=unc_ns, **{"boot_id": 7, "source": "oep:usb:p4x", **meta})
        rec.probe_lines(_rows(lines, armed), lost=list(lost), probe="usb:p4x", boot_id=7)
        time.sleep(0.002)                     # the probe's lines end before the test does
    rec.expect("test/burst", expect or [pulses("P0", count=3)])
    rec.close()
    return verify.verify(tmp_path)


BURST = [(120, 130), (160, 170), (200, 210), (500, 510), (600, 610)]     # 3 pulses in the burst, 2 after it


def test_a_section_of_the_probes_lines_is_its_part_of_the_capture(tmp_path):
    rep = _run(tmp_path, [(100, "## burst"), (250, "wrote 3"), (300, "##")], BURST)
    (r,) = rep["results"]
    assert r["status"] == "ok", r
    assert r["measured"]["window"] == [100, 300] and r["capture"] == "c0001.wireskein@100:300"
    log = json.loads((tmp_path / "run.json").read_text())["log"]
    assert [e["text"] for e in log] == ["# test", "## burst", "wrote 3", "##", "#"]     # sorted on the host clock


def test_the_probes_lines_are_markers_of_the_capture(tmp_path):
    _run(tmp_path, [(100, "## burst"), (250, "wrote 3"), (300, "##")], BURST, unc_ns=5000)
    ms = fileformat.markers(tmp_path / "c0001.wireskein")
    assert [(m["t"], m["label"], m["by"], m["uncertainty_ticks"]) for m in ms] == [
        (100, "## burst", "probe-log", 5), (250, "wrote 3", "probe-log", 5), (300, "##", "probe-log", 5)]


def test_a_verdict_that_depends_on_the_bounds_uncertainty_is_unchecked(tmp_path):
    # a pulse 4 ticks after the close: inside the section if the bound is 5 ticks later
    rep = _run(tmp_path, [(100, "## burst"), (300, "##")], BURST + [(304, 306)], unc_ns=5000)
    (r,) = rep["results"]
    assert r["status"] == "unchecked" and "depends on where the section's bounds fall" in r["reason"], r
    rep = _run(tmp_path / "far", [(100, "## burst"), (300, "##")], BURST + [(320, 330)], unc_ns=5000)
    assert rep["results"][0]["status"] == "ok"


def test_another_boot_is_not_placed(tmp_path):
    rep = _run(tmp_path, [(100, "## burst"), (300, "##")], BURST, boot_id=0)
    (r,) = rep["results"]
    assert r["status"] == "ng" and "boot" in r["reason"], r


def test_a_section_begun_before_the_capture_is_clipped(tmp_path):
    rep = _run(tmp_path, [(-11, "## burst"), (300, "##")], BURST)
    (r,) = rep["results"]
    assert r["status"] == "ok" and r["measured"]["clipped"] == ["begin"] and r["measured"]["window"] == [0, 300]


def test_lines_the_probe_lost_fail_the_run(tmp_path):
    rep = _run(tmp_path, [(100, "## burst"), (300, "##")], BURST, lost=["log overflowed: 2 lines"])
    assert [(r["check"], r["status"]) for r in rep["results"]] == [("pulses", "ok"), ("probe_log", "ng")]
    assert verify.failed(rep)
