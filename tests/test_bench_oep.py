"""On a real OEP probe with a fixture UART (the P4X on the bench): logic triggers, which need a line that moves while
the capture waits. wireskein only captures; the stimulus is this test's: the UART's TX planned onto the line logic
role 4 watches (channel 34, as dev-oep's checks), written from the same session right after the tracks start.

Run with WIRESKEIN_BENCH_OEP=usb:<serial> (holding the bench lock); skipped otherwise."""

import os

import pytest

from wireskein import fileformat, sources

TARGET = os.environ.get("WIRESKEIN_BENCH_OEP")
pytestmark = pytest.mark.skipif(not TARGET, reason="no bench probe (WIRESKEIN_BENCH_OEP)")
TX_CHANNEL = 34                      # fixture uart role 2 (TX) and logic role 4 on the same pin
LOGIC = [(f"L{k}", str(30 + k)) for k in range(8)]
TEXT = b"\x55" * 2048                # 8N1 0x55: a start bit, then every bit toggles; 180 ms, past the arming


@pytest.fixture
def uart_while_capturing(monkeypatch):
    """The fixture UART's TX joins wireskein's plan, and 0x55s go out once the capture has started."""
    pytest.importorskip("oep_client")
    from oep_client import capture as oc, core, fixture
    plan_apply = core.plan_apply
    state = {}

    def with_tx(hst, plan):
        uart = fixture.FixtureUartIO(hst, name=fixture.FixtureUart.NAME)
        state["uart"] = uart
        return plan_apply(hst, [*plan, (uart.uart.fn, 2, TX_CHANNEL)])

    def then_write(start):
        def run(self, *a, **kw):
            r = start(self, *a, **kw)
            uart = state["uart"]
            uart.configure(115200)
            uart.write(TEXT)
            return r
        return run

    monkeypatch.setattr(core, "plan_apply", with_tx)
    monkeypatch.setattr(oc.CaptureGroup, "start", then_write(oc.CaptureGroup.start))
    monkeypatch.setattr(oc.LogicCapture, "start", then_write(oc.LogicCapture.start))
    return state


def _check(out, samples, pretrigger, rate):
    head, chans = fileformat.read(out)
    logic = [c for c in chans if not isinstance(c, fileformat.AnalogChannel)]
    assert logic[0].n >= samples and head["meta"]["trigger_tick"] == pretrigger
    tx = fileformat.unpack(logic[4])
    assert tx[pretrigger - 1] == 1 and tx[pretrigger] == 0      # the start bit's falling edge, at the trigger
    probe = head["meta"]["probe"]
    if "trigger_ns" in probe:                                   # the event's time: pretrigger samples after start_ns
        assert abs(probe["trigger_ns"] - head["meta"]["start_ns"] - pretrigger * 10**9 // rate) <= \
            probe.get("trigger_uncertainty_ns", 0) + head["meta"].get("start_uncertainty_ns", 0)
    return head, chans


def test_a_logic_edge_alone_on_a_long_one_shot(uart_while_capturing, tmp_path):
    req = sources.Request(LOGIC, 10_000_000, 500_000, trigger=("L4", "fall"), pretrigger=20_000, timeout=15)
    _check(sources.capture(f"oep:{TARGET}", req, tmp_path / "a.wireskein"), 500_000, 20_000, 10_000_000)


def test_a_logic_edge_starts_the_analog_too(uart_while_capturing, tmp_path):
    req = sources.Request(LOGIC, 10_000_000, 200_000, trigger=("L4", "fall"), pretrigger=20_000, timeout=15,
                          analog=[("AN", "17", None)], analog_rate=20_000)
    head, chans = _check(sources.capture(f"oep:{TARGET}", req, tmp_path / "a.wireskein"), 200_000, 20_000, 10_000_000)
    an = chans[-1]
    assert an.acquisition["trigger_index"] == 40 and an.n >= 400   # 20000 at 10 MHz = 40 at 20 kHz; as long (20 ms)


def test_an_analog_crossing_starts_the_logic_too(tmp_path):
    """GPIO17 floats: its values wander, so the middle of a short look is crossed soon (no stimulus needed)."""
    look = sources.capture(f"oep:{TARGET}", sources.Request([], 20_000, 2000, analog=[("AN", "17", None)],
                                                            analog_rate=20_000), tmp_path / "look.wireskein")
    (a,) = fileformat.read(look)[1]
    v = a.values()
    volts = ((min(v) + max(v)) // 2 + 1 - a.zero) * a.scale_nv * 1e-9
    req = sources.Request(LOGIC, 10_000_000, 200_000, trigger=("AN", "rise", volts), pretrigger=20_000, timeout=15,
                          analog=[("AN", "17", None)], analog_rate=20_000)
    head, chans = fileformat.read(sources.capture(f"oep:{TARGET}", req, tmp_path / "a.wireskein"))
    assert chans[0].n >= 200_000 and head["meta"]["trigger_tick"] == 20_000
    assert chans[-1].acquisition["trigger_index"] == 40


def test_repeat_runs_on_as_a_timeline(tmp_path):
    req = sources.Request(LOGIC, 1_000_000, 20_000, mode="repeat", segments=10)
    head = fileformat.read_header(sources.capture(f"oep:{TARGET}", req, tmp_path / "r.wireskein"))
    assert head["layout"] == "timeline" and head["complete"] is True and len(head["parts"]) == 10
    assert all(p["ticks"] >= 20_000 for p in head["parts"])


def test_repeat_one_part_per_trigger(uart_while_capturing, tmp_path):
    """Each acquisition on its own falling edge of the fixture UART's TX (rearm 1)."""
    req = sources.Request(LOGIC, 10_000_000, 20_000, mode="repeat", segments=5, trigger=("L4", "fall"),
                          pretrigger=2_000, timeout=15)
    try:
        out = sources.capture(f"oep:{TARGET}", req, tmp_path / "r.wireskein")
    except RuntimeError as e:      # oep-probe-arduino 0.0.36 takes a trigger in one-shot only
        if "logic in repeat with a trigger" in str(e):
            pytest.xfail(f"the probe has no trigger in repeat: {e}")
        raise
    head = fileformat.read_header(out)
    assert head["layout"] == "segments" and len(head["parts"]) == 5
    for k, p in enumerate(head["parts"]):
        assert p["meta"]["trigger_tick"] == 2_000
        tx = fileformat.unpack(fileformat.read(out, part=k)[1][4])
        assert tx[1_999] == 1 and tx[2_000] == 0


def test_an_analog_stream_alone(tmp_path):
    req = sources.Request([], 20_000, 0, mode="stream", duration=3.0, analog=[("AN", "17", None)], analog_rate=20_000)
    try:
        out = sources.capture(f"oep:{TARGET}", req, tmp_path / "a.wireskein")
    except RuntimeError as e:      # oep-probe-arduino 0.0.36: the analog track has no streaming
        if "cannot capture analog with this mode" in str(e):
            pytest.xfail(f"the probe does not stream its analog track: {e}")
        raise
    head = fileformat.read_header(out)
    assert head["complete"] is True, head.get("lost")
    (an,) = fileformat.read(out)[1]
    rate = an.rate_hz.numerator / an.rate_hz.denominator
    assert an.n >= 0.9 * 3.0 * rate and an.zero is not None and an.scale_nv
