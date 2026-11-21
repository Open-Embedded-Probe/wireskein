"""S/PDIF (IEC 60958) on one line: biphase mark coding (a change at each cell's start, another in its middle for a 1),
subframes of 32 cells: a preamble that breaks the coding (a run of one and a half cells: B starts a block of 192
frames on channel A, M is channel A, W channel B), 4 auxiliary bits, 20 audio bits (LSB first: with the aux bits a
24-bit sample), V, U, C and an even parity over the 28 cells after the preamble."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

PREAMBLES = {(3, 1, 1, 3): "B", (3, 3, 1, 1): "M", (3, 2, 1, 2): "W"}   # in half cells
EVIDENCE_BITS = 8
SPDIF_MIN = 16            # subframes whose parity holds, at least
SPDIF_CHAIN = 0.9         # of them, this share in A / B turns a subframe apart
SPDIF_SHARE = 0.01        # a run length this share of the runs at least to count as half a cell


def subframes(edges: np.ndarray) -> list[dict]:
    if len(edges) < 200:
        return []
    d = np.diff(edges).astype(float)
    # the shortest run is half a cell: the shortest length a share of the runs have (audio of 0s is mostly whole
    # cells, a percentile would take one)
    vals, cnt = np.unique(np.round(d), return_counts=True)
    common = vals[cnt >= SPDIF_SHARE * len(d)]
    if not len(common) or common[0] < 1:
        return []
    # at a few samples a half cell its runs split over two lengths (2 and 3 for 2.8): the half cell is the length
    # the runs (of up to three halves) are whole multiples of most nearly
    x = d[d < 4 * common[0]]
    cands = np.linspace(0.8 * common[0], 1.6 * common[0], 161)
    if len(x) > 4000:
        x = x[:: len(x) // 4000]                       # (a sample of the runs tells the length)
    r = x[None, :] / cands[:, None]
    err = np.mean(np.abs(r - np.round(r)), axis=1)
    half = float(cands[int(np.argmin(err))])
    u = np.round(d / half).astype(int)
    out, n = [], len(u)
    # a subframe starts at a run of one and a half cells (a preamble's coding violation): from those only
    starts = np.flatnonzero(u[:max(0, n - 3)] == 3)
    if len(starts) < 8:
        return []
    k, i = 0, int(starts[0])
    while i + 4 <= n:
        pre = PREAMBLES.get(tuple(u[i:i + 4].tolist()))
        if pre is None:
            k = int(np.searchsorted(starts, i + 1))
            if k >= len(starts):
                break
            i = int(starts[k])
            continue
        s0 = int(edges[i])
        j, bits = i + 4, []
        while j < n and len(bits) < 28:
            if u[j] == 2:
                bits.append(0)
                j += 1
            elif u[j] == 1 and j + 1 < n and u[j + 1] == 1:
                bits.append(1)
                j += 2
            else:
                break
        if len(bits) < 28:
            i = j if j > i + 4 else i + 1
            continue
        val = sum(b << k for k, b in enumerate(bits[:24]))
        out.append({"s": s0, "e": int(edges[j]), "preamble": pre, "audio": val, "v": bits[24], "u": bits[25],
                    "c": bits[26], "ok": sum(bits) % 2 == 0})
        i = j
    return out


class SpdifPlugin:
    """S/PDIF on one line."""
    name = "spdif"
    consumes = ("line",)

    def run(self, c, ch, g):
        sf = subframes(np.asarray(ch.edges, np.int64))
        good = [x for x in sf if x["ok"]]
        if len(good) < SPDIF_MIN:
            return []
        # subframes in their order: channel A (B / M) and B (W) by turns, one subframe's time apart (32 cells);
        # preambles a line of other pulses shows by chance come in no such chain
        span = float(np.median([x["e"] - x["s"] for x in good]))
        chain = sum(1 for a, b in zip(good, good[1:])
                    if ((a["preamble"] == "W") != (b["preamble"] == "W")) and abs((b["s"] - a["s"]) / span - 1) < 0.1)
        if chain < SPDIF_CHAIN * (len(good) - 1):
            return []
        # evidence: each subframe's preamble (a coding violation of its own pattern: 3) and parity (1)
        info = 4 * len(good)
        score = (len(good) / len(sf)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        blocks = sum(1 for x in sf if x["preamble"] == "B")
        return [Node("spdif", {}, {"data": ch.name}, Stream("spdif.subframes", sf),
                     {"subframes": len(sf), "ok": len(good), "blocks": blocks}, layer_score=score, total=score)]
