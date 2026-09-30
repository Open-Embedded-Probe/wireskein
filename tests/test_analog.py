"""Analog channels in a .wsc (wsc-format §4.2, §4.3): raw values with their
linear conversion, volts, own rates and start times, acquisition settings,
and the .sr round trip."""

import shutil
import subprocess
import sys
from fractions import Fraction

import numpy as np
import pytest

from wireskein import wsc
from wireskein._engine import wscio
from wireskein.analyze import load, save

TICK = 20_000_000
ADC_RATE = Fraction(80_000_000, 1667)          # 47990.4... Hz: 416.75 ticks of 20 MHz per sample
PROBE = {"model": "esp32-p4-devkit", "firmware": "0.0.9", "chip": "ESP32-P4", "chip_revision": "1.0",
         "calibration": {"scheme": "curve-fitting-v1", "raw": "0a1b2c3d"}}


def logic(n=4000):
    lv = ((np.arange(n) // 100) & 1).astype(np.uint8)
    return wsc.Channel("CLK", wsc.pack(lv.tobytes()), n, acquisition={"pin": 47})


def test_raw_analog_keeps_values_settings_and_probe_info(tmp_path):
    raw = [0, 100, 4095, 2048, 7]
    a = wsc.analog_raw("VBUS", raw, ADC_RATE, width=16, t0_ticks=Fraction(104_000, 3), value_bits=12, zero=12,
                       scale_nv=805_860, pin=22, attenuation_db=12, reference={"source": "internal", "mv": 1100},
                       vrefint_raw=1489)
    wsc.write(tmp_path / "a.wsc", TICK, [logic(), a], probe=PROBE)
    head, (c, b) = wsc.read(tmp_path / "a.wsc")
    assert head["meta"]["probe"] == PROBE
    assert isinstance(b, wsc.AnalogChannel) and b.values() == raw and b.rate_hz == ADC_RATE
    assert b.t0_ticks == Fraction(104_000, 3) and (b.width, b.value_bits, b.zero, b.scale_nv) == (16, 12, 12, 805_860)
    assert b.acquisition == {"pin": 22, "attenuation_db": 12, "reference": {"source": "internal", "mv": 1100},
                             "vrefint_raw": 1489}
    assert c.acquisition == {"pin": 47}
    assert b.volts()[2] == pytest.approx((4095 - 12) * 805_860e-9)
    # ticks covers the analog channel's end: t0 + n * tick / rate
    assert head["ticks"] == max(4000, -(-(Fraction(104_000, 3) + 5 * TICK / ADC_RATE) // 1))


def test_model_and_wsc_round_trip(tmp_path):
    a = wsc.analog_raw("VBUS", list(range(50)), ADC_RATE, t0_ticks=Fraction(7, 2), pin=22)
    f = wsc.analog_volts("SINE", [0.0, 0.5, -0.25, float("nan")], 1_000_000)
    wsc.write(tmp_path / "a.wsc", TICK, [logic(), a, f])
    cap = load(tmp_path / "a.wsc")
    assert [t.name for t in cap.analog] == ["VBUS", "SINE"] and cap.channel("CLK").acquisition == {"pin": 47}
    vb = cap.analog[0]
    assert vb.ticks(TICK)[1] == pytest.approx(3.5 + TICK / float(ADC_RATE)) and vb.volts() is None   # no conversion
    save(tmp_path / "b.wsc", cap)
    _, chans = wsc.read(tmp_path / "b.wsc")
    assert chans[1].values() == list(range(50)) and chans[1].t0_ticks == Fraction(7, 2)
    assert np.isnan(chans[2].values()[3]) and chans[2].values()[:3] == [0.0, 0.5, -0.25]


def test_sr_round_trip_restores_raw_analog(tmp_path):
    a = wsc.analog_raw("VBUS", [10, 20, 30, 40], 2_000_000, t0_ticks=5, zero=0, scale_nv=1_000_000, pin=3)
    f = wsc.analog_volts("SINE", [0.25, -0.5], 5_000_000)
    wsc.write(tmp_path / "a.wsc", TICK, [logic(80), a, f])
    r = subprocess.run([sys.executable, "-m", "wireskein", "convert", str(tmp_path / "a.wsc"), str(tmp_path / "b.sr")],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert "1 logic + 2 analog channels" in r.stdout
    back = load(tmp_path / "b.sr")
    vb, sn = back.analog
    assert (vb.encoding, list(vb.values), vb.t0_ticks, vb.rate_hz, vb.acquisition) == \
        ("analog", [10, 20, 30, 40], 5, 2_000_000, {"pin": 3})
    assert (sn.encoding, list(sn.values), sn.rate_hz) == ("analog-f32", [0.25, -0.5], 5_000_000)
    if shutil.which("sigrok-cli"):                  # a plain .sr for sigrok: volts, repeated to the tick rate
        out = subprocess.run(["sigrok-cli", "-i", str(tmp_path / "b.sr"), "--show"], capture_output=True, text=True).stdout
        assert "VBUS: analog" in out and "SINE: analog" in out


@pytest.mark.parametrize("chan,why", [
    (wsc.analog_raw("ADC", [1, 2, 3], ADC_RATE, zero=0, scale_nv=1), "does not land on"),
    (wsc.analog_raw("ADC", [1, 2, 3], 1_000_000), "no conversion to volts"),
])
def test_sr_refuses_what_it_cannot_hold(tmp_path, chan, why):
    wsc.write(tmp_path / "a.wsc", TICK, [logic(), chan])
    r = subprocess.run([sys.executable, "-m", "wireskein", "convert", str(tmp_path / "a.wsc"), str(tmp_path / "b.sr")],
                       capture_output=True, text=True)
    assert r.returncode != 0 and why in r.stderr and not (tmp_path / "b.sr").exists()


def test_info_lists_analog(tmp_path):
    a = wsc.analog_raw("VBUS", [1, 2], ADC_RATE, value_bits=12, zero=0, scale_nv=805_860, attenuation_db=12)
    wsc.write(tmp_path / "a.wsc", TICK, [logic(), a], probe=PROBE)
    out = subprocess.run([sys.executable, "-m", "wireskein", "info", str(tmp_path / "a.wsc")],
                         capture_output=True, text=True, check=True).stdout
    assert "VBUS" in out and "analog" in out and "12 valid" in out and "attenuation_db" in out and "calibration" in out


def test_logic_checks_ignore_analog_channels(tmp_path):
    from wireskein.runlog import Recorder, level
    from wireskein.verify import verify
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[level("P0", 0), level("VBUS", 0)]):
        rec.capture(rec.armed(), 1e6, channels=[wsc.Channel("P0", wsc.pack(bytes(100)), 100),
                                                wsc.analog_volts("VBUS", [3.3] * 10, 100_000)])
    rec.close()
    rep = verify(tmp_path)
    assert [(r["check"], r["ok"]) for r in rep["results"]] == [("level", True), ("level", None)]


@pytest.mark.skipif(shutil.which("sigrok-cli") is None, reason="sigrok-cli not installed")
def test_sigrok_demo_logic_and_analog_together(tmp_path):
    out = tmp_path / "m.wsc"
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", "sigrok:demo",
                        "--channels", "CLK=D0,SQ=A0,SINE=A1", "--rate", "1M", "--samples", "2000", "-o", str(out)],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    cap = wscio.load(out)
    assert [c.name for c in cap.channels] == ["CLK"] and [a.name for a in cap.analog] == ["SQ", "SINE"]
    sq, sine = (a.volts() for a in cap.analog)
    assert set(np.round(sq, 3)) <= {-10.0, 10.0} and len(set(np.round(sq, 3))) == 2      # the demo's square wave
    assert sine.max() == pytest.approx(10, abs=0.1) and sine.min() == pytest.approx(-10, abs=0.1)
