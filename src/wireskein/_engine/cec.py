"""HDMI CEC (one line idling high, open drain): a start bit (3.7 ms low of 4.5 ms), then blocks of 10 bits, each bit a
low pulse in 2.4 ms (0.6 ms low: 1, 1.5 ms: 0): 8 data bits MSB first, EOM (1 on the last block), ACK (the follower
pulls it to 0 for a message to it; a broadcast's 0 is a refusal). The first block is the header: the initiator's
logical address and the destination's (15: broadcast); the next, if any, the opcode."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

START_LOW = (3.5e-3, 3.9e-3)      # a start bit's low (3.7 ms nominal)
BIT_SPLIT = 1.05e-3               # a data bit's low shorter than this: 1 (0.6 ms), longer: 0 (1.5 ms)
BIT_LOW_MAX = 2.0e-3              # a data bit's low at most
LOW_1 = (0.4e-3, 0.8e-3)          # a 1's low (0.6 ms)
LOW_0 = (1.3e-3, 1.7e-3)          # a 0's low (1.5 ms)
BIT_GAP = (2.05e-3, 2.75e-3)      # a bit's start after the one before (2.4 ms)
START_GAP = (4.3e-3, 4.7e-3)      # the first bit's start after the start bit's (4.5 ms)
EVIDENCE_BITS = 8


def messages(ch, n: int, rate: float) -> list[dict]:
    st, length, level = ch.runs(n)
    sel = level == 0
    sel[0] = sel[-1] = False
    s, w = st[sel] / rate, length[sel] / rate
    out, i = [], 0
    while i < len(s):
        if not START_LOW[0] <= w[i] <= START_LOW[1]:
            i += 1
            continue
        msg = {"s": int(round(s[i] * rate)), "blocks": [], "acks": []}
        i += 1
        bits = []
        first = True
        while i < len(s):
            # each bit as CEC times it: a low of 0.6 or 1.5 ms, 2.4 ms after the bit before (4.5 after the start)
            gap = s[i] - s[i - 1]
            if not ((START_GAP if first else BIT_GAP)[0] <= gap <= (START_GAP if first else BIT_GAP)[1]):
                break
            if LOW_1[0] <= w[i] <= LOW_1[1]:
                bits.append(1)
            elif LOW_0[0] <= w[i] <= LOW_0[1]:
                bits.append(0)
            else:
                break
            first = False
            i += 1
            if len(bits) == 10:
                msg["blocks"].append(int("".join(map(str, bits[:8])), 2))
                msg["acks"].append(bits[9])
                msg["e"] = int(round((s[i - 1] + w[i - 1]) * rate))
                eom = bits[8]
                bits = []
                if eom:
                    break
        else:
            eom = 0
        if not msg["blocks"]:
            continue
        hdr = msg["blocks"][0]
        dest = hdr & 0xF
        broadcast = dest == 0xF
        row = {"s": msg["s"], "e": msg["e"], "initiator": hdr >> 4, "destination": dest}
        if len(msg["blocks"]) > 1:
            row["opcode"] = msg["blocks"][1]
            row["data"] = bytes(msg["blocks"][2:]).hex()
        # the follower's acknowledge: 0 to it, 1 (no refusal) for a broadcast
        row["acked"] = all((a == 1) if broadcast else (a == 0) for a in msg["acks"])
        row["complete"] = bool(eom)
        out.append(row)
    return out


class CecPlugin:
    """HDMI CEC on one line."""
    name = "cec"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 20e3:
            return []
        msgs = messages(ch, c.cap.n_samples, rate)
        done = [m for m in msgs if m["complete"]]
        if len(done) < 2:
            return []
        # evidence in bits: each complete message's start bit of its length and its blocks' EOM in place (4 a
        # message, 1 a block)
        info = sum(4 + (2 if "opcode" in m else 1) for m in done)
        score = (len(done) / len(msgs)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("cec", {}, {"cec": ch.name}, Stream("cec.messages", msgs),
                     {"messages": len(msgs), "complete": len(done)}, layer_score=score, total=score)]
