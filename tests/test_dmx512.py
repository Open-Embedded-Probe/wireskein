"""DMX512 at its standard's 250 kbaud: slots of one value (all 0) give the line no bit time of its own (a train of
like pulses, a "clock" to the survey), a short capture holds one packet, and RS-485's two lines change together
(no clock and its data). Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

RATE = 1e6          # 4 samples a bit, as the logic analyzers of sigrok-dumps' DMX512 captures


def _claims(waves, end):
    cap = gen.quantize(waves, RATE, end + 50e-6, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    return ex["claims"]


def _pair(packets, mark_after=60e-6):
    plus = gen.Wave(1)
    t = 20e-6
    for slots in packets:
        t = gen.dmx(plus, t, [slots])
        t += mark_after
    minus = gen.Wave(0, times=list(plus.times))
    minus.level = plus.level ^ 1
    return {"DMX+": plus, "DMX-": minus}, t


def test_slots_of_zero_on_a_pair():
    waves, end = _pair([bytes(64)] * 4)
    cl = _claims(waves, end)
    assert {c["protocol"] for c in cl} == {"dmx512"}
    for c in cl:
        assert c["verdict"] == "confirmed"
        assert [p["data"] for p in c["result"]["packets"]] == ["00" * 64] * 4


def test_one_packet_alone():
    slots = bytes(range(100))
    w = gen.Wave(1)
    t = gen.dmx(w, 30e-6, [slots])
    (c,) = [c for c in _claims({"DMX": w}, t) if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "dmx512"
    assert c["result"]["packets"][0]["data"] == slots.hex()


def test_uart_with_a_break_is_no_dmx():
    rng = np.random.default_rng(3)
    w = gen.Wave(1)
    t = gen.uart(w, 20e-6, bytes(rng.integers(0, 256, 40).tolist()), 115200)
    w.set(t + 20e-6, 0)
    w.set(t + 400e-6, 1)
    t = gen.uart(w, t + 450e-6, bytes(rng.integers(0, 256, 40).tolist()), 115200)
    cl = _claims({"TX": w}, t)
    assert not any(c["protocol"] == "dmx512" for c in cl)
    assert any(c["protocol"] == "uart" and c["verdict"] == "confirmed" for c in cl)


def test_clock_bursts_between_lows_are_no_dmx():
    """Bursts of a square wave (7 bits a half period at 250 kbaud) with long lows between: read at the shortest
    length the runs fit (6 data bits) every burst is a packet of slots of 0 with a start code of 0. A slot is 11
    bits: so read, the lows hold 0xC0 and no packet starts with 0."""
    rng = np.random.default_rng(4)
    w = gen.Wave(1)
    t, half = 20e-6, 7 * 4e-6 * 0.98
    for _ in range(10):
        for _ in range(int(rng.integers(20, 60))):
            w.set(t, 0)
            w.set(t + half, 1)
            t += 2 * half
        w.set(t, 0)
        t += float(rng.uniform(2e-3, 7e-3))
        w.set(t, 1)
        t += half
    assert not any(c["protocol"] == "dmx512" for c in _claims({"CLK": w}, t))


def test_packets_of_a_slot_are_no_dmx():
    """A start code and one slot after each break, a millisecond apart (a USB line's keep-alives between idle gaps
    read so): no universe of channels, no DMX512."""
    w = gen.Wave(1)
    t = 30e-6
    for _ in range(20):
        t = gen.dmx(w, t, [bytes([0xFF])])
        t += 1e-3
    assert not any(c["protocol"] == "dmx512" for c in _claims({"DM": w}, t))


def test_square_bursts_of_nine_bits_are_no_dmx():
    """A clock's bursts at 13.9 kHz between long lows: half a period is 9 bits at 250 kbaud, so its lows read as
    slots of 0 (start and eight 0s) with a mark between: a square wave, not DMX512's 9 low and 2 high."""
    w = gen.Wave(1)
    t, half = 30e-6, 0.5 / 13.9e3
    for k in range(12):
        w.set(t, 0)
        t += 200e-6 + 37e-6 * k                       # a break
        w.set(t, 1)
        t += half
        for _ in range(40):
            w.set(t, 0)
            w.set(t + half, 1)
            t += 2 * half
    assert not any(c["protocol"] == "dmx512" for c in _claims({"CLK": w}, t + 100e-6))
