"""USB low and full speed on D+ / D-, read as `usb`: tokens with their CRC5, data with its CRC16, handshakes, SOFs,
bit stuffing; the speed and which line is D+ told by the idle state and the bit time. An encoder written here (the
CRCs as registers of bits, NRZI and stuffing from the bits), apart from the reader."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

PID = {"OUT": 0x1, "IN": 0x9, "SOF": 0x5, "SETUP": 0xD, "DATA0": 0x3, "DATA1": 0xB, "ACK": 0x2, "NAK": 0xA}


def lsb(v, n):
    return [(v >> i) & 1 for i in range(n)]


def crc_bits(bits, width, poly, init):
    """The CRC as sent (MSB first), the register's complement."""
    reg = init
    top = 1 << (width - 1)
    for b in bits:
        fb = ((reg & top) >> (width - 1)) ^ b
        reg = (reg << 1) & ((1 << width) - 1)
        if fb:
            reg ^= poly
    reg ^= (1 << width) - 1
    return [(reg >> (width - 1 - i)) & 1 for i in range(width)]


def packet_bits(pkt):
    pid = PID[pkt["pid"]]
    bits = [0] * 7 + [1] + lsb(pid, 4) + lsb(pid ^ 0xF, 4)
    if "addr" in pkt:
        body = lsb(pkt["addr"], 7) + lsb(pkt["endp"], 4)
        bits += body + crc_bits(body, 5, 0b00101, 0x1F)
    elif "frame" in pkt:
        body = lsb(pkt["frame"], 11)
        bits += body + crc_bits(body, 5, 0b00101, 0x1F)
    elif "data" in pkt:
        body = [b for x in pkt["data"] for b in lsb(x, 8)]
        bits += body + crc_bits(body, 16, 0x8005, 0xFFFF)
    return bits


def line(packets, speed, rate):
    """D+ and D- with the packets, NRZI and stuffed, each ending in an SE0 of two bits; idle J between."""
    fs = speed == "full"
    bt = 1 / (12e6 if fs else 1.5e6)
    j = (1, 0) if fs else (0, 1)
    dp, dm = gen.Wave(j[0]), gen.Wave(j[1])
    t, lvl = 20e-6, j
    for p in packets:
        raw, ones = [], 0
        for b in packet_bits(p):
            raw.append(b)
            ones = ones + 1 if b else 0
            if ones == 6:
                raw.append(0)
                ones = 0
        for b in raw:
            if b == 0:
                lvl = (1 - lvl[0], 1 - lvl[1])
            dp.set(t, lvl[0])
            dm.set(t, lvl[1])
            t += bt
        dp.set(t, 0)
        dm.set(t, 0)
        t += 2 * bt
        lvl = j
        dp.set(t, j[0])
        dm.set(t, j[1])
        t += 40 * bt
    return gen.quantize({"DP": dp, "DM": dm}, rate, t + 20e-6, np.random.default_rng(1))


@pytest.mark.parametrize("speed,rate", [("full", 48e6), ("low", 12e6)])
def test_transactions(speed, rate):
    rng = np.random.default_rng(3)
    pk = []
    for k in range(6):
        pk.append({"pid": "SOF", "frame": 100 + k})
        pk.append({"pid": "IN", "addr": 5, "endp": 1})
        data = bytes(rng.integers(0, 256, 8).tolist())
        if k == 2:
            data = b"\xff" * 8                       # stuffed bits
        pk.append({"pid": "DATA1" if k % 2 else "DATA0", "data": data})
        pk.append({"pid": "ACK"})
    cap = line(pk, speed, rate)
    ex = decode.export(decode.analyze(cap), cap, depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "usb" and c["roles"] == {"dp": "DP", "dm": "DM"} and c["params"]["speed"] == speed
    got = [{k: v for k, v in p.items() if k not in ("s", "e", "ok")} for p in c["result"]["packets"]]
    want = [{**p, **({"data": p["data"].hex()} if "data" in p else {})} for p in pk]
    assert got == want and all(p["ok"] for p in c["result"]["packets"])


def test_setup_transaction():
    """A control transfer's SETUP stage as a transaction with its request named (GET_DESCRIPTOR of the device)."""
    pk = [{"pid": "SETUP", "addr": 0, "endp": 0}, {"pid": "DATA0", "data": bytes.fromhex("8006000100004000")},
          {"pid": "ACK"}, {"pid": "IN", "addr": 0, "endp": 0}, {"pid": "NAK"}] * 3
    cap = line(pk, "low", 12e6)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["protocol"] == "usb"]
    tr = c["result"]["transactions"]
    assert [(x["token"], x.get("handshake")) for x in tr] == [("SETUP", "ACK"), ("IN", "NAK")] * 3
    st = tr[0]["setup"]
    assert (st["request"], st["direction"], st["wValue"], st["wLength"]) == ("GET_DESCRIPTOR", "in", 0x0100, 64)
