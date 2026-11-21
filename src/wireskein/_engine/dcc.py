"""DCC (NMRA S-9.1 / S-9.2, model railway control) on the track signal: each bit a full cycle, two halves of equal
length: 1 is 52-64 us a half, 0 is 90 us or more; a packet is a preamble of at least 10 ones, then bytes each led by a
0, the last byte the XOR of the others (the error detection byte), and a closing 1. The first byte: a decoder's
address (1-127 short; 192-231 starts a long address of 14 bits; 0 broadcast; 255 idle), then the instruction."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

ONE_HALF = (52e-6, 64e-6)
ZERO_HALF = (90e-6, 10e-3)
PREAMBLE = 10
EVIDENCE_BITS = 8


def bits_of(ch, n: int, rate: float) -> tuple[list[int], list[int]]:
    """(bits, tick of each bit's start): a bit where two halves of the same kind follow; an unpaired half restarts the
    pairing (a packet's phase is not known until a 0 shows it)."""
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 40:
        return [], []
    d = np.diff(e) / rate
    kind = np.where((d >= ONE_HALF[0]) & (d <= ONE_HALF[1]), 1, np.where((d >= ZERO_HALF[0]) & (d <= ZERO_HALF[1]), 0, -1))
    bits, at = [], []
    i = 0
    while i + 1 < len(kind):
        if kind[i] >= 0 and kind[i] == kind[i + 1]:
            bits.append(int(kind[i]))
            at.append(int(e[i]))
            i += 2
        else:
            bits.append(-1)
            at.append(int(e[i]))
            i += 1
    return bits, at


def _address(b: list[int]) -> dict:
    a = b[0]
    if a == 0xFF:
        return {"address": "idle"}
    if a == 0:
        return {"address": "broadcast"}
    if 1 <= a <= 127:
        return {"address": a}
    if 192 <= a <= 231 and len(b) > 1:
        return {"address": ((a & 0x3F) << 8) | b[1], "long": True}
    return {"address": a, "accessory": 128 <= a <= 191}


def packets(ch, n: int, rate: float) -> list[dict]:
    bits, at = bits_of(ch, n, rate)
    out, i = [], 0
    while i < len(bits):
        # a preamble: ten 1s at least, then the packet start bit 0
        k = i
        while k < len(bits) and bits[k] == 1:
            k += 1
        if k - i < PREAMBLE or k >= len(bits) or bits[k] != 0:
            i = k + 1 if k == i else k
            continue
        j, data, ok = k, [], True
        while j + 9 <= len(bits) and bits[j] == 0:
            byte = bits[j + 1:j + 9]
            if -1 in byte:
                ok = False
                break
            data.append(int("".join(map(str, byte)), 2))
            j += 9
        end_ok = j < len(bits) and bits[j] == 1
        if len(data) >= 2:
            x = 0
            for v in data[:-1]:
                x ^= v
            row = {"s": at[i], "e": at[min(j, len(at) - 1)], "bytes": bytes(data).hex(),
                   "ok": ok and end_ok and x == data[-1], **_address(data)}
            out.append(row)
        i = max(j, k + 1)
    return out


class DccPlugin:
    """DCC on the track line."""
    name = "dcc"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 50e3:
            return []
        pk = packets(ch, c.cap.n_samples, rate)
        good = [p for p in pk if p["ok"]]
        if len(good) < 3:
            return []
        # evidence in bits: each packet's preamble (a run of 1s of DCC's halves) and its error detection byte (8)
        info = sum(10 for _ in good)
        score = (len(good) / len(pk)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("dcc", {}, {"track": ch.name}, Stream("dcc.packets", pk),
                     {"packets": len(pk), "ok": len(good)}, layer_score=score, total=score)]
