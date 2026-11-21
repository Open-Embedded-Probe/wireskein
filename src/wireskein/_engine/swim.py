"""SWIM (STM8's single wire debug): each bit a low pulse, short for 1 and long for 0 (low speed: 2 or 20 of the 22
SWIM clocks a bit; high speed: 2 or 8 of 10), the line idling high. A frame: the start bit (0 from the host, 1 from
the target), 3 command bits or 8 data bits MSB first, their parity (even), the receiver's acknowledge (1). The host's
command frame names ROTF (read on the fly), WOTF (write on the fly) or SRST; ROTF and WOTF go on with the byte count
N and a 24-bit address (host frames), then N bytes (the target's for ROTF, the host's for WOTF)."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

COMMANDS = {0: "SRST", 1: "ROTF", 2: "WOTF"}
SWIM_GAP = 1.6            # a gap of this many bit times between pulses ends a frame
SWIM_SHORT = 0.5          # a low shorter than this share of a bit: 1
EVIDENCE_BITS = 8
SWIM_GOOD = 0.8           # frames whose parity and acknowledge hold, this share of all at least


def frames(ch, n: int, rate: float) -> list[dict]:
    """The frames on the line: {"s", "e", "who" (host / target), "bits" (payload), "parity_ok", "ack"}."""
    st, length, level = ch.runs(n)
    sel = (level == 0)
    sel[0] = sel[-1] = False
    s, w = st[sel], length[sel]
    if len(s) < 12:
        return []
    gaps = np.diff(s)
    bit = float(np.median(gaps))
    # SWIM's bit is 10 or 22 SWIM clocks: the median spacing of pulses in frames
    if bit <= 2:
        return []
    cut = np.flatnonzero(gaps > SWIM_GAP * bit) + 1
    groups = [list(range(a, z)) for a, z in zip(np.concatenate([[0], cut]), np.concatenate([cut, [len(s)]]))]
    # an acknowledge comes from the other side, maybe after a pause: a lone pulse after a frame is its
    merged = []
    for g in groups:
        if len(g) == 1 and merged and len(merged[-1]) in (5, 10):
            merged[-1] = merged[-1] + g
        else:
            merged.append(g)
    out = []
    for g in merged:
        if len(g) not in (6, 11):
            continue
        b = [int(w[i] < SWIM_SHORT * bit) for i in g]
        payload = b[1:-2]
        out.append({"s": int(s[g[0]]), "e": int(s[g[-1]] + w[g[-1]]), "who": "target" if b[0] else "host",
                    "bits": payload, "parity_ok": (sum(payload) & 1) == b[-2], "ack": b[-1]})
    return out


def _byte(bits):
    return int("".join(map(str, bits)), 2)


def transactions(fr: list[dict]) -> list[dict]:
    """The commands and their bytes."""
    out, i = [], 0
    while i < len(fr):
        f = fr[i]
        if f["who"] != "host" or len(f["bits"]) != 3:
            i += 1
            continue
        cmd = COMMANDS.get(_byte(f["bits"]), f"CMD{_byte(f['bits'])}")
        t = {"s": f["s"], "e": f["e"], "cmd": cmd, "ok": f["parity_ok"] and f["ack"] == 1}
        i += 1
        if cmd in ("ROTF", "WOTF") and i + 4 <= len(fr) and all(len(x["bits"]) == 8 for x in fr[i:i + 4]):
            hdr = fr[i:i + 4]
            t["count"] = _byte(hdr[0]["bits"])
            t["addr"] = (_byte(hdr[1]["bits"]) << 16) | (_byte(hdr[2]["bits"]) << 8) | _byte(hdr[3]["bits"])
            t["ok"] &= all(x["parity_ok"] for x in hdr)
            i += 4
            data = []
            while i < len(fr) and len(fr[i]["bits"]) == 8 and len(data) < max(1, t["count"]):
                data.append(_byte(fr[i]["bits"]))
                t["ok"] &= fr[i]["parity_ok"]
                t["e"] = fr[i]["e"]
                i += 1
            t["data"] = bytes(data).hex()
        out.append(t)
    return out


class SwimPlugin:
    """SWIM on one line: frames of pulse-width bits with parity and acknowledge."""
    name = "swim"
    consumes = ("line",)

    def run(self, c, ch, g):
        fr = frames(ch, c.cap.n_samples, float(c.cap.rate))
        if not fr:
            return []
        good = [f for f in fr if f["parity_ok"] and f["ack"] == 1]
        # a frame's parity and acknowledge hold by chance one time in four: a line of other pulses has some
        if len(good) < 3 or len(good) < SWIM_GOOD * len(fr):
            return []
        tr = transactions(fr)
        # evidence in bits: each frame's parity and acknowledge (2), the frames' lengths (3 or 8 bits: 1)
        info = 3 * len(good)
        score = (len(good) / len(fr)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("swim", {}, {"swim": ch.name}, Stream("swim.transactions", tr),
                     {"frames": len(fr), "good": len(good), "transactions": len(tr)}, layer_score=score, total=score)]
