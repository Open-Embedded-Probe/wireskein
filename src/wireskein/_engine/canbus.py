"""Classic CAN read as a receiving node does (ISO 11898-1), for the checks: one line at the bus level (a
controller's RX, or TX and RX wired together; recessive = the idle level).

A node starts a frame at a recessive-to-dominant edge after the bus was idle (11 recessive bits; 3 after a frame's
end, its intermission), samples each bit at the sample point and resynchronizes at each recessive-to-dominant edge.
From the start of frame to the end of the CRC a sixth equal bit is a stuff error; a fixed-form bit (the CRC and ACK
delimiters, the end of frame) dominant is a form error; a CRC that does not match is a CRC error. After an error the
reading waits for the bus idle again (an error frame's delimiter and the intermission are 11 recessive bits).
"""

from __future__ import annotations

import numpy as np

from .model import Channel

CRC_POLY = 0x4599


def crc15(bits) -> int:
    crc = 0
    for b in bits:
        nxt = b ^ ((crc >> 14) & 1)
        crc = (crc << 1) & 0x7FFF
        if nxt:
            crc ^= CRC_POLY
    return crc


class _Line:
    def __init__(self, ch: Channel, idle: int):
        self.e = np.asarray(ch.edges, np.int64)
        self.initial = ch.initial
        self.idle = idle
        lv = self.initial ^ ((np.arange(len(self.e)) + 1) & 1)      # the level after each edge
        self.falls = self.e[lv != idle]                              # recessive to dominant
        self.rises = self.e[lv == idle]

    def level(self, t: float) -> int:
        k = int(np.searchsorted(self.e, t, side="right"))
        return self.initial ^ (k & 1)

    def next_fall(self, t: float):
        k = int(np.searchsorted(self.falls, t, side="left"))
        return int(self.falls[k]) if k < len(self.falls) else None

    def fall_between(self, a: float, b: float):
        k = int(np.searchsorted(self.falls, a, side="right"))
        return int(self.falls[k]) if k < len(self.falls) and self.falls[k] <= b else None

    def recessive_since(self, t: float) -> float:
        """The tick the line went recessive before t (the last rise at or before it), -inf from the start."""
        k = int(np.searchsorted(self.rises, t, side="right"))
        if k:
            return float(self.rises[k - 1])
        return float("-inf") if self.initial == self.idle else 0.0


class _Error(Exception):
    def __init__(self, kind: str, tick: float, field: str):
        self.kind, self.tick, self.field = kind, tick, field


class _Cut(Exception):
    pass


def _read_frame(line: _Line, sof: int, u: float, sp: float, n: int) -> tuple[dict, float]:
    """One frame from its start of frame: (the frame, the tick its end of frame ends)."""
    rec = line.idle
    st = {"edge": float(sof), "prev_sample": float(sof), "k": 0}

    def raw():
        # the next bit at the sample point, resynchronized at a recessive-to-dominant edge since the last sample
        nominal = st["edge"] + sp * u
        f = line.fall_between(st["prev_sample"], nominal)
        if f is not None and st["k"] > 0:
            st["edge"] = float(f)
        t = st["edge"] + sp * u
        if t >= n:
            raise _Cut()
        st["prev_sample"] = t
        st["edge"] += u
        st["k"] += 1
        return (0 if line.level(t) != rec else 1), t

    run = {"v": None, "c": 0}
    body = []

    def stuffed(field):
        b, t = raw()
        if run["c"] == 5:
            if b == run["v"]:
                raise _Error("stuff", t, field)
            run["v"], run["c"] = b, 1
            b, t = raw()
        run["c"] = run["c"] + 1 if b == run["v"] else 1
        run["v"] = b
        body.append(b)
        return b

    def bits(k, field):
        v = 0
        for _ in range(k):
            v = (v << 1) | stuffed(field)
        return v

    def fixed(field):
        b, t = raw()
        if b != 1:
            raise _Error("form", t, field)

    if bits(1, "sof") != 0:
        raise _Error("form", sof + sp * u, "sof")
    id_a = bits(11, "id")
    rtr_srr = bits(1, "rtr")
    ide = bits(1, "ide")
    if ide:
        id_b = bits(18, "id")
        rtr = bits(1, "rtr")
        bits(1, "r1")
        ident, remote = (id_a << 18) | id_b, rtr
    else:
        ident, remote = id_a, rtr_srr
    bits(1, "r0")
    dlc = bits(4, "dlc")
    data = bytes(bits(8, "data") for _ in range(0 if remote else min(dlc, 8)))
    covered = len(body)
    crc = bits(15, "crc")
    if run["c"] == 5:                                     # a stuff bit after the CRC's last five equal bits
        b, t = raw()
        if b == run["v"]:
            raise _Error("stuff", t, "crc")
    crc_ok = crc == crc15(body[:covered])
    if not crc_ok:
        _, t = raw()
        raise _Error("crc", t, "crc")
    fixed("crc_delimiter")
    ack_bit, _ = raw()
    fixed("ack_delimiter")
    for _ in range(6):
        fixed("eof")
    last, t = raw()                                       # a dominant last bit of the end of frame is an overload
    frame = {"tick": int(sof), "id": int(ident), "ext": bool(ide), "rtr": bool(remote), "dlc": int(dlc),
             "data": data.hex(), "ack": ack_bit == 0}
    if last == 0:
        frame["overload"] = int(t)
    return frame, st["edge"]


def read(ch: Channel, n: int, u: float, idle: int = 1, sp: float = 0.75) -> dict:
    """The frames and errors a node at bit time u (ticks) reads: {"frames", "errors" [{"kind", "tick", "field"}],
    "lead_in" (ticks before the first frame it could start), "cut_at_end", "error_flags", "overloads"}."""
    line = _Line(ch, idle)
    frames, errors, overloads = [], [], []
    need, after, pos = 11, 0.0, 0.0      # recessive bits before a start of frame, counted from `after`
    lead_in, cut = None, False
    while True:
        f = line.next_fall(pos)
        if f is None:
            break
        since = max(line.recessive_since(f), after)
        if f - since < (need - 0.5) * u:
            if need == 3 and f - since >= 0.5 * u:        # dominant inside the intermission: an overload frame
                overloads.append({"tick": int(f)})
            need, pos = 11, f + 1
            continue
        if lead_in is None:
            lead_in = int(f)
        try:
            fr, end = _read_frame(line, f, u, sp, n)
        except _Cut:
            cut = True
            break
        except _Error as x:
            errors.append({"kind": x.kind, "tick": int(x.tick), "field": x.field})
            need, after, pos = 11, 0.0, x.tick
            continue
        if "overload" in fr:
            overloads.append({"tick": fr.pop("overload")})
            need, after, pos = 11, 0.0, end
        else:
            need, after, pos = 3, end, end
        frames.append(fr)
    start, length, level = ch.runs(n)
    flags = int(np.sum((level != idle) & (length >= 5.5 * u * 1)))   # dominant six bits or more: error / overload flags
    return {"frames": frames, "errors": errors, "lead_in": lead_in or 0, "cut_at_end": cut,
            "error_flags": flags, "overloads": overloads}


def bit_time(ch: Channel, u0: float, idle: int = 1) -> float | None:
    """The bit time from the intervals between recessive-to-dominant edges near whole bits of u0 (the same edge's
    delay cancels), least squares."""
    line = _Line(ch, idle)
    d = np.diff(line.falls).astype(np.float64)
    if not len(d):
        return None
    u = u0
    for _ in range(3):
        k = np.round(d / u)
        keep = (k >= 1) & (k <= 10) & (np.abs(d / u - k) < 0.3)
        if not keep.any():
            return None
        u = float(d[keep].sum() / k[keep].sum())
    return u
