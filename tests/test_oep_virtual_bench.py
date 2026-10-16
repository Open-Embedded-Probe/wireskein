"""The OEP source over a real link to oep-client's virtual bench (virtual_bench_serve, on TCP). Skipped when
oep-client is missing."""

import re
import subprocess
import sys

import numpy as np
import pytest

from wireskein import sources, fileformat

pytest.importorskip("oep_client")
SERVE = "oep_client.virtual_bench_serve"


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
    for k, lv in enumerate(levels(out)):                        # the virtual bench's waveform: sample i is the counter i
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
    # a rising edge of B at the trigger index; a trigger before the pretrigger filled leaves it smaller (capture §3.3)
    assert t >= 1 and b[t] == 1 and b[t - 1] == 0


def test_rate_is_the_probes_answer(probe, tmp_path):
    out = sources.capture(probe(), sources.Request([("A", "10")], 3_000_000, 100), tmp_path / "r.wireskein")
    tick = fileformat.tick_hz(fileformat.read(out)[0])
    assert tick <= 3_000_000 and 20_000_000 % tick == 0           # source / integer, at most what was asked


def test_esp32_sampler_profile(probe, tmp_path):
    out = sources.capture(probe(profile="esp32-v003"), sources.Request([("A", "4"), ("B", "5")], 1_000_000, 200),
                          tmp_path / "v.wireskein")
    i = np.arange(200)
    assert [np.array_equal(lv, (i >> k) & 1) for k, lv in enumerate(levels(out))] == [True, True]


def test_analog_alone_keeps_raw_values_and_what_the_probe_knows(probe, tmp_path):
    req = sources.Request([], 20_000_000, 512, analog=[("SQ", "16", None), ("SINE", "17", 3)], analog_rate=48_000)
    out = sources.capture(probe(), req, tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    sq, sine = chans
    assert isinstance(sq, fileformat.AnalogChannel) and sq.encoding == "analog" and sq.n == 512
    assert sq.value_bits == 12 and sq.zero is not None and sq.scale_nv
    v = sq.values()
    assert len(set(v)) == 2                                        # the virtual bench's square wave on even channels
    fe = sine.acquisition["frontend"]
    assert fe["frontend"] == 3 and fe["range_min_mv"] < fe["range_max_mv"]       # describe's frontend (capture §3.5)
    assert sq.acquisition["pin"] == 16 and "reference" in sq.acquisition and sq.acquisition["vrefint_raw"] == 1365
    p = head["meta"]["probe"]
    assert p["calibration"] and "boot_id" in p and "start_ns" in head["meta"]
    assert p.get("unit_id")                                        # core describe: the probe's own id
    assert isinstance(sq.acquisition["vrefint_nominal_mv"], int)
    assert set(p["generation"]) == {"analog"} and isinstance(p["generation"]["analog"], int)


def test_logic_and_analog_together_in_a_group(probe, tmp_path):
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 2000, analog=[("SQ", "16", None)],
                          analog_rate=48_000)
    out = sources.capture(probe(), req, tmp_path / "g.wireskein")
    head, chans = fileformat.read(out)
    a, b, sq = chans
    assert isinstance(a, fileformat.Channel) and isinstance(sq, fileformat.AnalogChannel)
    assert head["tick_hz"] == [20_000_000, 1]
    # the virtual bench starts the analog track 5 us (+-2 us) after the group, logic at once: 100 ticks of 20 MHz
    assert abs(float(sq.t0_ticks) - 100) <= 40 + 1
    assert sq.acquisition["start_uncertainty_ns"] == 2000 and "group_start_ns" in head["meta"]["probe"]
    assert set(head["meta"]["probe"]["generation"]) == {"logic", "analog"}
    assert sq.n == round(2000 * 48_000 / 20_000_000) or sq.n >= 1


def test_group_trigger(probe, tmp_path):
    """A trigger on a logic channel with analog: both tracks marked when the client's group takes a trigger track,
    else refused in one line before opening (the current OEP v1 draft's group starts at once only, capture §4)."""
    from oep_client import capture as oc
    from wireskein.sources.oep import _group_trigger
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 2000, trigger=("B", "rise"), pretrigger=100,
                          analog=[("SQ", "16", None)], analog_rate=48_000, analog_samples=64)
    if not _group_trigger(oc):
        with pytest.raises(ValueError, match="only at once"):
            sources.capture(probe(), req, tmp_path / "t.wireskein")
        return
    out = sources.capture(probe(), req, tmp_path / "t.wireskein")
    head, chans = fileformat.read(out)
    assert "trigger_tick" in head["meta"] and "trigger_index" in chans[2].acquisition
    assert "trigger_ns" in head["meta"]["probe"]


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



def test_format_1_keeps_each_line_at_its_resolution(probe, tmp_path):
    """OEP capture format 1 (§1.3) on the bench's counter (channel k is bit k of the sample number): A every change
    (/1; it changes every sample, so the probe carries it raw), B raw, C per 4 (one change a quantum: also a Channel
    of step 4), D per 64 (8 changes a quantum: the count exact, where in the quantum hidden)."""
    from wireskein.analyze import load
    text = "A=10/1,B=11,C=12/4,D=13/64"
    req = sources.Request(sources.parse_channels(text), 20_000_000, 4096, resolution=sources.parse_resolution(text))
    out = sources.capture(probe(), req, tmp_path / "f1.wireskein")
    head, chans = fileformat.read(out)
    assert [c["encoding"] for c in head["channels"]] == ["events", "bits", "events", "events"]
    assert [c.get("step") for c in head["channels"]] == [1, 1, 4, 64]
    i = np.arange(4096)
    cap = load(out)
    for name, k in (("A", 0), ("B", 1)):                     # exact lines: every edge where the counter says
        assert np.array_equal(cap.channel(name).level_at(i), (i >> k) & 1), name
    c = cap.channel("C")                                     # per 4: right at each quantum's last tick, edges within it
    ends_ = np.arange(3, 4096, 4)
    assert c.step == 4 and np.array_equal(c.level_at(ends_), (ends_ >> 2) & 1)
    traces = {t.name: t for t in cap.events}                 # C (with its Channel view) and D (hidden changes)
    assert set(traces) == {"C", "D"} and traces["C"].hidden == 0
    dtrace = traces["D"]
    assert dtrace.step == 64 and dtrace.counts[0] == 7 and set(dtrace.counts[1:].tolist()) == {8} and dtrace.hidden == 64   # sample 0 is the start level: no change there
    data = head["meta"]["probe"]["data"]
    assert data["format"] == 1 and data["bytes"] <= data["worst_bytes"]


def test_a_resolution_on_sigrok_is_refused():
    with pytest.raises(ValueError, match="needs an OEP probe with capture format 1"):
        sources.run("sigrok:demo", sources.Request([("A", "D0")], 1_000_000, 100, resolution={"A": 4}))
