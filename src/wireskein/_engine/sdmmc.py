"""SD (and MMC) in SD mode: the CMD line, read on CLK's rise. A command or a short response is 48 bits: start 0, the
transmission bit (1 host to card, 0 card to host), the command index (6), the argument (32), CRC7 (x^7 + x^3 + 1
over the 40 bits before it), end 1, MSB first. R3 (the OCR) has no CRC: index and CRC are all 1s. R2 (CID / CSD)
is 136 bits: start, transmission 0, six 1s, the 128-bit register whose last 7 bits before the end bit are the CRC7
of the 120 before. The clock may run between frames or stop (a command may start a burst).

Every bit position is a candidate, and a CRC7 of 7 bits holds by chance once in 128: the CRC7 of every window is
found at once (it is linear: the XOR of each 1 bit's own CRC, so a convolution per CRC bit), and a reading counts
only where commands are answered (the response that follows: an R1-type with the same index, or an R2 / R3)."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

POLY = 0x09                 # x^7 + x^3 + 1 (the x^7 term implied)
SD_ANSWERED = 0.3           # commands answered, this share of them at least
SD_RESPONSES_ANSWERING = 0.5   # responses following a command, this share of them for a whole score
EVIDENCE_BITS = 8


def _crc7_unit(nbits: int) -> np.ndarray:
    """The CRC7 of an nbits-long message holding a single 1 at each position (MSB first): [nbits] values."""
    out = np.zeros(nbits, np.int64)
    for j in range(nbits):
        crc = 0
        for k in range(nbits):
            b = 1 if k == j else 0
            fb = ((crc >> 6) & 1) ^ b
            crc = (crc << 1) & 0x7F
            if fb:
                crc ^= POLY
        out[j] = crc
    return out


_UNIT = {40: _crc7_unit(40), 120: _crc7_unit(120)}


def _crc_windows(bits: np.ndarray, nbits: int) -> np.ndarray:
    """The CRC7 of every window bits[i:i + nbits] (i = 0 .. len - nbits). Each CRC bit is the parity of a sum over the
    window; the seven sums run in one convolution, each in a 6-bit digit of its own (a sum is nbits <= 120 at most:
    the digits are 8 bits wide, 56 bits in all)."""
    unit = _UNIT[nbits]
    kern = np.zeros(nbits, np.int64)
    for k in range(7):
        kern |= ((unit >> k) & 1) << (8 * k)
    tot = np.convolve(bits.astype(np.int64), kern[::-1], "valid")
    out = np.zeros(len(tot), np.int64)
    for k in range(7):
        out |= ((tot >> (8 * k)) & 1) << k
    return out


def _value(bits: np.ndarray, i: int, n: int) -> int:
    return int("".join(map(str, bits[i:i + n].tolist())), 2)


R2_AFTER = (2, 9, 10)       # the commands an R2 (CID / CSD) answers
R3_AFTER = (41,)            # and an R3 (the OCR): ACMD41


def frames(t: np.ndarray, bits: np.ndarray) -> list[dict]:
    """The CMD line's frames, in order, not overlapping. The 48-bit ones first; an R2 (136 bits, its CRC7 over 120)
    where the frame before it is a command it answers (CMD2 / 9 / 10: it takes the place of 48-bit "frames" inside
    it, which a CRC7 of 7 bits gives by chance), an R3 (no CRC) only after ACMD41."""
    n = len(bits)
    if n < 48:
        return []
    crc = _crc_windows(bits, 40)
    m = n - 47
    idx = np.arange(m)
    got = np.zeros(m, np.int64)
    for k in range(7):
        got = (got << 1) | bits[idx + 40 + k]
    start_end = (bits[:m] == 0) & (bits[47:47 + m] == 1)
    ok48 = start_end & (crc[:m] == got)
    ones = np.ones(m, bool)
    for k in range(2, 8):
        ones &= bits[k:m + k] == 1
    r3 = start_end & (bits[1:m + 1] == 0) & ones & (got == 0x7F)
    taken = np.zeros(n, bool)
    out = []
    for i in np.flatnonzero(ok48 | r3).tolist():
        if taken[i:i + 48].any():
            continue
        taken[i:i + 48] = True
        if r3[i] and not ok48[i]:
            out.append({"start": int(t[i]), "end": int(t[i + 47]), "dir": "R3", "ocr": _value(bits, i + 8, 32),
                        "ok": True, "_i": i, "_n": 48})
        else:
            out.append({"start": int(t[i]), "end": int(t[i + 47]), "dir": "CMD" if bits[i + 1] else "RSP",
                        "index": _value(bits, i + 2, 6), "arg": _value(bits, i + 8, 32), "ok": True, "_i": i,
                        "_n": 48})
    if n >= 136:
        crc2 = _crc_windows(bits, 120)
        m2 = n - 135
        idx2 = np.arange(m2)
        got2 = np.zeros(m2, np.int64)
        for k in range(7):
            got2 = (got2 << 1) | bits[idx2 + 128 + k]
        head = (bits[:m2] == 0) & (bits[1:m2 + 1] == 0) & (bits[135:135 + m2] == 1)
        for k in range(2, 8):
            head &= bits[k:m2 + k] == 1
        for i in np.flatnonzero(head & (crc2[8:8 + m2] == got2)).tolist():
            if bits[i + 8:i + 128].min() == bits[i + 8:i + 128].max():
                continue                      # a register of all 1s (or 0s): an idle line's, no CID / CSD
            before = [f for f in out if f["_i"] + f["_n"] <= i]
            prev = before[-1] if before else None
            if prev is None or prev["dir"] != "CMD" or prev["index"] not in R2_AFTER:
                continue
            out = [f for f in out if f["_i"] + f["_n"] <= i or f["_i"] >= i + 136]
            out.append({"start": int(t[i]), "end": int(t[i + 135]), "dir": "R2", "reg0": _value(bits, i + 8, 32),
                        "reg1": _value(bits, i + 40, 32), "reg2": _value(bits, i + 72, 32),
                        "reg3": _value(bits, i + 104, 24), "ok": True, "_i": i, "_n": 136})
    out.sort(key=lambda f: f["_i"])
    keep = []
    for f in out:
        if f["dir"] == "R3" and not (keep and keep[-1]["dir"] == "CMD" and keep[-1]["index"] in R3_AFTER):
            continue
        keep.append(f)
    for f in keep:
        f.pop("_i")
        f.pop("_n")
    return keep


def answered(fr: list[dict]) -> tuple[int, int]:
    """(commands answered by the frame after them, commands)."""
    cmds = [i for i, f in enumerate(fr) if f.get("dir") == "CMD"]
    ans = 0
    for i in cmds:
        if i + 1 >= len(fr):
            continue
        a, b = fr[i], fr[i + 1]
        soon = b["start"] - a["end"] < 2 * max(1, a["end"] - a["start"])
        same = b.get("dir") in ("R2", "R3") or (b.get("dir") == "RSP" and b.get("index") == a.get("index"))
        ans += bool(soon and same)
    return ans, len(cmds)


class SdmmcPlugin:
    """SD mode's CMD line on a clocked group: CLK the clock, CMD one of its data lines."""
    name = "sdmmc"
    consumes = ("frames.gap",)

    def run(self, c, fr, g):
        sb = fr.source
        if sb.sample_edge != "rise":
            return []
        best = None
        for k, d in enumerate(sb.data):
            bits = np.asarray(sb.bits[k], np.int64)
            f = frames(np.asarray(sb.t, np.int64), bits)
            if len(f) < 2:
                continue
            ans, ncmd = answered(f)
            if ans < 1 or ans < SD_ANSWERED * ncmd:
                continue
            # evidence in bits: each frame's CRC7 (7; R3's fixed 1s alike), an answered command again (its index: 6)
            info = 7 * len(f) + 6 * ans
            score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * (0.6 + 0.4 * ans / max(1, ncmd))
            # and a card answers commands: responses that follow none are chance CRCs (a free clock sampling a CAN
            # line four times a bit: five "responses" to one command, 2026-11-21, synth-protocols-251 with spikes)
            n_rsp = sum(1 for x in f if x.get("dir") in ("RSP", "R2", "R3"))
            if n_rsp >= 2:
                score *= min(1.0, ans / n_rsp / SD_RESPONSES_ANSWERING)
            node = Node("sdmmc", {"sample_edge": "rise"}, {"clk": sb.clock, "cmd": d}, Stream("sdmmc.frames", f),
                        {"frames": len(f), "commands": ncmd, "answered": ans}, layer_score=score, total=score)
            if best is None or node.total > best.total:
                best = node
        return [best] if best is not None else []
