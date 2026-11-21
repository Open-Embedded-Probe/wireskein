"""Wiegand (an access-control reader's output): two lines idle high, D0 and D1; each bit a short low pulse on one of
them (D1: 1, D0: 0), never both at once, pulses at a steady pace; a frame is the pulses between gaps many pulse
times long. The 26-bit format (and the 34-bit one so): an even parity over the first half, an odd one over the
second; the 26-bit one's facility (8) and card (16) numbers between."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

WIEGAND_GAP = 10          # a gap this many pulse intervals long ends a frame
WIEGAND_MIN_BITS = 8      # bits a frame at least
WIEGAND_MAX_WIDTH = 0.5   # a pulse at most this share of the interval between pulses
EVIDENCE_BITS = 8
PARITY_FORMATS = (26, 34)  # an even parity over the first half, an odd one over the second


def _pulses(ch, n: int):
    """(start, width) of the line's low pulses (it idles high)."""
    st, length, level = ch.runs(n)
    sel = level == 0
    sel[0] = sel[-1] = False                     # a run cut by the capture's start or end is no whole pulse
    return st[sel], length[sel]


def read_pair(d0, d1, n: int) -> list[dict] | None:
    """The frames on D0 / D1 as given, or None where they are no Wiegand pair (pulses overlapping, of no steady
    width)."""
    if d0.initial != 1 or d1.initial != 1:
        return None
    s0, w0 = _pulses(d0, n)
    s1, w1 = _pulses(d1, n)
    if len(s0) + len(s1) < WIEGAND_MIN_BITS or not len(s0) or not len(s1):
        return None
    s = np.concatenate([s0, s1])
    w = np.concatenate([w0, w1])
    b = np.concatenate([np.zeros(len(s0), np.int8), np.ones(len(s1), np.int8)])
    o = np.argsort(s, kind="stable")
    s, w, b = s[o], w[o], b[o]
    # never both low: each pulse ends before the next starts
    if np.any(s[:-1] + w[:-1] > s[1:]):
        return None
    gaps = np.diff(s)
    pace = float(np.median(gaps)) if len(gaps) else 0.0
    if pace <= 0 or float(np.median(w)) > WIEGAND_MAX_WIDTH * pace:
        return None
    cut = np.flatnonzero(gaps > WIEGAND_GAP * pace) + 1
    frames = []
    for a, z in zip(np.concatenate([[0], cut]), np.concatenate([cut, [len(s)]])):
        bits = b[a:z].tolist()
        if len(bits) < WIEGAND_MIN_BITS:
            continue
        f = {"s": int(s[a]), "e": int(s[z - 1] + w[z - 1]), "bits": len(bits), "data": "".join(map(str, bits)),
             "value": format(int("".join(map(str, bits)), 2), "x")}
        if len(bits) in PARITY_FORMATS:
            h = len(bits) // 2
            even = sum(bits[:h]) % 2 == 0
            odd = sum(bits[h:]) % 2 == 1
            f["ok"] = even and odd
            if len(bits) == 26:
                f.update(facility=int("".join(map(str, bits[1:9])), 2), card=int("".join(map(str, bits[9:25])), 2))
        frames.append(f)
    return frames or None


class WiegandPlugin:
    """Wiegand on two lines idle high whose low pulses take turns."""
    name = "wiegand"

    def run(self, c, a: str, b: str):
        cap = c.cap
        best = None
        for d0, d1 in ((a, b), (b, a)):
            fr = read_pair(cap.channel(d0), cap.channel(d1), cap.n_samples)
            if fr:
                best = (fr, d0, d1)
                break
        if best is None:
            return []
        fr, d0, d1 = best
        # which line is D1 is not on the wire save through a format's parity: the 26- and 34-bit ones tell it
        if any(f["bits"] in PARITY_FORMATS for f in fr):
            alt = read_pair(cap.channel(d1), cap.channel(d0), cap.n_samples) or []
            if sum(bool(f.get("ok")) for f in alt) > sum(bool(f.get("ok")) for f in fr):
                fr, d0, d1 = alt, d1, d0
        nbits = sum(f["bits"] for f in fr)
        checked = sum(1 for f in fr if "ok" in f)
        good = sum(1 for f in fr if f.get("ok"))
        # evidence: pulses taking turns at a steady pace on two lines (a bit each, by the count), frames of one
        # length again, parities that hold
        lengths = [f["bits"] for f in fr]
        same = max(lengths.count(x) for x in set(lengths))
        info = min(nbits, 64) / 8 + 4 * max(0, same - 1) + 2 * good
        score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * (good / checked if checked else 0.9)
        return [Node("wiegand", {"bits": max(set(lengths), key=lengths.count)}, {"d0": d0, "d1": d1},
                     Stream("wiegand.frames", fr), {"frames": len(fr), "bits": nbits, "checked": checked,
                                                    "parity_ok": good}, layer_score=score, total=score)]
