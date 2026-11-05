"""Lines the probe kept on its own clock (oep_client.probe_log.rows()): sections placed in a capture by probe_ns
(docs/run-format.ja.md §2.4)."""

import json
import time

from wireskein import _fileformat as fileformat, verify
from wireskein.runlog import Recorder, pulses

START_NS = 5_000_000_000              # the capture's first sample on the probe's clock
RATE = 1_000_000                      # 1 MHz: a tick is 1000 ns


def _wave(n: int, highs: list[tuple[int, int]]) -> bytes:
    b = bytearray(n)
    for a, z in highs:
        b[a:z] = b"\x01" * (z - a)
    return bytes(b)


def _rows(lines: list[tuple[int, str]], armed: float, boot=7, probe="usb:p4x"):
    """(tick, text) as probe_log.rows() gives them: t on time.monotonic() seconds (the capture's start at armed, ns)."""
    return [{"t": armed / 1e9 + tick * 1e-6, "src": "marker" if text.startswith("#") else "dut", "text": text,
             "probe_ns": START_NS + tick * 1000, "boot_id": boot, "probe": probe} for tick, text in lines]


def _run(tmp_path, lines, highs, unc_ns=0, expect=None, lost=(), line_probe="usb:p4x", **meta):
    rec = Recorder(tmp_path)
    with rec.section(1, "test"):
        time.sleep(0.002)
        armed = rec.armed()
        rec.capture(armed, RATE, interleaved=_wave(1000, highs), names=["P0"], start_ns=START_NS,
                    start_uncertainty_ns=unc_ns, **{"boot_id": 7, "source": "oep:usb:p4x", **meta})
        rec.probe_lines(_rows(lines, armed, probe=line_probe), lost=list(lost), probe=line_probe, boot_id=7)
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


def test_a_probe_is_its_place_whatever_route_reached_it(tmp_path):
    # the capture opened as oep://p4x?via=usb, the lines' probe its place oep://p4x (host guide §5.3: no route)
    lines = [(100, "## burst"), (250, "wrote 3"), (300, "##")]
    at = {"source": "oep:oep://p4x?via=usb", "probe": {"address": "oep://p4x", "boot_id": 7}}
    (r,) = _run(tmp_path / "same", lines, BURST, line_probe="oep://p4x", **at)["results"]
    assert r["status"] == "ok", r
    rep = _run(tmp_path / "other", lines, BURST, line_probe="oep://other", **at)
    assert all(x["status"] != "ok" for x in rep["results"]), rep["results"]


class _Reading:
    """oep_client's ClockReading as Recorder reads it: the probe's uptime_ns, its boot_id, the host time."""
    def __init__(self, tick: int, host_ns: int, boot_id: int = 7):
        self.uptime_ns, self.boot_id, self.host_ns = START_NS + tick * 1000, boot_id, host_ns


def _clock(ticks: list[int]):
    """A probe clock read at these ticks of the capture, one per heading."""
    it = iter(ticks)
    return lambda: _Reading(next(it), time.monotonic_ns())


def test_host_headings_on_the_probes_clock_cut_one_long_capture(tmp_path):
    # one capture taken on and on; the host changes the condition in each section (docs/run-format.ja.md §2.4)
    rec = Recorder(tmp_path)
    armed = rec.armed()
    rec.use_clock(_clock([100, 300, 450, 700]), probe="oep://p4x")
    with rec.section(1, "burst", expect=[pulses("P0", count=3)]):
        rec.command("PWM on")
    with rec.section(1, "after", expect=[pulses("P0", count=2)]):
        rec.command("PWM slow")
    rec.use_clock(None)
    rec.capture(armed, RATE, interleaved=_wave(1000, BURST), names=["P0"], start_ns=START_NS,
                probe={"address": "oep://p4x", "boot_id": 7})
    rec.close()
    rep = verify.verify(tmp_path)
    assert [(r["path"], r["status"], r["measured"]["window"]) for r in rep["results"]] == [
        ("burst", "ok", [100, 300]), ("after", "ok", [450, 700])], rep["results"]
    log = json.loads((tmp_path / "run.json").read_text())["log"]
    assert [(e["text"], e.get("probe_ns"), e.get("probe")) for e in log if e["src"] == "marker"] == [
        ("# burst", START_NS + 100_000, "oep://p4x"), ("#", START_NS + 300_000, "oep://p4x"),
        ("# after", START_NS + 450_000, "oep://p4x"), ("#", START_NS + 700_000, "oep://p4x")]


def test_a_section_on_the_probes_clock_by_its_own_argument(tmp_path):
    rec = Recorder(tmp_path)
    armed = rec.armed()
    with rec.section(1, "test"):
        with rec.section(2, "burst", expect=[pulses("P0", count=3)], clock=_clock([100, 300]), probe="oep://p4x"):
            rec.command("PWM on")
        rec.capture(armed, RATE, interleaved=_wave(1000, BURST), names=["P0"], start_ns=START_NS,
                    probe={"address": "oep://p4x", "boot_id": 7})
    rec.close()
    (r,) = verify.verify(tmp_path)["results"]
    assert (r["path"], r["status"], r["measured"]["window"]) == ("test/burst", "ok", [100, 300]), r


def test_a_clock_that_cannot_be_read_fails_the_run(tmp_path):
    def broken():
        raise TimeoutError("no answer")
    rec = Recorder(tmp_path)
    armed = rec.armed()
    with rec.section(1, "burst", expect=[pulses("P0", count=5)], clock=broken, probe="oep://p4x"):
        rec.capture(armed, RATE, interleaved=_wave(1000, BURST), names=["P0"], start_ns=START_NS,
                    probe={"address": "oep://p4x", "boot_id": 7})
    rec.close()
    rep = verify.verify(tmp_path)
    assert [(r["check"], r["status"]) for r in rep["results"]] == [("pulses", "ok"), ("probe_log", "ng"),
                                                                    ("probe_log", "ng")], rep["results"]
    assert "no answer" in rep["results"][1]["reason"] and verify.failed(rep)
