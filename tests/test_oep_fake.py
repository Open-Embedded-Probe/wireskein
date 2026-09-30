"""The OEP source over a real link to oep-client's fake probe (fake_serve on
TCP). Skipped when oep-client is missing or its fake probe has no capture
(before oep-client-python 0.0.9)."""

import re
import subprocess
import sys

import numpy as np
import pytest

from wireskein import sources, wsc

oep_client = pytest.importorskip("oep_client")
try:
    from oep_client import endpoint
    HAS_CAPTURE = "capture_slipped" in open(endpoint.__file__).read()
except ImportError:
    HAS_CAPTURE = False
pytestmark = pytest.mark.skipif(not HAS_CAPTURE, reason="oep-client's fake probe has no capture")


@pytest.fixture
def probe():
    procs = []

    def start(*extra, profile="p4-x035"):
        p = subprocess.Popen([sys.executable, "-m", "oep_client.fake_serve", "--tcp", "0", "--framing", "length",
                              "--profile", profile, *extra], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True)
        procs.append(p)
        port = re.search(r"PORT (\d+)", p.stdout.readline()).group(1)
        return f"oep:tcp://127.0.0.1:{port}"
    yield start
    for p in procs:
        p.stdin.close()
        p.wait(5)


def levels(path):
    return [np.frombuffer(wsc.unpack(c), np.uint8) for c in wsc.read(path)[1]]


def test_three_channels_of_a_four_bit_stream(probe, tmp_path):
    src = probe()
    out = sources.capture(src, sources.Request([("A", "10"), ("B", "11"), ("C", "12")], 20_000_000, 1000), tmp_path / "a.wsc")
    head, chans = wsc.read(out)
    assert head["tick_hz"] == [20_000_000, 1] and [c.n for c in chans] == [1000] * 3
    i = np.arange(1000)
    for k, lv in enumerate(levels(out)):                        # the fake's waveform: sample i is the counter i
        assert np.array_equal(lv, (i >> k) & 1), k
    assert head["meta"]["probe_channels"] == {"A": 10, "B": 11, "C": 12} and "start_us" in head["meta"]


def test_trigger_and_slip(probe, tmp_path):
    src = probe("--capture-slipped")
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 400, trigger=("B", "rise"), pretrigger=50)
    out = sources.capture(src, req, tmp_path / "t.wsc")
    meta = wsc.read(out)[0]["meta"]
    assert meta["time_base_slipped"] is True
    b = levels(out)[1]
    t = meta["trigger_index"]
    assert t >= 50 and b[t] == 1 and b[t - 1] == 0                # a rising edge of B at the trigger index


def test_rate_is_the_probes_answer(probe, tmp_path):
    out = sources.capture(probe(), sources.Request([("A", "10")], 3_000_000, 100), tmp_path / "r.wsc")
    tick = wsc.tick_hz(wsc.read(out)[0])
    assert tick <= 3_000_000 and 20_000_000 % tick == 0           # source / integer, at most what was asked


def test_a_pin_another_interface_listens_to_can_be_captured(probe, tmp_path):
    src = probe("--uart-plan")        # the fixture UART's saved plan holds channels 0 (RX) and 1 (TX); capture only listens
    out = sources.capture(src, sources.Request([("RX", "0"), ("TX", "1")], 1_000_000, 64), tmp_path / "s.wsc")
    assert [c.n for c in wsc.read(out)[1]] == [64, 64]


def test_esp32_sampler_profile(probe, tmp_path):
    out = sources.capture(probe(profile="esp32-v003"), sources.Request([("A", "4"), ("B", "5")], 1_000_000, 200),
                          tmp_path / "v.wsc")
    i = np.arange(200)
    assert [np.array_equal(lv, (i >> k) & 1) for k, lv in enumerate(levels(out))] == [True, True]
