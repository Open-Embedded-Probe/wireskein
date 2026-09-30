"""The UART check on synthetic lines: 16-byte blocks with 20 idle bits between
them, the window opening and closing inside characters."""

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import uart
from wireskein.verify import check_uart

PAYLOAD = bytes(range(0x30, 0x40))


def line(rate, baud, payload=PAYLOAD, bits=8, parity="none", stop=1, idle=1, gap_bits=20, rise_delay=0.0, jitter=0.0,
         n=65536, seed=0):
    rng = np.random.default_rng(seed)
    u = rate / baud
    t, lv, cur = [], [], 1
    x = -0.37 * 16 * (bits + 3) * u          # start before the window, so the window opens inside a block

    def put(level, dur):
        nonlocal cur, x
        if level != cur:
            t.append(x + (rise_delay if level == 1 else 0) + rng.normal(0, jitter) if jitter else x + (rise_delay if level == 1 else 0))
            lv.append(level)
            cur = level
        x += dur

    while x < n + 100 * u:
        for byte in payload:
            b = [(byte >> i) & 1 for i in range(bits)]
            put(0, u)
            for v in b:
                put(v, u)
            if parity != "none":
                p = sum(b) % 2
                put(p if parity == "even" else 1 - p, u)
            put(1, stop * u)
        put(1, gap_bits * u)
    t, lv = np.array(t), np.array(lv)
    before = t < 0
    init = int(lv[before][-1]) if before.any() else 1
    inside = (t >= 0) & (t < n)
    e = np.ceil(t[inside]).astype(np.int64)
    if idle == 0:
        init = 1 - init
    return Capture(float(rate), n, [Channel("TX", init, e)])


def fmt(kw):
    return {k: kw[k] for k in ("bits", "parity", "stop", "idle") if k in kw}


@pytest.mark.parametrize("rate,actual,kw", [
    (20e6, 921600 * 0.965, {}),                   # 8 MHz / BRR 9
    (20e6, 2.5e6 * 1.02, {}),                     # 8 samples per bit
    (2e6, 250000, {"rise_delay": 1.5}),           # rise and fall delays differ
    (20e6, 1e6, {"parity": "even"}),
    (20e6, 1e6, {"parity": "odd", "stop": 2, "idle": 0}),
    (20e6, 1e6, {"parity": "even", "bits": 7}),
    (2e6, 9600, {}),                              # 1.5 blocks in the window
    (20e6, 2.5e6, {"jitter": 0.5}),
])
def test_rate_is_measured_and_cut_characters_are_no_errors(rate, actual, kw):
    payload = bytes(b & ((1 << kw.get("bits", 8)) - 1) for b in PAYLOAD)
    ok, got, why = check_uart(line(rate, actual, payload, **kw), uart("TX", actual, tol_baud=0.015, max_errors=0, **fmt(kw)))
    assert ok, why
    assert abs(got["baud"] / actual - 1) < 1e-4
    assert got["frame_errors"] == got["parity_errors"] == 0
    assert got["cut_at_end"]                     # the window ends inside a character: not an error
    assert got["data"].startswith(payload.hex()[:12])


def test_nominal_rate_off_by_2_5_percent():
    ok, got, why = check_uart(line(2e6, 115200 * 0.975), uart("TX", 115200, tol_baud=0.015))
    assert not ok and abs(got["baud_error"] + 0.025) < 1e-4 and "-2.50% off" in why


@pytest.mark.parametrize("kw,x,reason", [
    ({"parity": "even"}, uart("TX", 1e6, max_errors=0), "framing"),
    ({}, uart("TX", 1e6, stop=2, max_errors=0), "framing"),
    ({}, uart("TX", 1e6, idle=0), "idle level 1, not 0"),
])
def test_wrong_expectations_fail(kw, x, reason):
    ok, got, why = check_uart(line(20e6, 1e6, **kw), x)
    assert ok is False and reason in why
    assert "time base" not in why          # the edges sit on the bit grid


def test_back_to_back_stream_without_an_idle_gap():
    ok, got, why = check_uart(line(20e6, 1e6, gap_bits=0), uart("TX", 1e6, tol_baud=0.015, max_errors=0))
    assert ok, why
    assert got["frame_errors"] == 0 and got["chars"] > 300 and got["idle"] == 1


def test_short_idle_before_back_to_back_bytes_at_a_low_rate():
    """The bench case: 733 baud 8N1 at 1 MHz, 2 idle bits, then bytes back to
    back. The longest run is a low data run (up to 9 bits), not the idle."""
    rate, baud = 1e6, 733
    u = rate / baud
    payload = bytes([0x00, 0x80, 0x01, 0x55, 0xAA, 0x10] * 3)   # long low runs in the data
    lv = [1] * round(2 * u)
    for b in payload:
        lv += [0] * round(u) + [(b >> i) & 1 for i in range(8) for _ in range(round(u))] + [1] * round(u)
    lv += [1] * round(3 * u)                                    # the window closes after the last stop bit
    lv = np.array(lv, np.uint8)
    init, edges = int(lv[0]), np.flatnonzero(np.diff(lv)) + 1
    cap = Capture(rate, len(lv), [Channel("TX", init, edges)])
    ok, got, why = check_uart(cap, uart("TX", baud, tol_baud=0.015, max_errors=0))
    assert ok, why
    assert got["idle"] == 1 and got["chars"] == len(payload) and got["data"] == payload.hex()


def test_rate_far_off_is_still_measured():
    ok, got, why = check_uart(line(20e6, 1e6), uart("TX", 1.1e6, max_errors=0))
    assert not ok and abs(got["baud"] / 1e6 - 1) < 1e-4


def test_measure_only():
    ok, got, why = check_uart(line(20e6, 921600 * 0.965), uart("TX", None))
    assert ok == "measured" and abs(got["baud"] / (921600 * 0.965) - 1) < 1e-4 and got["baud_error"] is None
    assert why.startswith("measured ")
