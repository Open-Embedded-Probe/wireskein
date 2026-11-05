"""The uart check reads as a receiver does: a pulse that is not low at the middle of a start bit is a glitch (no
character, the reading stays in step), a line held active through a whole character is a break (not a framing
error), and after a framing error it gets in step again at the next start after a bit of idle. The lines are made
by an encoder written here, apart from the receiver."""

import numpy as np

from wireskein.verify import check_uart
from wireskein._engine.model import Capture, Channel

RATE, BAUD = 10e6, 115200
BT = 1 / BAUD
DATA = b"Hello, UART\r\n"


def chars(data, gap_bits=1.0):
    out = []
    for b in data:
        out += [(0, BT)] + [((b >> k) & 1, BT) for k in range(8)] + [(1, BT * (1 + gap_bits))]
    return out


def line(segs, lead=200e-6, tail=200e-6):
    t, edges, lvl = lead, [], 1
    for level, d in segs:
        if level != lvl:
            edges.append(int(round(t * RATE)))
            lvl = level
        t += d
    if lvl != 1:
        edges.append(int(round(t * RATE)))
    return Capture(RATE, int(round((t + tail) * RATE)), [Channel("TX", 1, np.array(edges, np.int64))])


def read(segs, **x):
    return check_uart(line(segs), {"kind": "uart", "pin": "TX", "baud": BAUD, "baud_rel": 0.03, "idle": 1, "bits": 8,
                                   "parity": "none", "stop": 1, "errors_max": None, **x})


def test_a_glitch_shorter_than_half_a_bit_is_no_character():
    for segs in ([(0, BT / 4), (1, 3 * BT)] + chars(DATA),                           # at start-up
                 chars(DATA[:5]) + [(0, 1 / RATE), (1, 5 * BT)] + chars(DATA[5:])):  # a one-sample spike
        ok, got, _ = read(segs, data=DATA.hex(), errors_max=0)
        assert ok and got["frame_errors"] == 0 and got["glitches"] == 1, got
        assert [n["kind"] for n in got["notices"]] == ["glitch"]


def test_a_break_is_not_a_framing_error():
    ok, got, _ = read([(0, 20 * BT), (1, 2 * BT)] + chars(DATA), data=DATA.hex(), errors_max=0)
    assert ok and got["breaks"] == 1 and got["frame_errors"] == 0, got
    (n,) = got["notices"]
    assert n["kind"] == "break" and abs(n["length_ticks"] - 20 * BT * RATE) <= 2


def test_after_a_framing_error_it_gets_in_step_at_the_next_idle_start():
    # one character's stop bit low at its middle, then the rest after a bit of idle: the error counts once
    segs = chars(DATA[:3]) + [(0, BT)] + [((DATA[3] >> k) & 1, BT) for k in range(8)] + [(0, 0.6 * BT), (1, 2 * BT)]   # low past the middle of the stop bit
    ok, got, _ = read(segs + chars(DATA[4:]))
    assert got["frame_errors"] == 1 and got["resync_skipped"] == 0, got
    assert got["data"] == (DATA[:3] + DATA[4:]).hex()
