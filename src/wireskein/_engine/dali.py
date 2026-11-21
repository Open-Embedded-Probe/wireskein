"""DALI (IEC 62386, lighting control): one line idling high, Manchester at 1200 b/s (a bit is two halves of 417 us,
low then high for 1, high then low for 0), a start bit (1), then 16 bits from the controller (a forward frame: the
address byte and the command or arc power byte) or 8 from a gear (a backward frame: the answer), MSB first, then
the line idles (two stop bits and more). The address byte: 0AAAAAAS a short address, 100AAAAS a group, 1111111S
broadcast; S 1: a command follows, 0: a direct arc power level."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

DALI_HALF = 1 / 2400       # a half bit, seconds
DALI_TOL = 0.25            # runs within this share of a whole number of halves
EVIDENCE_BITS = 8
DALI_GOOD = 0.8           # frames of valid Manchester and length, this share of all at least


def _address(a: int) -> dict:
    s = a & 1
    if a >> 7 == 0:
        out = {"to": "short", "address": (a >> 1) & 0x3F}
    elif a >> 5 == 0b100:
        out = {"to": "group", "address": (a >> 1) & 0xF}
    elif a >> 1 == 0x7F:
        out = {"to": "broadcast"}
    else:
        out = {"to": "special"}
    out["selector"] = "command" if s else "arc_power"
    return out


def frames(ch, n: int, rate: float) -> list[dict]:
    st, length, level = ch.runs(n)
    half = DALI_HALF * rate
    k = length / half
    out, i = [], 1
    while i < len(st) - 1:
        # a frame starts where the idle high falls
        if level[i] != 0 or level[i - 1] != 1:
            i += 1
            continue
        halves, j, ok = [], i, True
        while j < len(st) - 1:
            m = int(round(k[j]))
            if m in (1, 2) and abs(k[j] - m) <= DALI_TOL:
                halves += [int(level[j])] * m
                j += 1
            else:
                break
        # the line back at idle (high) after the last bit: a trailing high half belongs to it
        if j < len(st) and level[j] == 1 and len(halves) % 2 == 1:
            halves.append(1)
        bits = []
        for q in range(0, len(halves) - 1, 2):
            pair = (halves[q], halves[q + 1])
            if pair == (0, 1):
                bits.append(1)
            elif pair == (1, 0):
                bits.append(0)
            else:
                ok = False
                break
        s0, e0 = int(st[i]), int(st[j]) if j < len(st) else n
        i = max(j, i + 1)
        if not ok or not bits or bits[0] != 1 or len(bits) - 1 not in (8, 16):
            if len(bits) > 2:
                out.append({"s": s0, "e": e0, "ok": False, "bits": len(bits) - 1})
            continue
        data = int("".join(map(str, bits[1:])), 2)
        if len(bits) - 1 == 16:
            a, cmd = data >> 8, data & 0xFF
            out.append({"s": s0, "e": e0, "kind": "forward", "addr_byte": a, "data": cmd, **_address(a), "ok": True})
        else:
            out.append({"s": s0, "e": e0, "kind": "backward", "data": data, "ok": True})
    return out


class DaliPlugin:
    """DALI on one line."""
    name = "dali"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 10e3:
            return []
        fr = frames(ch, c.cap.n_samples, rate)
        good = [f for f in fr if f.get("ok")]
        if len(good) < 2 or len(good) < DALI_GOOD * len(fr):
            return []
        # evidence in bits: each frame's halves all valid Manchester pairs at DALI's own rate, and its length 8 or 16
        info = sum(4 + (2 if f["kind"] == "forward" else 1) for f in good)
        score = (len(good) / len(fr)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("dali", {}, {"dali": ch.name}, Stream("dali.frames", fr),
                     {"frames": len(fr), "ok": len(good)}, layer_score=score, total=score)]
