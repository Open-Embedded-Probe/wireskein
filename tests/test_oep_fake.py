"""The OEP source over a real link to oep-client's virtual bench (virtual_bench_serve, fake_serve before
oep-client-python 0.0.29, on TCP). Skipped when oep-client is missing or its fake probe has no capture
(before oep-client-python 0.0.9)."""

import re
import subprocess
import sys

import numpy as np
import pytest

from wireskein import sources, fileformat

oep_client = pytest.importorskip("oep_client")
try:
    from oep_client import endpoint
    HAS_CAPTURE = "capture_slipped" in open(endpoint.__file__).read()
except ImportError:
    HAS_CAPTURE = False
import importlib.util
SERVE = next(m for m in ("oep_client.virtual_bench_serve", "oep_client.fake_serve")
             if m == "oep_client.fake_serve" or importlib.util.find_spec(m))
pytestmark = pytest.mark.skipif(not HAS_CAPTURE, reason="oep-client's fake probe has no capture")


@pytest.fixture
def probe():
    procs = []

    def start(*extra, profile="p4-x035"):
        p = subprocess.Popen([sys.executable, "-m", SERVE, "--tcp", "0", "--framing", "length",
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
    return [np.frombuffer(fileformat.unpack(c), np.uint8) for c in fileformat.read(path)[1]]


def test_three_channels_of_a_four_bit_stream(probe, tmp_path):
    src = probe()
    out = sources.capture(src, sources.Request([("A", "10"), ("B", "11"), ("C", "12")], 20_000_000, 1000), tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    assert head["tick_hz"] == [20_000_000, 1] and [c.n for c in chans] == [1000] * 3
    i = np.arange(1000)
    for k, lv in enumerate(levels(out)):                        # the fake's waveform: sample i is the counter i
        assert np.array_equal(lv, (i >> k) & 1), k
    assert head["meta"]["probe_channels"] == {"A": 10, "B": 11, "C": 12} and "start_ns" in head["meta"]
    assert "start_uncertainty_ns" in head["meta"]


def test_trigger_and_slip(probe, tmp_path):
    src = probe("--capture-slipped")
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 400, trigger=("B", "rise"), pretrigger=50)
    out = sources.capture(src, req, tmp_path / "t.wireskein")
    meta = fileformat.read(out)[0]["meta"]
    assert meta["time_base_slipped"] is True
    b = levels(out)[1]
    t = meta["trigger_tick"]
    assert t >= 50 and b[t] == 1 and b[t - 1] == 0                # a rising edge of B at the trigger index


def test_rate_is_the_probes_answer(probe, tmp_path):
    out = sources.capture(probe(), sources.Request([("A", "10")], 3_000_000, 100), tmp_path / "r.wireskein")
    tick = fileformat.tick_hz(fileformat.read(out)[0])
    assert tick <= 3_000_000 and 20_000_000 % tick == 0           # source / integer, at most what was asked


def test_a_pin_another_interface_listens_to_can_be_captured(probe, tmp_path):
    src = probe("--uart-plan")        # the fixture UART's saved plan holds channels 0 (RX) and 1 (TX); capture only listens
    out = sources.capture(src, sources.Request([("RX", "0"), ("TX", "1")], 1_000_000, 64), tmp_path / "s.wireskein")
    assert [c.n for c in fileformat.read(out)[1]] == [64, 64]


def test_esp32_sampler_profile(probe, tmp_path):
    out = sources.capture(probe(profile="esp32-v003"), sources.Request([("A", "4"), ("B", "5")], 1_000_000, 200),
                          tmp_path / "v.wireskein")
    i = np.arange(200)
    assert [np.array_equal(lv, (i >> k) & 1) for k, lv in enumerate(levels(out))] == [True, True]


HAS_ANALOG = hasattr(oep_client.capture if hasattr(oep_client, "capture") else __import__("oep_client.capture").capture,
                     "CaptureGroup")
analog_only = pytest.mark.skipif(not HAS_ANALOG, reason="oep-client has no analog / group (before 0.0.10)")
_cal = getattr(__import__("oep_client.capture").capture, "Calibration", None)
V1 = _cal is not None and "vrefint_nominal_mv" in getattr(_cal, "__dataclass_fields__", {})     # OEP v1 clients


@analog_only
def test_analog_alone_keeps_raw_values_and_what_the_probe_knows(probe, tmp_path):
    req = sources.Request([], 20_000_000, 512, analog=[("SQ", "16", None), ("SINE", "17", 3)], analog_rate=48_000)
    out = sources.capture(probe(), req, tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    sq, sine = chans
    assert isinstance(sq, fileformat.AnalogChannel) and sq.encoding == "analog" and sq.n == 512
    assert sq.value_bits == 12 and sq.zero is not None and sq.scale_nv
    v = sq.values()
    assert len(set(v)) == 2                                        # the fake's square wave on even channels
    assert sine.acquisition["frontend"]["frontend"] == 3 and sine.acquisition["attenuation_db"] == 12
    assert sq.acquisition["pin"] == 16 and "reference" in sq.acquisition and sq.acquisition["vrefint_raw"] == 1365
    p = head["meta"]["probe"]
    assert p["calibration"] and "boot_id" in p and "start_ns" in head["meta"]
    assert p.get("unit_id")                                        # core describe: the probe's own id
    if V1:                                                         # what an OEP v1 client also gives
        assert isinstance(sq.acquisition["vrefint_nominal_mv"], int)
        assert set(p["generation"]) == {"analog"} and isinstance(p["generation"]["analog"], int)


@analog_only
def test_logic_and_analog_together_in_a_group(probe, tmp_path):
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 2000, analog=[("SQ", "16", None)],
                          analog_rate=48_000)
    out = sources.capture(probe(), req, tmp_path / "g.wireskein")
    head, chans = fileformat.read(out)
    a, b, sq = chans
    assert isinstance(a, fileformat.Channel) and isinstance(sq, fileformat.AnalogChannel)
    assert head["tick_hz"] == [20_000_000, 1]
    # the fake starts the analog track 5 us (+-2 us) after the group, logic at once: 100 ticks of 20 MHz
    assert abs(float(sq.t0_ticks) - 100) <= 40 + 1
    assert sq.acquisition["start_uncertainty_ns"] == 2000 and "group_start_ns" in head["meta"]["probe"]
    if V1:
        assert set(head["meta"]["probe"]["generation"]) == {"logic", "analog"}
    assert sq.n == round(2000 * 48_000 / 20_000_000) or sq.n >= 1


@analog_only
def test_group_trigger_marks_both_tracks(probe, tmp_path):
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 2000, trigger=("B", "rise"), pretrigger=100,
                          analog=[("SQ", "16", None)], analog_rate=48_000, analog_samples=64)
    out = sources.capture(probe(), req, tmp_path / "t.wireskein")
    head, chans = fileformat.read(out)
    assert "trigger_tick" in head["meta"] and "trigger_index" in chans[2].acquisition
    assert "trigger_ns" in head["meta"]["probe"]


@analog_only
def test_cli_analog_only_needs_no_logic_rate(probe, tmp_path):
    src = probe()
    out = tmp_path / "a.wireskein"
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", src, "--analog", "SQ=16",
                        "--analog-rate", "20k", "--analog-samples", "200", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    (sq,) = fileformat.read(out)[1]
    assert sq.n == 200
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", src, "--channels", "A=10",
                        "--analog-rate", "20k", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode != 0 and "give --rate and --samples" in r.stderr


def test_a_trigger_that_never_comes_is_named():
    from wireskein.sources.oep import _waiting, oc_state

    class Waiting:
        def status(self):
            return (oc_state("waiting"), 0, 0, 0)

    def never():
        raise TimeoutError("capture did not finish")

    req = sources.Request([("A", "10")], 1_000_000, 100, trigger=("A", "rise"), timeout=2)
    with pytest.raises(TimeoutError, match=r"no trigger \(A:rise\) within 2 s"):
        _waiting(never, Waiting(), req)
    with pytest.raises(TimeoutError, match="did not finish"):          # no trigger asked: the plain timeout
        _waiting(never, Waiting(), sources.Request([("A", "10")], 1_000_000, 100, timeout=2))


HAS_MULTIRATE = __import__("importlib").util.find_spec("oep_client.multirate") is not None


@pytest.mark.skipif(not HAS_MULTIRATE, reason="oep-client has no multirate (before 0.0.29)")
def test_multirate_keeps_each_channel_as_asked(probe, tmp_path):
    """OEP multirate: a raw channel, a sample every 4 from 1, any-low per 32, latch-high per 8 - each read back as the
    probe's counter waveform (role k is bit k of the base sample number) summarized the same way."""
    src = probe()
    text = "A=10,B=11/4+1,C=12:any-low/32,D=13:latch-high/8"
    req = sources.Request(sources.parse_channels(text), 20_000_000, 1000, reduce=sources.parse_reduce(text),
                          trigger=("C", "fall"), pretrigger=40)
    out = sources.capture(src, req, tmp_path / "m.wireskein")
    head, chans = fileformat.read(out)
    assert [c["encoding"] for c in head["channels"]] == ["bits", "bits", "interval-any", "interval-latch"]
    a, b, c, d = chans
    n = a.n                                                       # base samples (the probe rounds up to its block)
    assert n >= 1000 and (b.step, b.phase, c.step, d.step) == (4, 1, 32, 8)
    t = head["meta"]["trigger_tick"]
    lvl = lambda k: [(i >> k) & 1 for i in range(n)]              # noqa: E731  (the virtual bench's counter)
    assert np.frombuffer(fileformat.unpack(a), np.uint8).tolist() == lvl(0)
    assert np.frombuffer(fileformat.unpack(b), np.uint8).tolist() == lvl(1)[1::4]
    any_low = [0 if 0 in lvl(2)[j:j + 32] else 1 for j in range(0, n - n % 32, 32)]
    assert c.values() == any_low and c.active == 0
    x = lvl(3)
    latch = [x[j + 7] | (any(n_ >= 1 and x[n_ - 1] == 0 and x[n_] == 1 for n_ in range(j, j + 8)) << 1)
             for j in range(0, n - n % 8, 8)]
    assert d.values() == latch and d.active == 1
    assert lvl(2)[t] == 0 and lvl(2)[t - 1] == 1                  # the trigger: C fell at that base sample
