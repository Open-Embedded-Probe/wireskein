"""The OEP source over a real link to oep-client's virtual bench (virtual_bench_serve, on TCP). Skipped when
oep-client is missing."""

import re
import subprocess
import sys

import numpy as np
import pytest

from wireskein import sources, _fileformat as fileformat

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
    out = sources.capture(src, sources.Request([("A", "GPIO10"), ("B", "GPIO11"), ("C", "GPIO12")], 20_000_000, 1000), tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    assert head["tick_hz"] == [20_000_000, 1] and [c.n for c in chans] == [1000] * 3
    i = np.arange(1000)
    for k, lv in enumerate(levels(out)):                        # the virtual bench's waveform: sample i is the counter i
        assert np.array_equal(lv, (i >> k) & 1), k
    assert head["meta"]["probe_channels"] == {"A": 10, "B": 11, "C": 12} and "start_ns" in head["meta"]
    assert "start_uncertainty_ns" in head["meta"]


def test_trigger_and_slip(probe, tmp_path):
    src = probe("--capture-slipped")
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 400, trigger=("B", "rise"), pretrigger=50)
    out = sources.capture(src, req, tmp_path / "t.wireskein")
    meta = fileformat.read(out)[0]["meta"]
    assert meta["time_base_slipped"] is True
    b = levels(out)[1]
    t = meta["trigger_tick"]
    # armed once the pretrigger is there (the default, capture §3.3): the trigger index is the pretrigger, at a
    # rising edge of B
    assert t == 50 and b[t] == 1 and b[t - 1] == 0


def test_rate_is_the_probes_answer(probe, tmp_path):
    out = sources.capture(probe(), sources.Request([("A", "GPIO10")], 3_000_000, 100), tmp_path / "r.wireskein")
    tick = fileformat.tick_hz(fileformat.read(out)[0])
    assert tick <= 3_000_000 and 20_000_000 % tick == 0           # source / integer, at most what was asked


def test_esp32_sampler_profile(probe, tmp_path):
    out = sources.capture(probe(profile="esp32-v003"), sources.Request([("A", "GPIO4"), ("B", "GPIO5")], 1_000_000, 200),
                          tmp_path / "v.wireskein")
    i = np.arange(200)
    assert [np.array_equal(lv, (i >> k) & 1) for k, lv in enumerate(levels(out))] == [True, True]


def test_analog_alone_keeps_raw_values_and_what_the_probe_knows(probe, tmp_path):
    req = sources.Request([], 20_000_000, 512, analog=[("SQ", "GPIO16", None), ("SINE", "GPIO17", 3)], analog_rate=48_000)
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
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 2000, analog=[("SQ", "GPIO16", None)],
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
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 2000, trigger=("B", "rise"), pretrigger=100,
                          analog=[("SQ", "GPIO16", None)], analog_rate=48_000, analog_samples=64)
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
    req = sources.Request([("A", "GPIO10")], 20_000_000, 2000, trigger=("SQ", "rise", 1.0), pretrigger=100,
                          analog=[("SQ", "GPIO16", None)], analog_rate=48_000, analog_samples=64)
    out = sources.capture(probe(), req, tmp_path / "a.wireskein")
    head, chans = fileformat.read(out)
    sq = chans[1]
    i = sq.acquisition["trigger_index"]
    v = sq.values()
    volts = lambda x: (x - sq.zero) * sq.scale_nv * 1e-9            # noqa: E731
    assert volts(v[i]) >= 1.0 > volts(v[i - 1])                      # it crossed up there
    assert "trigger_tick" in head["meta"] and "trigger_ns" in head["meta"]["probe"]
    assert "cross_up" in head["meta"]["probe"]["trigger_caps"]["kinds"]   # the group's declaration, kept


def test_a_condition_the_group_does_not_declare_is_refused_before_bind():
    from oep_client import capture as oc
    from oep_client.acquire import _check_caps

    class Group:
        def trigger_caps(self):
            return oc.TriggerCaps(1, 1, 1, False, sum(1 << oc.COND[k] for k in ("level", "edge")), 0, None)

    an = type("T", (), {"fn": 3})()
    source = oc.TriggerSource([oc.Stage([[oc.Cond.above(an, 0, 100)]])])
    req = sources.Request([("A", "GPIO10")], 1_000_000, 100, trigger=("SQ", "high", 1.0), analog=[("SQ", "GPIO16", None)])
    probe = {}
    with pytest.raises(RuntimeError, match=r"cannot start its tracks on SQ:high:1.0V together: its capture group "
                                           r"triggers on level, edge \(trigger_caps"):
        _check_caps(Group(), source, req, "x", probe)
    assert probe["trigger_caps"]["kinds"] == ["level", "edge"]


def test_a_group_refused_says_which_track_before_setting_either(probe, tmp_path):
    """Both tracks asked with the values they would be set to (capture §3.3 query) before either is set: the analog's
    pretrigger, the logic's counted at the analog rate, is not below its 4 samples - refused, and where it came from."""
    req = sources.Request([("A", "GPIO10")], 20_000_000, 2000, trigger=("A", "rise"), pretrigger=1900,
                          analog=[("SQ", "GPIO16", None)], analog_rate=48_000, analog_samples=4)
    with pytest.raises(RuntimeError, match=r"each track asked before either was set\): analog \(its pretrigger \d+ of 4 "
                                           r"samples is the logic's 1900 .*\) with a pretrigger \(at most 3 samples "
                                           r"before the trigger here, of 4\)"):   # found by query: below its 4 samples
        sources.capture(probe(), req, tmp_path / "a.wireskein")


def test_repeat_without_a_trigger_runs_on_as_a_timeline(probe, tmp_path):
    """--mode repeat (capture mode 2): segments read and released as they come, each a part right after the one
    before; the counter's waveform runs on across them."""
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 1_000_000, 2000, mode="repeat", segments=4)
    out = sources.capture(probe(), req, tmp_path / "r.wireskein")
    head = fileformat.read_header(out)
    assert head["layout"] == "timeline" and head["complete"] is True
    assert [(p["ticks"], p["tick0"]) for p in head["parts"]] == [(2000, 2000 * k) for k in range(4)]
    _, (a0, _) = fileformat.read(out, part=0)
    _, (a3, _) = fileformat.read(out, part=3)
    first = np.frombuffer(fileformat.unpack(a0), np.uint8)[:a0.n]
    last = np.frombuffer(fileformat.unpack(a3), np.uint8)[:a3.n]
    start = int(np.flatnonzero(first != first[0])[0])            # where bit 0 of the counter first moves
    i = np.arange(2000)
    phase = (i + 6000 + (1 - start) % 2) & 1                      # four segments on: the same counter, 6000 later
    assert np.array_equal(last, phase) or np.array_equal(last, 1 - phase)


def test_repeat_with_a_trigger_is_one_part_per_trigger(probe, tmp_path):
    """--mode repeat with --trigger: rearm 1, each segment its own trigger (layout segments, its trigger_tick and
    start_ns); more segments than the probe holds, as it is read and released."""
    req = sources.Request([("A", "GPIO10")], 1_000_000, 500, mode="repeat", segments=40, trigger=("A", "rise"),
                          pretrigger=100)
    out = sources.capture(probe(), req, tmp_path / "r.wireskein")
    head = fileformat.read_header(out)
    assert head["layout"] == "segments" and len(head["parts"]) == 40
    assert all(p["meta"]["trigger_tick"] == 100 and "start_ns" in p["meta"] for p in head["parts"])
    for k in (0, 39):
        _, (a,) = fileformat.read(out, part=k)
        v = np.frombuffer(fileformat.unpack(a), np.uint8)[:a.n]
        assert v[99] == 0 and v[100] == 1                         # the rise at the trigger
    with pytest.raises(ValueError, match="segments"):
        sources.Request([("A", "GPIO10")], 1_000_000, 500, mode="repeat")
    with pytest.raises(ValueError, match="repeat only"):
        sources.Request([("A", "GPIO10")], 1_000_000, 500, segments=3)


def test_an_analog_stream_alone(probe, tmp_path):
    """--mode stream with analog channels only: the analog track streamed, a timeline at its own rate, the values as
    a one-shot reads them (the bench's analog_value)."""
    from oep_client.virtual_bench_capture import analog_value
    req = sources.Request([], 20_000, 0, mode="stream", duration=0.3, analog=[("SQ", "GPIO16", None)], analog_rate=20_000)
    out = sources.capture(probe(), req, tmp_path / "a.wireskein")
    head = fileformat.read_header(out)
    assert head["complete"] is True and head["layout"] == "timeline" and head["meta"]["stream"]["frames"] >= 1
    _, (sq,) = fileformat.read(out)
    assert sq.n > 1000 and sq.acquisition["pin"] == 16
    assert list(sq.values()[:200]) == [analog_value(0, i) for i in range(200)]   # the track's channel 0


def test_logic_and_analog_streamed_together(probe, tmp_path):
    """--mode stream with logic and analog: both tracks streamed in one group (capture §4.1), each part given the
    analog samples of its time, placed by the segments' start_ns."""
    from oep_client import capture as oc
    if not hasattr(oc.CaptureGroup, "stream"):
        pytest.skip("oep-client-python without CaptureGroup.stream")
    from oep_client.virtual_bench_capture import analog_value
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 1_000_000, 0, mode="stream", duration=0.3,
                          analog=[("SQ", "GPIO16", None)], analog_rate=20_000)
    out = sources.capture(probe(), req, tmp_path / "g.wireskein")
    head = fileformat.read_header(out)
    assert head["complete"] is True and len(head["parts"]) == 1
    _, (a, b, sq) = fileformat.read(out)
    span = a.n / 1e6                                              # the logic's seconds
    rate = sq.rate_hz.numerator / sq.rate_hz.denominator
    assert abs(sq.n / rate - span) < 0.01                          # the analog over the same time
    assert 0 <= sq.t0_ticks < 1e6 / rate * 2                       # from about the logic's start
    assert list(sq.values()[:100]) == [analog_value(0, i) for i in range(100)]


def test_an_analog_trigger_needs_its_voltage():
    with pytest.raises(ValueError, match="volts it crosses"):
        sources.Request([("A", "GPIO10")], 1_000_000, 100, trigger=("SQ", "rise"), analog=[("SQ", "GPIO16", None)])
    assert sources.parse_trigger("VDD:fall:2.9V") == ("VDD", "fall", 2.9)


def test_cli_analog_only_needs_no_logic_rate(probe, tmp_path):
    src = probe()
    out = tmp_path / "a.wireskein"
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", src, "--analog", "SQ=GPIO16",
                        "--analog-rate", "20k", "--analog-samples", "200", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    (sq,) = fileformat.read(out)[1]
    assert sq.n == 200
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", src, "--channels", "A=GPIO10",
                        "--analog-rate", "20k", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode != 0 and "give --rate and --samples" in r.stderr


def test_a_trigger_that_never_comes_is_named():
    from oep_client.acquire import _waiting, oc_state

    class Waiting:
        def status(self):
            return (oc_state("waiting"), 0, 0, 0)

    def never():
        raise TimeoutError("capture did not finish")

    req = sources.Request([("A", "GPIO10")], 1_000_000, 100, trigger=("A", "rise"), timeout=2)
    with pytest.raises(TimeoutError, match=r"no trigger \(A:rise\) within 2 s"):
        _waiting(never, Waiting(), req)
    with pytest.raises(TimeoutError, match="did not finish"):          # no trigger asked: the plain timeout
        _waiting(never, Waiting(), sources.Request([("A", "GPIO10")], 1_000_000, 100, timeout=2))



def _reductions():
    from oep_client import acquire
    if not hasattr(acquire, "Reduced"):
        pytest.skip("oep-client-python before capture format 1's reductions")


def test_format_1_keeps_each_line_at_its_resolution(probe, tmp_path):
    """OEP capture format 1 (§1.3) on the bench's counter (the k-th channel asked is bit k of the sample number): A
    and B raw, C every 4th sample (bits at step 4), D any_high over 4, E any_low over 64, F and G change_latch over 64
    (F, bit 5, changes twice an interval from the second: changed and came back; G, bit 6, once from the second)."""
    _reductions()
    from wireskein.decode import load
    text = "A=GPIO10,B=GPIO11,C=GPIO12/sample:4,D=GPIO13/any_high:4,E=GPIO14/any_low:64,F=GPIO15/change_latch:64," \
           "G=GPIO16/change_latch:64"
    req = sources.Request(sources.parse_channels(text), 20_000_000, 4096, resolution=sources.parse_resolution(text))
    out = sources.capture(probe(), req, tmp_path / "f1.wireskein")
    head, chans = fileformat.read(out)
    assert [c["encoding"] for c in head["channels"]] == ["bits", "bits", "bits", "any_high", "any_low", "change_latch",
                                                         "change_latch"]
    assert [c.get("step") for c in head["channels"]] == [1, 1, 4, 4, 64, 64, 64] and head["ticks"] == 4096
    k4, k64 = np.arange(1024), np.arange(64)
    a, _, c, d, e, f, g = chans
    assert np.array_equal(np.frombuffer(fileformat.unpack(a), np.uint8)[:4096], np.arange(4096) & 1)
    assert np.array_equal(np.frombuffer(fileformat.unpack(c), np.uint8)[:1024], k4 & 1)
    assert np.array_equal(d.values(), (k4 >> 1) & 1) and not e.values().any()
    ks, lv, counts = f.latches()
    assert ks.tolist() == list(range(64)) and lv.all() and counts.tolist() == [1] + [2] * 63
    assert np.array_equal(g.values(), (k64 & 1) | np.where(k64 > 0, 2, 0))
    cap = load(out)
    assert cap.channel("D").acquisition["reduction"] == "any_high" and cap.channel("D").step == 4
    traces = {t.name: t for t in cap.events}
    assert set(traces) == {"F", "G"} and traces["F"].hidden == 63 and traces["G"].hidden == 0
    assert cap.channel("G").step == 64 and "F" not in {ch.name for ch in cap.channels}
    data = head["meta"]["probe"]["data"]
    assert data["format"] == 1 and data["bytes"] <= data["expected_bytes"]


def test_a_part_of_reduced_lines_only_keeps_the_probes_length(probe, tmp_path):
    """No line of every sample: the part's length is the probe's (Part.samples), its last interval cut there."""
    _reductions()
    from oep_client import acquire
    if "samples" not in getattr(acquire.Part, "__dataclass_fields__", {}):
        pytest.skip("oep-client-python before Part.samples")
    text = "A=GPIO10/any_high:64,B=GPIO11/change_latch:64"
    req = sources.Request(sources.parse_channels(text), 20_000_000, 4000, resolution=sources.parse_resolution(text))
    out = sources.capture(probe(), req, tmp_path / "r.wireskein")
    head, (a, b) = fileformat.read(out)
    assert head["ticks"] == 4000 and a.n == b.n == 63 and a.end == 4032


def test_a_resolution_on_sigrok_is_refused():
    with pytest.raises(ValueError, match="needs an OEP probe with capture format 1"):
        sources.run("sigrok:demo", sources.Request([("A", "D0")], 1_000_000, 100, resolution={"A": ("sample", 4)}))


def test_arm_now_may_leave_a_shorter_pretrigger(probe, tmp_path):
    """--arm now (trigger_opts arm 0): looked for at once, so B's first rise (sample 2 of the counter) triggers before
    the 50 samples of pretrigger are there."""
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 400, trigger=("B", "rise"), pretrigger=50, arm="now")
    out = sources.capture(probe(), req, tmp_path / "n.wireskein")
    meta = fileformat.read(out)[0]["meta"]
    b = levels(out)[1]
    t = meta["trigger_tick"]
    assert 1 <= t < 50 and b[t] == 1 and b[t - 1] == 0


def test_a_stream_is_one_part_when_nothing_is_lost(probe, tmp_path):
    """--mode stream on the bench (capture mode 3): longer than one segment, every byte arrived, one timeline part
    with the counter's waveform throughout."""
    req = sources.Request([("A", "GPIO10"), ("B", "GPIO11")], 1_000_000, 0, mode="stream", duration=0.3)
    out = sources.capture(probe(), req, tmp_path / "s.wireskein")
    raw = fileformat.read_header(out)
    assert raw["complete"] is True and len(raw["parts"]) == 1 and raw["meta"]["stream"]["frames"] >= 2
    head, (a, b) = fileformat.read(out)
    n = a.n
    assert n > 1000 and "start_ns" in head["meta"]
    i = np.arange(n)
    assert np.array_equal(np.frombuffer(fileformat.unpack(b), np.uint8)[:n], (i >> 1) & 1)


def test_a_format_1_stream(probe, tmp_path):
    _reductions()
    req = sources.Request([("A", "GPIO10"), ("D", "GPIO13")], 1_000_000, 0, mode="stream", duration=0.3,
                          resolution={"D": ("change_latch", 64)})
    out = sources.capture(probe(), req, tmp_path / "f.wireskein")
    raw = fileformat.read_header(out)
    assert raw["complete"] is True
    for head, chans in fileformat.read_parts(out):              # a grid restart may cut parts: never a loss
        assert [c.encoding for c in chans] == ["bits", "change_latch"]


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
    from oep_client.acquire import _stream_parts
    cfg = SimpleNamespace(rate=1_000_000, width=8, positions=[0], event_d={})
    # segments of 100 samples at 1 MHz (100 us each); the one at position 200 was dropped (a gap of 100 bytes)
    got = fake_stream([(0, 100, 0), (100, 100, 100_000), (300, 100, 300_000)], [(200, 100)])
    req = sources.Request([("A", "GPIO1")], 1_000_000, 0, mode="stream", duration=1)
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert len(parts) == 1 and parts[0].channels[0].n == 200 and lost == [
        {"after_part": 0, "reason": "the probe dropped a segment (no room to send)", "stopped": True}]
    req = sources.Request([("A", "GPIO1")], 1_000_000, 0, mode="stream", duration=1, on_loss="continue")
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert [p.tick0 for p in parts] == [0, 300] and [p.channels[0].n for p in parts] == [200, 100]
    assert lost == [{"after_part": 0, "reason": "the probe dropped a segment (no room to send)", "ticks": 100}]
    assert parts[1].meta["start_ns"] == 300_000


def test_a_stream_that_lost_its_start_keeps_nothing_by_default():
    from types import SimpleNamespace
    from oep_client.acquire import _stream_parts
    cfg = SimpleNamespace(rate=1_000_000, width=8, positions=[0], event_d={})
    got = fake_stream([(100, 100, 100_000)], [])
    req = sources.Request([("A", "GPIO1")], 1_000_000, 0, mode="stream", duration=1)
    with pytest.raises(RuntimeError, match="dropped the stream's first 100 bytes"):
        _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    req = sources.Request([("A", "GPIO1")], 1_000_000, 0, mode="stream", duration=1, on_loss="continue")
    parts, lost = _stream_parts(FakeCap(), cfg, got, ["A"], [1], req)
    assert parts[0].tick0 == 0 and lost[0]["after_part"] == -1


def test_a_format_1_stream_reads_the_same_line_as_format_0(probe, tmp_path):
    """The same lines streamed as samples (format 0) and reduced (format 1): TX every 2nd sample, CLK (a change every 4
    samples) as change_latch over 2. Equal where both have it - TX at its samples, CLK at each interval's last tick -
    across segments."""
    _reductions()
    from wireskein.decode import load
    src = probe("--logic-wave", "10=uart:115200", "--logic-wave", "11=square:8")
    got = {}
    for name, res in (("f0", {}), ("f1", {"TX": ("sample", 2), "CLK": ("change_latch", 2)})):
        req = sources.Request([("TX", "GPIO10"), ("CLK", "GPIO11")], 200_000, 0, mode="stream", duration=0.3, resolution=res)
        out = sources.capture(src, req, tmp_path / f"{name}.wireskein")
        assert fileformat.read_header(out)["complete"] is True
        got[name] = load(out, part=0)
    f0, f1 = got["f0"], got["f1"]
    n = min(f0.n_samples, f1.n_samples) // 2 * 2
    for line, ticks in (("TX", np.arange(0, n, 2)), ("CLK", np.arange(1, n, 2))):
        a, b = f0.channel(line), f1.channel(line)
        assert np.array_equal(a.level_at(ticks), b.level_at(ticks)), line


def _polls(segments, gaps, polls):
    """Received states as the client fills them over `polls` calls: the same bytes and gaps as fake_stream, cut."""
    full = fake_stream(segments, gaps)
    cuts = sorted({0, len(full.data)} | {len(full.data) * k // polls for k in range(1, polls)})
    return full, cuts


def test_a_spooled_stream_is_the_same_as_one_put_together_at_the_end(tmp_path):
    """_Spool, fed poll by poll (the bytes taken out of the client's Received each time), gives the parts and losses
    that _stream_parts gives from everything at once - and keeps the bits on disk."""
    from types import SimpleNamespace
    from oep_client import capture as oc
    from oep_client.acquire import _Spool, _stream_parts
    cfg = SimpleNamespace(rate=1_000_000, width=8, positions=[0], event_d={})
    segs = [(0, 100, 0), (100, 100, 100_000), (300, 100, 300_000), (400, 60, 400_000)]
    req = sources.Request([("A", "GPIO1")], 1_000_000, 0, mode="stream", duration=1, on_loss="continue",
                          spool_dir=str(tmp_path))
    once = fake_stream(segs, [(200, 100)])
    want_parts, want_lost = _stream_parts(FakeCap(), cfg, once, ["A"], [1], req)
    full = fake_stream(segs, [(200, 100)])
    spool = _Spool(FakeCap(), cfg, ["A"], [1], req)
    got = oc.Received()
    for lo, hi in ((0, 37), (37, 150), (150, 260), (260, 360)):   # the client's bytes, a poll at a time
        if got.start is None:
            got.start = 0
        got.data += full.data[lo:hi]
        got.gaps += [(i - lo + len(got.data) - (hi - lo), n) for i, n in full.gaps if lo <= i < hi]
        got.segments.update({p: s for p, s in full.segments.items() if True})
        spool.take(got)
        assert len(got.data) == 0                                  # taken out: memory holds a poll's worth
    spool.take(got, final=True)
    assert [(p.tick0, p.channels[0].n) for p in spool.parts] == [(p.tick0, p.channels[0].n) for p in want_parts]
    assert spool.done_lost == want_lost
    for a, b in zip(spool.parts, want_parts):
        assert a.channels[0].bits[:] == b.channels[0].bits        # the same bits, read back from disk


def test_sections_on_the_probes_clock_cut_one_stream(probe, tmp_path):
    """A stream taken while the script steps through sections on the same session (acquire's during=): the headings
    on the probe's clock (Recorder.use_clock) cut each section out of the one capture (run-format §2.4)."""
    from oep_client import acquire, core, link
    from wireskein import verify
    from wireskein.runlog import Recorder, square
    from wireskein.sources.oep import capture_of
    hst = link.open_host(probe().removeprefix("oep:"))
    try:
        core.take(hst, 30_000, owner="wireskein test")
        rec = Recorder(tmp_path / "run")
        armed = rec.armed()

        def steps(wait):
            rec.use_clock(lambda: hst.clock_best(2))
            for k in range(2):
                wait(0.05)                                 # a quiet gap between sections
                with rec.section(1, f"step{k}", expect=[square("A", 500_000, 0.5)]):   # GPIO10: sample i's bit 0
                    wait(0.1)
            rec.use_clock(None)
        res = acquire.capture(hst, acquire.Request([("A", "GPIO10")], 1_000_000, 0, mode="stream", during=steps,
                                                   spool_dir=str(tmp_path)))
        rec.add(armed, capture_of(res))
        rec.close()
    finally:
        hst.end()
        hst.link.close()
    rep = verify.verify(tmp_path / "run")
    assert [(r["path"], r["status"]) for r in rep["results"]] == [("step0", "ok"), ("step1", "ok")], rep["results"]
    (a, b) = [r["measured"]["window"] for r in rep["results"]]
    assert 0 < a[0] < a[1] < b[0] < b[1]                   # each its own stretch of the one capture
    assert {r["capture"].partition("@")[0] for r in rep["results"]} == {"c0001.wireskein"}
