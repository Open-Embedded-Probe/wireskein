"""The checks of lines and peripherals on captures from the P4X (e8f60ae84e28, oep-probe-arduino 0.0.39): its
MCPWM, RMT, fixture SPI / I2C controllers and patterns it sent (tests/data/p4x-i1, each VCD with the settings
that made it in the JSON of the same name)."""

import json
from pathlib import Path

import numpy as np
import pytest

from wireskein import runlog as R
from wireskein import verify
from wireskein._engine import vcdio
from wireskein._engine.model import Capture, Channel

D = Path(__file__).parent / "data" / "p4x-i1"


def _load(name):
    """The VCD (1 ns ticks) back on the probe's own sample grid, so the resolution is the probe's."""
    meta = json.loads((D / f"{name}.json").read_text())
    cap = vcdio.read_vcd(D / f"{name}.vcd")
    k = cap.rate / meta["rate_hz"]
    chs = [Channel(c.name, c.initial, np.round(c.edges / k).astype(np.int64)) for c in cap.channels]
    return Capture(meta["rate_hz"], int(round(cap.n_samples / k)), chs)


def _judge(name, x):
    return verify._judge(_load(name), x, verify._pins_of(x))


MCPWM = "mcpwm-100k-phase-deadtime"


def test_mcpwm_phase_skew_and_dead_time():
    # one timer at 100 kHz: b 90 deg after a; comp_hi rises 200 ns late, comp_lo (its inverse) 500 ns after it falls
    ok, got, why = _judge(MCPWM, R.phase("pwm_a", "pwm_b_90deg", 90, tol_deg=2, freq_hz=100e3))
    assert ok is True and got["phase_deg"] == 90, why
    assert _judge(MCPWM, R.phase("pwm_a", "pwm_b_90deg", 90, tol_deg=0.5))[0] is None     # 1.8 deg a sample
    ok, got, why = _judge(MCPWM, R.skew("pwm_a", "comp_hi", 60e-9, 200e-9, edge="rise"))
    assert ok is True and got["median_s"] == pytest.approx(200e-9), why
    ok, got, why = _judge(MCPWM, R.dead_time("comp_hi", "comp_lo", min_s=150e-9))
    assert ok is True and got["hi_to_lo"]["min_s"] == pytest.approx(500e-9), why
    assert got["lo_to_hi"]["min_s"] == pytest.approx(200e-9) and got["overlaps"]["count"] == 0


def test_rmt_two_pads_together():
    # every edge of the two pads on the same sample at 40 Msps; rmt_a has one more rise near the end
    ok, got, why = _judge("rmt-two-pads-80M", R.skew("rmt_a", "rmt_b", 30e-9))
    assert got["max_s"] == got["min_s"] == 0 and (got["unpaired_a"], got["unpaired_b"]) == (1, 0)
    assert ok is False and "without a partner" in why


@pytest.mark.parametrize("mode", [0, 1, 2, 3])
def test_spi_controller_modes(mode):
    ok, got, why = _judge(f"spi-controller-mode{mode}-1000k", R.spi("sck", "mosi", None, "cs", mode=mode,
                                                                    mosi_bytes="a53c0f81"))
    assert ok is True and (got["cpol"], got["cpha"]) == (mode >> 1, mode & 1), why
    # asked for 1 MHz, the fixture's SCK runs at about 620 kHz
    assert 0.6e6 < got["sck_hz"] < 0.64e6


def test_i2c_controller_against_um10204():
    ok, got, why = _judge("i2c-controller-100k-nack", R.i2c_timing("scl", "sda", "standard"))
    assert ok is True, why
    # the 400 kHz setting: SCL at about 290 kHz, yet a 1 us low and data valid 1.7 us after SCL falls
    ok, got, why = _judge("i2c-controller-400k-nack", R.i2c_timing("scl", "sda", "fast"))
    assert ok is False and why.startswith("t_low 1 us < 1.3 us") and "t_vd_dat 1.7 us > 900 ns" in why, why
    for f in ("100k", "400k"):
        assert _judge(f"i2c-controller-{f}-nack", R.i2c("scl", "sda", [{"addr": 0x50, "rw": "write",
                                                                         "ack": False}]))[0] is True


def test_ws2812_frames_and_a_pulse_after():
    ok, got, why = _judge("ws2812-3px", R.rgb_led("din", ["123456a55aff008001"] * 2))
    assert got["frames"][:2] == ["123456a55aff008001"] * 2
    assert got["t0h"]["min_s"] == pytest.approx(400e-9) and got["period"]["max_s"] == pytest.approx(1.3e-6)
    # a 45.85 us high 89 us after the second frame: a third frame of one bit
    assert ok is False and got["bits"][2] == 1 and "T1H 45.85 us" in why


@pytest.mark.parametrize("pin, carrier", [("ir_carrier", 38e3), ("ir_envelope", None)])
def test_nec_with_and_without_the_carrier(pin, carrier):
    ok, got, why = _judge("nec-38k-repeat", R.ir(pin, [{"addr": 0, "cmd": 0x45}, {"repeat": True}],
                                                 carrier_hz=carrier))
    assert ok is True, why
    if carrier:
        assert got["carrier_duty"] == pytest.approx(1 / 3, abs=0.02)


def test_onewire_reset_at_its_limit():
    ok, got, why = _judge("onewire-reset-cc44", R.onewire("dq", "cc44", presence=False))
    # a 480 us reset is the limit itself: within a sample of it, not decided
    assert got["data"] == ["cc44"] and ok is None and why == "reset 480 us near 480 us at 500 us"
