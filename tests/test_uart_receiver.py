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
                                   "parity": "none", "stop": 1, "errors": None, **x})


def test_a_glitch_shorter_than_half_a_bit_is_no_character():
    for segs in ([(0, BT / 4), (1, 3 * BT)] + chars(DATA),                           # at start-up
                 chars(DATA[:5]) + [(0, 1 / RATE), (1, 5 * BT)] + chars(DATA[5:])):  # a one-sample spike
        ok, got, _ = read(segs, data=DATA.hex())
        assert ok and got["frame_errors"] == 0 and got["glitches"] == 1, got
        assert [n["kind"] for n in got["notices"]] == ["glitch"]


def test_a_break_is_not_a_framing_error():
    ok, got, _ = read([(0, 20 * BT), (1, 2 * BT)] + chars(DATA), data=DATA.hex())
    assert ok and got["breaks"] == 1 and got["frame_errors"] == 0, got
    (n,) = got["notices"]
    assert n["kind"] == "break" and abs(n["length_ticks"] - 20 * BT * RATE) <= 2


def test_after_a_framing_error_it_gets_in_step_at_the_next_idle_start():
    # one character's stop bit low at its middle, then the rest after a bit of idle: the error counts once
    segs = chars(DATA[:3]) + [(0, BT)] + [((DATA[3] >> k) & 1, BT) for k in range(8)] + [(0, 0.6 * BT), (1, 2 * BT)]   # low past the middle of the stop bit
    ok, got, _ = read(segs + chars(DATA[4:]))
    assert got["frame_errors"] == 1 and got["resync_skipped"] == 0, got
    assert got["data"] == (DATA[:3] + DATA[4:]).hex()


def frames_8x1(data, parity):
    """Characters with a parity bit ("even" / "odd"), back to back with one stop bit."""
    out = []
    for b in data:
        ones = bin(b).count("1")
        p = ones & 1 if parity == "even" else 1 - (ones & 1)
        out += [(0, BT)] + [((b >> k) & 1, BT) for k in range(8)] + [(p, BT), (1, BT)]
    return out


EVEN_ONES = bytes([0x03, 0x05, 0x06, 0x09, 0x0A, 0x0C, 0x11, 0x12, 0x14, 0x18, 0x21, 0x22, 0x24, 0x28, 0x30, 0x33])


def test_each_character_in_error_counts_when_errors_follow_one_another():
    # as a receive-errors test sends them: 8E1 of bytes with an even number of ones, read as 8N1 (the parity bit 0
    # where the stop bit should be): sixteen framing errors, one each
    _, got, _ = read(frames_8x1(EVEN_ONES, "even"))
    assert (got["chars"], got["frame_errors"], got["error_runs"]) == (16, 16, 1), got
    # 8O1 read as 8E1: sixteen parity errors
    _, got, _ = read(frames_8x1(DATA + b"abc", "odd"), parity="even")
    assert (got["parity_errors"], got["frame_errors"]) == (16, 0), got
    # 0x00 sent 8E1, read 8N1: the line low a character's worth exactly - a framing error with data 0, no break
    _, got, _ = read(frames_8x1(bytes(4), "even"))
    assert (got["frame_errors"], got["breaks"]) == (4, 0), got
    # the same at 5 MS/s with long rests around (the runs as an OEP probe's ESP-IDF UART made them, 115211 baud):
    # read the other way round, the bits between them are good characters, yet the line rests high
    runs = [(1, 591533), (0, 434), (1, 44), (0, 434), (1, 43), (0, 434), (1, 43), (0, 434), (1, 2288166)]
    edges = np.cumsum([n for _, n in runs])[:-1]
    cap = Capture(5e6, int(sum(n for _, n in runs)), [Channel("TX", 1, edges.astype(np.int64))])
    ok, got, why = check_uart(cap, {"kind": "uart", "pin": "TX", "baud": 115211, "baud_rel": 0.03, "idle": 1,
                                    "bits": 8, "parity": "none", "stop": 1, "errors": {"framing": 4}})
    assert ok is True and got["idle"] == 1, (why, got)


def test_errors_are_ng_unless_expected_exactly():
    sent = frames_8x1(EVEN_ONES, "even")                      # sixteen framing errors read as 8N1
    ok, _, why = read(sent)
    assert ok is False and "16 framing and 0 parity errors, not 0 and 0" in why
    assert read(sent, errors={"framing": 16})[0] is True
    assert read(sent, errors={"framing": 15})[0] is False
    assert read(sent, errors={"framing": 16, "parity": 1})[0] is False
    ok, _, why = read(sent, errors={"overrun": 1})
    assert ok is False and "unknown kind overrun" in why
    sent = frames_8x1(DATA + b"abc", "odd")                   # sixteen parity errors read as 8E1
    assert read(sent, parity="even", errors={"parity": 16})[0] is True
    assert read(sent, parity="even")[0] is False
    # a break and a glitch are notices, no errors
    assert read([(0, 20 * BT), (1, 2 * BT)] + chars(DATA))[0] is True
    # measuring only judges no errors unless they are expected
    assert read(frames_8x1(EVEN_ONES, "even"), baud=None)[0] == "measured"
    assert read(frames_8x1(EVEN_ONES, "even"), baud=None, errors={"framing": 16})[0] is True


# ---- the truth from hardware: an OEP probe's ESP-IDF UART driver, its TX captured (tests/data/uart_truth) ----

TRUTH = __import__("pathlib").Path(__file__).parent / "data" / "uart_truth"


def _truth(name, **x):
    import json
    from wireskein._engine import fileio
    (cap,) = fileio.load_runs(TRUTH / f"{name}.wireskein")
    t = json.loads((TRUTH / f"{name}.truth.json").read_text())
    assert t["interface"] == "oep.fixture.uart (ESP-IDF UART driver)"
    ok, got, why = check_uart(cap, {"kind": "uart", "pin": "tx", "baud": 115211, "baud_rel": 0.03, "idle": 1, "bits": 8,
                                    "parity": "none", "stop": 1, "errors": None, **x})
    got["ok"] = ok
    return got


def test_hardware_framing_errors_and_a_characters_worth_of_low():
    # 41 42 in 8N1, then 03 00 in 8E1 (read as 8N1: stop bits 0; 00 a character's worth of low), then 43 44
    got = _truth("uart_A_framing_break")
    assert (got["chars"], got["frame_errors"], got["breaks"], got["data"]) == (6, 2, 0, "41424344"), got
    assert got["ok"] is False
    assert _truth("uart_A_framing_break", errors={"framing": 2}, data="41424344")["ok"] is True


def test_hardware_parity_errors():
    # 61 62 in 8E1, 78 in 8O1, 79 7a in 8E1
    got = _truth("uart_B_parity", parity="even")
    assert (got["parity_errors"], got["frame_errors"], got["data"]) == (1, 0, "6162797a"), got
    assert _truth("uart_B_parity", parity="even", errors={"parity": 1})["ok"] is True
    got = _truth("uart_B_parity", parity="odd")
    assert (got["parity_errors"], got["data"]) == (4, "78"), got


def test_hardware_bit_rates_off_by_up_to_three_percent():
    import json
    import numpy as np
    from wireskein._engine import fileio
    from wireskein.verify import _window
    (cap,) = fileio.load_runs(TRUTH / "uart_C_baud_offset.wireskein")
    used = [s["baud_used"] for s in json.loads((TRUTH / "uart_C_baud_offset.truth.json").read_text())["steps"]
            if s["step"] == "configure"]
    e = cap.channels[0].edges
    cut = np.flatnonzero(np.diff(e) > 30_000) + 1                  # 3 ms of idle between the bursts
    for (a, b), baud in zip(zip(np.concatenate(([0], cut)), np.concatenate((cut, [len(e)]))), used):
        w = _window(cap, int(e[a]) - 2000, int(e[b - 1]) + 2000)
        ok, got, _ = check_uart(w, {"kind": "uart", "pin": "tx", "baud": baud, "baud_rel": 0.001, "idle": 1, "bits": 8,
                                    "parity": "none", "stop": 1, "data": "552048656c6c6f0d0a"})
        assert ok, (baud, got)                                     # its rate within 0.1 % of what the driver made
