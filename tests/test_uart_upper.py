"""The upper layers of a UART: Modbus RTU (`modbus_rtu`): messages split on silence and checked by their CRC-16. A
request and its answer on one line (RS-485's bus), the answer after less than the 3.5 characters the standard asks
for, a byte of junk as the driver turns off, the line undriven (low) between transmissions. The CRC is computed
here (a register of bits, apart from the reader's), the waveforms made here. And NMEA's sentences (`nmea`) as the
final reading."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

BAUD = 19200


def crc16(data: bytes) -> bytes:
    reg = 0xFFFF
    for b in data:
        for k in range(8):
            bit = ((b >> k) & 1) ^ (reg & 1)
            reg >>= 1
            if bit:
                reg ^= 0xA001
    return reg.to_bytes(2, "little")


def message(rng, addr, func, n):
    body = bytes([addr, func]) + bytes(rng.integers(0, 256, n).tolist())
    return body + crc16(body)


def put(w, t, data):
    """8E1 characters, idle high, from t: start 0, data LSB first, even parity, stop 1. Returns the end."""
    bt = 1 / BAUD
    for b in data:
        bits = [0] + [(b >> k) & 1 for k in range(8)]
        bits += [sum(bits[1:]) & 1, 1]
        for x in bits:
            w.set(t, x)
            t += bt
    return t


def bus(rng, pairs, answer_after, junk=False, undriven_low=False):
    """Requests and answers on one line, `answer_after` characters of silence between a request and its answer;
    with undriven_low the line is low except while a node drives it (a mark of 3 characters before each message)."""
    char = 11 / BAUD                                         # 8E1: start, 8 data, parity, stop
    w = gen.Wave(0 if undriven_low else 1)
    t = 200e-6
    for req, ans in pairs:
        for k, msg in enumerate((req, ans)):
            if undriven_low:
                w.set(t, 1)                                  # the driver on: a mark
                t += 3 * char
            t = put(w, t, msg)
            if junk and k == 1:
                t = put(w, t + 0.1 * char, b"\xfe")
            if undriven_low:
                w.set(t + 0.2 * char, 0)                     # the driver off: the bus floats low
            t += (answer_after if k == 0 else 20) * char
    return w, t


def _messages(w, t):
    cap = gen.quantize({"BUS": w}, 2e6, t + 1e-3, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["protocol"] == "uart" and c["verdict"] in ("confirmed", "likely")]
    return c, c["result"].get("modbus_rtu")


@pytest.mark.parametrize("answer_after,junk,undriven", [(5.0, False, False), (2.8, False, False), (2.8, True, False),
                                                        (2.8, True, True)])
def test_requests_and_answers_on_one_line(answer_after, junk, undriven):
    rng = np.random.default_rng(int(answer_after * 10) + junk + 2 * undriven)
    pairs = [(message(rng, 1, 3, 4), message(rng, 1, 3, 1 + 2 * int(rng.integers(1, 8)))) for _ in range(12)]
    w, t = bus(rng, pairs, answer_after, junk, undriven)
    c, msgs = _messages(w, t)
    assert c["params"]["idle"] == 1                          # the level the line rests at between characters
    assert msgs == [m.hex() for pair in pairs for m in pair]


def test_nmea_sentences_are_the_final_reading():
    """A GPS's sentences, every checksum holding: the final reading is `nmea` (the checked layer), not text lines."""
    w = gen.Wave(1)
    t, want = 100e-6, []
    for k in range(8):
        body = f"GPGGA,{120000 + k:06d},4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,"
        cs = 0
        for ch in body.encode():
            cs ^= ch
        t = gen.uart(w, t, f"${body}*{cs:02X}\r\n".encode(), 9600) + 2e-3
        want.append(body)
    cap = gen.quantize({"TX": w}, 1e6, t, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["protocol"] == "uart"]
    assert c["result"] == {"nmea": want}
