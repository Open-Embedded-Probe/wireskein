"""SENT (SAE J2716, a sensor's single edge nibble transmission): the time between falling edges carries the values. A
message: a sync pulse of 56 clock ticks (the tick, 3 to 90 us, from it), a status nibble, 1 to 6 data nibbles and a
CRC nibble, each 12 + its value ticks, then maybe a pause pulse. CRC4: x^4 + x^3 + x^2 + 1, seed 5, over the data
nibbles; the 2010 recommendation runs a zero nibble in after them (both accepted, the kind kept)."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

SYNC_TICKS = 56
TICK_S = (2.5e-6, 100e-6)
NIBBLE = (11.5, 27.5)          # a nibble's ticks (12 to 27)
EVIDENCE_BITS = 8
SENT_TICK_TOL = 0.05       # a message's tick within this share of the median: the same sensor's
SENT_GOOD = 0.8            # messages of one tick and length whose CRC holds, this share of all at least


def _table():
    t = []
    for i in range(16):
        v = i << 4
        for b in (7, 6, 5, 4):
            if v & (1 << b):
                v ^= 0x1D << (b - 4)
        t.append(v & 0xF)
    return t


TABLE = _table()


def crc4(data: list[int], recommended: bool) -> int:
    c = 5
    for x in data:
        c = TABLE[c] ^ x
    return TABLE[c] if recommended else c


def messages(ch, n: int, rate: float) -> list[dict]:
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 20:
        return []
    lv = ch.level_at(e)
    f = e[lv == 0]
    d = np.diff(f).astype(float)
    out, i = [], 0
    while i + 3 < len(d):
        tick = d[i] / SYNC_TICKS
        nxt = d[i + 1:i + 4] / tick
        if not (TICK_S[0] * rate <= tick <= TICK_S[1] * rate) or not np.all((nxt >= NIBBLE[0]) & (nxt <= NIBBLE[1])):
            i += 1
            continue
        j, nib = i + 1, []
        while j < len(d) and NIBBLE[0] <= d[j] / tick <= NIBBLE[1] and len(nib) < 9:
            nib.append(int(round(d[j] / tick)) - 12)
            j += 1
        # the next sync (or a pause before it) ends the message
        if len(nib) < 3:
            i = j
            continue
        status, data, crc = nib[0], nib[1:-1], nib[-1]
        kind = "2010" if crc4(data, True) == crc else ("legacy" if crc4(data, False) == crc else None)
        out.append({"s": int(f[i]), "e": int(f[j]), "tick_s": tick / rate, "status": status, "data": data,
                    "crc": crc, "ok": kind is not None, **({"crc_kind": kind} if kind else {})})
        i = j
    return out


class SentPlugin:
    """SENT on one line."""
    name = "sent"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 1e6:
            return []
        msgs = messages(ch, c.cap.n_samples, rate)
        if len(msgs) < 3:
            return []
        # a sensor's messages share its tick and its count of data nibbles: those that do are its, the rest (a line
        # of other pulses read where some 56 ticks happen) are not; a CRC4 holds by chance one time in 16
        tick = float(np.median([m["tick_s"] for m in msgs]))
        lens = [len(m["data"]) for m in msgs]
        mode = max(set(lens), key=lens.count)
        same = [m for m in msgs if abs(m["tick_s"] / tick - 1) < SENT_TICK_TOL and len(m["data"]) == mode]
        good = [m for m in same if m["ok"]]
        if len(good) < 3 or len(good) < SENT_GOOD * len(msgs):
            return []
        # evidence in bits: each message's CRC (4) and its sync of 56 ticks with nibbles in range (2)
        info = 6 * len(good)
        lens = [len(m["data"]) for m in good]
        score = (len(good) / len(msgs)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("sent", {"tick_s": float(np.median([m["tick_s"] for m in good])),
                              "data_nibbles": max(set(lens), key=lens.count)}, {"data": ch.name},
                     Stream("sent.messages", msgs), {"messages": len(msgs), "ok": len(good)},
                     layer_score=score, total=score)]
