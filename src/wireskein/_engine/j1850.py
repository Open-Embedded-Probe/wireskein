"""SAE J1850 VPW (10.4 kb/s, GM's class 2 / OBD-II): one line, passive low, active high; a frame starts with SOF
(active, 200 us), then bits as alternating passive and active symbols of 64 us (short) or 128 us (long): a passive
short or an active long is 0, a passive long or an active short is 1; an EOD (passive, 200 us) ends the data, an
EOF (passive, longer) the frame. Bytes MSB first, the last the CRC8 (x^8 + x^4 + x^3 + x^2 + 1, preset 0xFF,
inverted). A comparator's offset lengthens one level and shortens the other: short and long are told apart for each
level by its own two lengths."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

GLITCH_S = 10e-6
SOF_S = (163e-6, 240e-6)
SYMBOL_S = (34e-6, 163e-6)
EVIDENCE_BITS = 8


def crc8(data: bytes) -> int:
    c = 0xFF
    for x in data:
        c ^= x
        for _ in range(8):
            c = ((c << 1) ^ 0x1D) & 0xFF if c & 0x80 else (c << 1) & 0xFF
    return c ^ 0xFF


def _runs(ch, n, rate):
    """The line's runs (seconds, level) with glitches shorter than GLITCH_S merged into their neighbours."""
    st, length, level = ch.runs(n)
    t, lv = [], []
    for a, l, v in zip(st.tolist(), length.tolist(), level.tolist()):
        d = l / rate
        if t and (d < GLITCH_S or lv[-1] == v):
            t[-1] += d
            continue
        t.append(d)
        lv.append(v)
    # a glitch merged into the run before it: the run after, of the same level, joins it too
    out_t, out_v = [], []
    for d, v in zip(t, lv):
        if out_v and out_v[-1] == v:
            out_t[-1] += d
        else:
            out_t.append(d)
            out_v.append(v)
    return np.asarray(out_t), np.asarray(out_v)


def _split(x: np.ndarray) -> float:
    """The length between a level's short and long symbols: the midpoint of their two clusters."""
    x = x[(x >= SYMBOL_S[0]) & (x <= SYMBOL_S[1])]
    if len(x) < 4:
        return 96e-6
    m = float(np.median(x))
    lo, hi = x[x < m], x[x >= m]
    if not len(lo) or not len(hi) or float(np.median(hi)) - float(np.median(lo)) < 30e-6:
        return 96e-6
    return (float(np.median(lo)) + float(np.median(hi))) / 2


def frames(ch, n: int, rate: float) -> list[dict]:
    d, v = _runs(ch, n, rate)
    if len(d) < 20:
        return []
    split = {lv: _split(d[v == lv]) for lv in (0, 1)}
    out, i, pos = [], 0, 0.0
    starts = np.concatenate([[0.0], np.cumsum(d)[:-1]])
    while i < len(d):
        if not (v[i] == 1 and SOF_S[0] <= d[i] <= SOF_S[1]):
            i += 1
            continue
        s0 = starts[i]
        bits, j = [], i + 1
        while j < len(d) and SYMBOL_S[0] <= d[j] <= SYMBOL_S[1]:
            short = d[j] < split[int(v[j])]
            bits.append(int(short) if v[j] == 1 else int(not short))
            j += 1
        i = j
        if len(bits) < 16 or len(bits) % 8:
            continue
        data = bytes(int("".join(map(str, bits[k:k + 8])), 2) for k in range(0, len(bits), 8))
        out.append({"s": int(round(s0 * rate)), "e": int(round(starts[min(j, len(d) - 1)] * rate)),
                    "bytes": data.hex(), "ok": crc8(data[:-1]) == data[-1]})
    return out


class J1850Plugin:
    """SAE J1850 VPW on one line."""
    name = "j1850"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 100e3:
            return []
        fr = frames(ch, c.cap.n_samples, rate)
        good = [f for f in fr if f["ok"]]
        if len(good) < 2:
            return []
        info = 10 * len(good)                       # each frame's CRC8 and its SOF / symbols of VPW's lengths
        score = (len(good) / len(fr)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("j1850", {"variant": "vpw"}, {"data": ch.name}, Stream("j1850.frames", fr),
                     {"frames": len(fr), "ok": len(good)}, layer_score=score, total=score)]
