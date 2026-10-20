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
    # armed once the pretrigger is there (the default, capture §3.3): the trigger index is the pretrigger, at a
    # rising edge of B
    assert t == 50 and b[t] == 1 and b[t - 1] == 0


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


def test_a_logic_trigger_starts_the_analog_too(probe, tmp_path):
    """--trigger on a logic line with analog channels: the capture-group's trigger source (capture §4.4) - both
    tracks linked, each with its own pretrigger, cut on the one event; its time and uncertainty kept."""
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 2000, trigger=("B", "rise"), pretrigger=100,
                          analog=[("SQ", "16", None)], analog_rate=48_000, analog_samples=64)
    out = sources.capture(probe(), req, tmp_path / "t.wireskein")
    head, chans = fileformat.read(out)
    b = np.frombuffer(fileformat.unpack(chans[1]), np.uint8)
    t = head["meta"]["trigger_tick"]
    assert t == 100 and b[t] == 1 and b[t - 1] == 0               # armed after the pretrigger: the rise at its end
    assert "trigger_index" in chans[2].acquisition
    probe_meta = head["meta"]["probe"]
    assert "trigger_ns" in probe_meta and probe_meta["trigger_uncertainty_ns"] >= 0


def test_an_analog_trigger_starts_the_logic_too(probe, tmp_path):
    """--trigger SQ:rise:VOLTS: the analog line crossing up through that voltage (in its ADC values by the answer's
    scale) is the group's event."""
    req = sources.Request([("A", "10")], 20_000_000, 2000, trigger=("SQ", "rise", 1.0), pretrigger=100,
                          analog=[("SQ", "16", None)], analog_rate=48_000, analog_samples=64)
    out = sources.capture(probe(), req, tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    sq = chans[1]
    i = sq.acquisition["trigger_index"]
    v = sq.values()
    volts = lambda x: (x - sq.zero) * sq.scale_nv * 1e-9            # noqa: E731
    assert volts(v[i]) >= 1.0 > volts(v[i - 1])                      # it crossed up there
    assert "trigger_tick" in head["meta"] and "trigger_ns" in head["meta"]["probe"]


def test_an_analog_trigger_needs_its_voltage():
    with pytest.raises(ValueError, match="volts it crosses"):
        sources.Request([("A", "10")], 1_000_000, 100, trigger=("SQ", "rise"), analog=[("SQ", "16", None)])
    assert sources.parse_trigger("VDD:fall:2.9V") == ("VDD", "fall", 2.9)


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


def test_arm_now_may_leave_a_shorter_pretrigger(probe, tmp_path):
    """--arm now (trigger_opts arm 0): looked for at once, so B's first rise (sample 2 of the counter) triggers before
    the 50 samples of pretrigger are there."""
    req = sources.Request([("A", "10"), ("B", "11")], 20_000_000, 400, trigger=("B", "rise"), pretrigger=50, arm="now")
    out = sources.capture(probe(), req, tmp_path / "n.wireskein")
    meta = fileformat.read(out)[0]["meta"]
    b = levels(out)[1]
    t = meta["trigger_tick"]
    assert 1 <= t < 50 and b[t] == 1 and b[t - 1] == 0


def test_a_stream_is_one_part_when_nothing_is_lost(probe, tmp_path):
    """--mode stream on the bench (capture mode 3): longer than one segment, every byte arrived, one timeline part
    with the counter's waveform throughout."""
    req = sources.Request([("A", "10"), ("B", "11")], 1_000_000, 0, mode="stream", duration=0.3)
    out = sources.capture(probe(), req, tmp_path / "s.wireskein")
    raw = fileformat.read_header(out)
    assert raw["complete"] is True and len(raw["parts"]) == 1 and raw["meta"]["stream"]["frames"] >= 2
    head, (a, b) = fileformat.read(out)
    n = a.n
    assert n > 1000 and "start_ns" in head["meta"]
    i = np.arange(n)
    assert np.array_equal(np.frombuffer(fileformat.unpack(b), np.uint8)[:n], (i >> 1) & 1)


def test_a_format_1_stream(probe, tmp_path):
    req = sources.Request([("A", "10"), ("D", "13")], 1_000_000, 0, mode="stream", duration=0.3,
                          resolution={"D": 64})
    out = sources.capture(probe(), req, tmp_path / "f.wireskein")
    raw = fileformat.read_header(out)
    assert raw["complete"] is True
    for head, chans in fileformat.read_parts(out):              # a grid restart may cut parts: never a loss
        assert [c.encoding for c in chans] == ["bits", "events"]


class FakeCap:
    """Stands for oep-client's LogicCapture in _stream_parts (format 0)."""


def fake_stream(segments, gaps):
    """A Received of format 0 (w 8, one channel at bit 0): segments as (position, samples, start_ns), each sample a
    byte; `gaps` (data index, bytes skipped) as the probe's drops."""
    from oep_client import capture as oc
    got = oc.Received()
    got.start = segments[0][0]
    for pos, n, ns in segments:
        got.segments[pos] = oc.SegmentInfo(pos, n, ns, 100)
        got.data += bytes([(pos + k) & 1 for k in range(n)])
    got.gaps = gaps
    return got


def test_stream_parts_split_at_a_loss_and_stop_there_by_default():
    from types import SimpleNamespace
    from wireskein.sources.oep import _stream_parts
    cfg = SimpleNamespace(rate=1_000_000, width=8, positions=[0], event_d={})
    # segments of 100 samples at 1 MHz (100 us each); the one at position 200 was dropped (a gap of 100 bytes)
    got = fake_stream([(0, 100, 0), (100, 100, 100_000), (300, 100, 300_000)], [(200, 100)])
    req = sources.Request([("A", "1")], 1_000_000, 0, mode="stream", duration=1)
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert len(parts) == 1 and parts[0].channels[0].n == 200 and lost == [
        {"after_part": 0, "reason": "the probe dropped a segment (no room to send)", "stopped": True}]
    req = sources.Request([("A", "1")], 1_000_000, 0, mode="stream", duration=1, on_loss="continue")
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert [p.tick0 for p in parts] == [0, 300] and [p.channels[0].n for p in parts] == [200, 100]
    assert lost == [{"after_part": 0, "reason": "the probe dropped a segment (no room to send)", "ticks": 100}]
    assert parts[1].meta["start_ns"] == 300_000


def test_a_stream_that_lost_its_start_keeps_nothing_by_default():
    from types import SimpleNamespace
    from wireskein.sources.oep import _stream_parts
    cfg = SimpleNamespace(rate=1_000_000, width=8, positions=[0], event_d={})
    got = fake_stream([(100, 100, 100_000)], [])
    req = sources.Request([("A", "1")], 1_000_000, 0, mode="stream", duration=1)
    with pytest.raises(RuntimeError, match="dropped the stream's first 100 bytes"):
        _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    req = sources.Request([("A", "1")], 1_000_000, 0, mode="stream", duration=1, on_loss="continue")
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert parts[0].tick0 == 0 and lost[0]["after_part"] == -1


def test_a_format_1_stream_reads_the_same_line_as_format_0(probe, tmp_path):
    """The same UART-like line streamed as samples (format 0) and as every change (/1, format 1): equal where both
    have it, across segments and the blocks the probe carried raw (a change at a segment's first sample counted)."""
    from wireskein.analyze import load
    src = probe("--logic-wave", "10=uart:115200", "--logic-wave", "11=square:8")
    got = {}
    for name, res in (("f0", {}), ("f1", {"TX": 1, "CLK": 1})):
        req = sources.Request([("TX", "10"), ("CLK", "11")], 200_000, 0, mode="stream", duration=0.3, resolution=res)
        out = sources.capture(src, req, tmp_path / f"{name}.wireskein")
        assert fileformat.read_header(out)["complete"] is True
        caps = [load(out, part=k) for k in range(len(fileformat.read_header(out)["parts"]))]
        got[name] = caps
    for line in ("TX", "CLK"):
        a, b = got["f0"][0].channel(line), got["f1"][0].channel(line)
        n = min(got["f0"][0].n_samples, got["f1"][0].n_samples)
        i = np.arange(n)
        assert np.array_equal(a.level_at(i), b.level_at(i)), line
