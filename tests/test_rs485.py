"""The rs485 check: UART on TX with the driver enable (DE) around each block of characters. The lines are made by
an encoder written here, apart from the reader."""

import numpy as np

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import rs485
from wireskein.verify import check_rs485

RATE, BAUD = 10e6, 115200
BT = 1 / BAUD


def char_segs(data):
    out = []
    for b in data:
        out += [(0, BT)] + [((b >> k) & 1, BT) for k in range(8)] + [(1, BT)]
    return out


def edges_of(segs, initial):
    t, edges, lvl = 0.0, [], initial
    for level, d in segs:
        if level != lvl:
            edges.append(int(round(t * RATE)))
            lvl = level
        t += d
    return np.array(edges, np.int64), t


def bus(blocks, de_active=1, idle_s=300e-6):
    """blocks: (data, lead_s, tail_s) each, DE active from lead_s before the first start bit to tail_s after the last
    stop bit's end (a negative tail: released inside it)."""
    tx, de_edges, t = [(1, idle_s)], [], idle_s
    for data, lead, tail in blocks:
        segs = char_segs(data)
        dur = sum(d for _, d in segs)
        de_edges += [t, t + lead + dur + tail]
        tx.append((1, lead))
        tx += segs
        tx.append((1, max(tail, 0) + idle_s))
        t += lead + dur + max(tail, 0) + idle_s
    e_tx, end = edges_of(tx, 1)
    e_de = np.array([int(round(x * RATE)) for x in de_edges], np.int64)
    return Capture(RATE, int(round(end * RATE)), [Channel("TX", 1, e_tx), Channel("DE", 1 - de_active, e_de)])


DATA = b"\x01\x03\x00\x00\x00\x02\xc4\x0b"


def test_de_around_each_block():
    cap = bus([(DATA, 20e-6, 5e-6), (b"ok", 2e-6, 0.5e-6)])
    ok, got, why = check_rs485(cap, rs485("TX", "DE", BAUD, data=(DATA + b"ok").hex(), lead_min_s=1e-6, tail_max_s=10e-6))
    assert ok is True, (why, got)
    w = got["de_windows"]
    assert [x["chars"] for x in w] == [8, 2]
    assert abs(w[0]["lead_s"] - 20e-6) < 0.3e-6 and abs(w[1]["tail_s"] - 0.5e-6) < 0.3e-6, w


def test_de_released_inside_the_last_stop_bit_is_ng():
    cap = bus([(DATA, 5e-6, -0.5 * BT)])
    ok, got, why = check_rs485(cap, rs485("TX", "DE", BAUD))
    assert ok is False and "DE tail" in why, (why, got)


def test_de_released_late_and_a_short_lead():
    cap = bus([(DATA, 0.2e-6, 50e-6)])
    ok, _, why = check_rs485(cap, rs485("TX", "DE", BAUD, lead_min_s=1e-6, tail_max_s=10e-6))
    assert ok is False and "DE lead" in why and "DE tail" in why, why


def test_characters_with_de_inactive_are_ng():
    cap = bus([(DATA, 5e-6, 2e-6)])
    tx, de = cap.channels
    cap = Capture(cap.rate, cap.n_samples, [tx, Channel("DE", 0, np.zeros(0, np.int64))])
    ok, got, why = check_rs485(cap, rs485("TX", "DE", BAUD))
    assert ok is False and got["chars_outside_de"] == 8 and "DE inactive" in why, (why, got)


def test_de_active_low_and_an_empty_window():
    cap = bus([(DATA, 5e-6, 2e-6), (b"", 5e-6, 2e-6)], de_active=0)
    ok, got, why = check_rs485(cap, rs485("TX", "DE", BAUD, de_active=0, data=DATA.hex()))
    assert ok is True and [n["kind"] for n in got["notices"]] == ["empty_de_window"], (why, got)


def test_uart_errors_are_ng_as_in_uart():
    cap = bus([(DATA, 5e-6, 2e-6)])
    ok, _, why = check_rs485(cap, rs485("TX", "DE", BAUD, parity="even"))
    assert ok is False and ("parity" in why or "framing" in why), why


def test_hardware_esp_idf_rs485_half_duplex():
    # an OEP probe's ESP-IDF UART in UART_MODE_RS485_HALF_DUPLEX: RTS is DE, set as the bytes go into the FIFO and
    # put back by the TX_DONE interrupt (tests/data/rs485_truth)
    import json
    from pathlib import Path
    from wireskein._engine import fileio
    d = Path(__file__).parent / "data" / "rs485_truth"
    (cap,) = fileio.load_runs(d / "rs485_A_blocks.wireskein")
    t = json.loads((d / "rs485_A_blocks.truth.json").read_text())
    sent = "".join(s["bytes"] for s in t["steps"] if s["step"] == "write")
    ok, got, why = check_rs485(cap, rs485("tx", "de", t["driver"]["baud_used"], data=sent, baud_rel=0.001,
                                         lead_min_s=1e-6, tail_max_s=100e-6))
    assert ok is True, (why, got)
    assert [w["chars"] for w in got["de_windows"]] == [1, 8, 64]
    assert all(w["tail_s"] > 1e-6 for w in got["de_windows"])        # the interrupt's latency, never inside the stop bit
