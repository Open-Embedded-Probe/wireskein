"""One line told by pulse widths: RGB LEDs (WS2812), 1-Wire, IR remote codes (NEC)."""

import numpy as np
import pytest

from wireskein import verify
from wireskein._engine import gen
from wireskein.runlog import ir, onewire, rgb_led


def _cap(w, end, rate):
    return gen.quantize({"D": w}, rate, end, np.random.default_rng(0))


def _judge(cap, x):
    return verify._judge(cap, x, verify._pins_of(x))


def _leds(frames, rate=40e6, **kw):
    w, t = gen.Wave(0), 10e-6
    for f in frames:
        t = gen.rgb_led(w, t, bytes.fromhex(f), **kw)
    return _cap(w, t, rate)


def test_rgb_led_frames_and_times():
    cap = _leds(["ff005a010203", "102030"])
    ok, got, why = _judge(cap, rgb_led("D", ["ff005a010203", "102030"]))
    assert ok is True, why
    assert got["t0h"]["min_s"] == pytest.approx(0.4e-6, abs=30e-9) and got["resets"]["count"] == 1
    ok, _, why = _judge(cap, rgb_led("D", ["ff005a010203", "102031"]))
    assert ok is False and "#1 want 102031, got 102030" in why


def test_rgb_led_bits_out_of_the_datasheet():
    cap = _leds(["00ff00"], t1h=1.1e-6, period=1.4e-6)
    ok, _, why = _judge(cap, rgb_led("D", "00ff00"))
    assert ok is False and why.startswith("T1H 1.1 us > 950 ns"), why


def test_rgb_led_without_a_reset_at_the_end_is_unchecked():
    cap = _leds(["00ff00"], reset=20e-6)
    ok, _, why = _judge(cap, rgb_led("D", "00ff00"))
    assert ok is None and "before a reset" in why
    assert _judge(cap, rgb_led("D", "00ff00", reset_min_s=None))[0] is True


def _wire(rounds, rate=4e6, **kw):
    w, t = gen.Wave(1), 10e-6
    for data, presence in rounds:
        t = gen.onewire(w, t, bytes.fromhex(data), presence, **kw)
    return _cap(w, t, rate)


def test_onewire_resets_presence_and_bytes():
    cap = _wire([("cc44", True), ("ccbe", True)])
    ok, got, why = _judge(cap, onewire("D", ["cc44", "ccbe"]))
    assert ok is True, why
    assert got["presence"] == [True, True] and got["wait"]["min_s"] == pytest.approx(30e-6, abs=1e-6)
    ok, _, why = _judge(_wire([("33", False)]), onewire("D", "33"))
    assert ok is False and "without a presence pulse" in why
    assert _judge(_wire([("33", False)]), onewire("D", "33", presence=False))[0] is True


def test_onewire_short_reset_and_long_zero():
    ok, _, why = _judge(_wire([("cc", True)], reset=400e-6, low_0=130e-6, slot=140e-6), onewire("D", "cc"))
    assert ok is False and "reset 400 us < 480 us" in why and "0 low 130 us > 120 us" in why, why


@pytest.mark.parametrize("carrier", [38e3, None])
def test_nec_frames(carrier):
    w = gen.Wave(0 if carrier else 1)
    frames = [{"addr": 0x04, "cmd": 0x08}, {"repeat": True}, {"addr": 0x1234, "cmd": 0x55}]
    cap = _cap(w, gen.nec(w, 1e-3, frames, carrier), 1e6)
    ok, got, why = _judge(cap, ir("D", frames, carrier_hz=carrier))
    assert ok is True, why
    assert (got["carrier_hz"] is None) == (carrier is None)
    ok, _, why = _judge(cap, ir("D", [{"addr": 0x04, "cmd": 0x09}]))
    assert ok is False and "frames" in why


def test_nec_wrong_carrier():
    w = gen.Wave(0)
    cap = _cap(w, gen.nec(w, 1e-3, [{"addr": 1, "cmd": 2}], 36e3), 1e6)
    ok, _, why = _judge(cap, ir("D", carrier_hz=38e3))
    assert ok is False and "carrier" in why


def _claims(cap):
    from wireskein._engine import export, staged
    return [(c.protocol, c.verdict, export.final_of(c.node, export.layers_of(c.node))) for c in staged.analyze(cap).claims]


def test_analyze_finds_the_pulse_width_protocols():
    (p, v, fin), = _claims(_leds(["ff005a010203", "102030aabbcc", "0f0f0f"]))
    assert (p, v) == ("rgb_led", "confirmed") and [f["data"] for f in fin["frames"]][2] == "0f0f0f"
    (p, v, fin), = _claims(_wire([("cc44", True), ("ccbe00", True)]))
    assert p == "onewire" and v in ("confirmed", "likely") and [z["data"] for z in fin["resets"]] == ["cc44", "ccbe00"]
    w = gen.Wave(0)
    (p, v, fin), = _claims(_cap(w, gen.nec(w, 1e-3, [{"addr": 4, "cmd": 8}, {"repeat": True}], 38e3), 1e6))
    assert (p, v) == ("ir_nec", "confirmed") and fin["frames"][0]["cmd"] == 8


def test_a_pwm_is_not_an_led_chain():
    w = gen.Wave(0)
    gen.pwm(w, 0, 1e-3, 800e3, 0.4)
    assert _claims(_cap(w, 1e-3, 40e6)) == []
