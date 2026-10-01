"""Captures from real ArduinoCore-CH32 (now ArduinoCore-CH32RV) runs (x035: ESP32-P4 PARLIO, v003:
classic ESP32 GPIO sampler) that once exposed a bug in the checks."""

import gzip
import json
from pathlib import Path

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel, edges_from_dense
from wireskein.verify import CHECKS, check_i2c

DATA = Path(__file__).parent / "data" / "real"
MANIFEST = {x["name"]: x for x in json.loads((DATA / "manifest.json").read_text())}


def capture(name: str):
    m = MANIFEST[name]
    v = np.frombuffer(gzip.decompress((DATA / f"{name}.bin.gz").read_bytes()), np.uint8)
    chans = [Channel(n, *edges_from_dense((v >> k) & 1)) for k, n in enumerate(m["bits"])]
    return Capture(float(m["rate"]), len(v), chans), m


def run_expectations(name):
    cap, m = capture(name)
    return [(x, *CHECKS[x["kind"]](cap, x)) for x in m["expect"]]


@pytest.mark.parametrize("name,mode,mosi,miso", [
    # one CS frame only; in mode 2 the clock goes to its idle level before CS
    ("x035-spi-mode2-one-cs-frame", 2, "a55a0f01", None),
    # 4 MHz: the bytes are separate clock bursts under one CS
    ("x035-spi-peer-4mhz-mode0", 0, "a55a0f01", "3c96c30f"),
])
def test_spi_single_cs_frame(name, mode, mosi, miso):
    for x, ok, got, why in run_expectations(name):
        assert ok, why
        if x["kind"] == "spi":
            assert got["mode"] == mode
            assert got["bytes"]["PA7"] == mosi
            if miso:
                assert got["bytes"]["PA6"] == miso


def test_i2c_transaction_cut_at_the_window_end():
    cap, _ = capture("x035-i2c-target-left-mid-byte")
    x = {"kind": "i2c", "scl": "PC16", "sda": "PC17", "hz": None, "tol_hz": 0.1, "released": True,
         "transactions": [{"addr": 0x42, "rw": "read", "bytes": [0], "complete": False, "pending_bits": [0]}]}
    ok, got, why = check_i2c(cap, x)
    assert got["transactions"] == [{"addr": 0x42, "rw": "read", "bytes": [0], "ack": True, "complete": False,
                                    "pending_bits": [0]}]
    assert ok is False and why == "bus not released at the end (SCL=1, SDA=0)"


def test_uart_error_in_a_skewed_character_points_at_the_time_base():
    (x, ok, got, why), = run_expectations("v003-uart-115200-time-base-slip")
    assert ok is False
    assert got["frame_errors"] + got["parity_errors"] == 1          # one disturbance, counted once
    assert "suspect the capture time base" in why
    assert abs(got["baud_error"]) < 0.015


def test_uart_measure_only_out_of_range_brr():
    (x, ok, got, why), = run_expectations("x035-uart-brr-below-16")
    assert ok == "measured"
    assert abs(got["baud"] / (6e6 / 16) - 1) < 0.003                 # F_CPU / 16, not the 460800 asked for
    assert got["frame_errors"] == 0


def test_manifest_matches_files():
    assert sorted(p.name.removesuffix(".bin.gz") for p in DATA.glob("*.bin.gz")) == sorted(MANIFEST)
