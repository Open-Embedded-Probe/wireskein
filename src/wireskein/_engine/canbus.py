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

    def idle_falls(self, min_ticks: float) -> np.ndarray:
        """The falls after a recessive run of min_ticks or more (from the capture's start where it began
        recessive): the places a start of frame may be after an error."""
        k = np.searchsorted(self.rises, self.falls, side="right")
        start0 = float("-inf") if self.initial == self.idle else 0.0
        since = np.where(k > 0, self.rises[np.maximum(k - 1, 0)].astype(np.float64), start0)
        return self.falls[self.falls - since >= min_ticks]

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


def _read_frame(line: _Line, sof: int, u: float, sp: float, n: int, fd_ratio: float | None = None,
                sp_data: float = 0.75, boundary_stuff: bool = False, seen: dict | None = None) -> tuple[dict, float]:
    """One frame from its start of frame: (the frame, the tick its end of frame ends). fd_ratio: CAN FD's frames are
    read too (FDF recessive), their data phase (BRS recessive) at u / fd_ratio with its sample point at sp_data;
    None: the bit after IDE (RTR in an extended frame) is r0 / r1 whatever its value (classic CAN alone)."""
    rec = line.idle
    st = {"edge": float(sof), "prev_sample": float(sof), "k": 0, "u": u, "sp": sp}

    def raw():
        # the next bit at the sample point, resynchronized at a recessive-to-dominant edge since the last sample
        bu, bsp = st["u"], st["sp"]
        nominal = st["edge"] + bsp * bu
        f = line.fall_between(st["prev_sample"], nominal)
        if f is not None and st["k"] > 0:
            st["edge"] = float(f)
        t = st["edge"] + bsp * bu
        if t >= n:
            raise _Cut()
        st["prev_sample"] = t
        st["edge"] += bu
        st["k"] += 1
        return (0 if line.level(t) != rec else 1), t

    def switch(nu, nsp):
        # at the last bit's sample point the bit time changes: the rest of that bit is the new timing's phase 2
        st["u"], st["sp"] = nu, nsp
        st["edge"] = st["prev_sample"] + (1 - nsp) * nu

    run = {"v": None, "c": 0, "stuff": 0}
    body = []
    sent = []                  # the bits as sent, dynamic stuff bits too (CAN FD's CRC covers them)

    def stuffed(field):
        b, t = raw()
        if run["c"] == 5:
            if b == run["v"]:
                raise _Error("stuff", t, field)
            sent.append(b)
            run["v"], run["c"] = b, 1
            run["stuff"] += 1
            b, t = raw()
        run["c"] = run["c"] + 1 if b == run["v"] else 1
        run["v"] = b
        body.append(b)
        sent.append(b)
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
        fdf = bits(1, "r1")
        ident, remote = (id_a << 18) | id_b, rtr
    else:
        ident, remote = id_a, rtr_srr
        fdf = None
    if fd_ratio is None:
        bits(1, "r0")
    else:
        if not ide:
            fdf = bits(1, "fdf")            # r0's place in a classic frame
        if fdf:
            if seen is not None:
                seen["fd"] = True           # (what follows depends on the data rate tried)
            fr = _read_fd(sof, u, sp, fd_ratio, sp_data, raw, switch, bits, fixed, run, sent, int(ident), bool(ide),
                          boundary_stuff, seen)
            return fr, st["edge"]
        if ide:
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


FD_LENGTHS = (0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64)    # CAN FD's data bytes by DLC
CRC17_POLY, CRC21_POLY = 0x1685B, 0x102899      # x^17 + ..., x^21 + ... (the top term implied)


def crc_fd(bits, width: int, poly: int) -> int:
    """CAN FD's CRC-17 / CRC-21 (ISO 11898-1:2015: the register starts with its top bit set)."""
    crc = 1 << (width - 1)
    top, mask = 1 << (width - 1), (1 << width) - 1
    for b in bits:
        nxt = b ^ (1 if crc & top else 0)
        crc = (crc << 1) & mask
        if nxt:
            crc ^= poly
    return crc


GRAY = (0, 1, 3, 2, 6, 7, 5, 4)         # the stuff count mod 8, Gray-coded, as sent


def _read_fd(sof, u, sp, ratio, sp_data, raw, switch, bits, fixed, run, sent, ident, ide, boundary_stuff,
             seen=None) -> dict:
    """The rest of a CAN FD frame after FDF: res, BRS, ESI, DLC, data (dynamically stuffed), the stuff count and the
    CRC (a fixed stuff bit, the complement of the bit before, ahead of them and after every 4 bits), the delimiter,
    ACK. With BRS recessive the bits from after BRS's sample point to the CRC delimiter's are at u / ratio."""
    bits(1, "res")
    brs = bits(1, "brs")
    if seen is not None:
        seen["brs"] = bool(brs)             # (without BRS the data rate tried changes nothing)
    if brs:
        switch(u / ratio, sp_data)
    esi = bits(1, "esi")
    dlc = bits(4, "dlc")
    nbytes = FD_LENGTHS[dlc]
    data = bytes(bits(8, "data") for _ in range(nbytes))
    if seen is not None:
        seen["five"] = run["c"] == 5        # (the boundary's two readings differ only so)
    if boundary_stuff and run["c"] == 5:
        # five equal bits ending the data (or the DLC): a dynamic stuff bit before the fixed one, counted and under
        # the CRC (else the fixed stuff bit stands for it): which one a transmitter does, the stuff count and the CRC
        # tell
        b, t = raw()
        if b == run["v"]:
            raise _Error("stuff", t, "data")
        sent.append(b)
        run["stuff"] += 1
        run["v"] = b
    elif boundary_stuff:
        raise _Error("stuff", 0.0, "boundary")            # (no such bit: the reading without it is the one)
    covered = list(sent)
    width, poly = (17, CRC17_POLY) if nbytes <= 16 else (21, CRC21_POLY)
    prev = {"v": run["v"]}

    def fsb(field):
        b, t = raw()
        if b == prev["v"]:
            raise _Error("stuff", t, field)
        prev["v"] = b

    def plain(field):
        b, t = raw()
        prev["v"] = b
        return b

    fsb("stuff_count")
    sc = [plain("stuff_count") for _ in range(4)]
    crc_bits = []
    k = 0
    while len(crc_bits) < width:
        if k % 4 == 0:
            fsb("crc")
        crc_bits.append(plain("crc"))
        k += 1
    count = (sc[0] << 2) | (sc[1] << 1) | sc[2]
    if (sum(sc[:3]) & 1) != sc[3] or count != GRAY[run["stuff"] % 8]:
        _, t = raw()
        raise _Error("stuff_count", t, "stuff_count")
    crc = 0
    for b in crc_bits:
        crc = (crc << 1) | b
    if crc != crc_fd(covered + sc, width, poly):
        _, t = raw()
        raise _Error("crc", t, "crc")
    fixed("crc_delimiter")
    if brs:
        switch(u, sp)
    ack_bit, _ = raw()
    fixed("ack_delimiter")
    for _ in range(6):
        fixed("eof")
    last, t = raw()
    frame = {"tick": int(sof), "id": ident, "ext": ide, "rtr": False, "dlc": int(dlc), "data": data.hex(),
             "ack": ack_bit == 0, "fd": True, "brs": bool(brs), "esi": bool(esi)}
    if last == 0:
        frame["overload"] = int(t)
    return frame


FD_RATIOS = (1, 2, 4, 5, 8, 10)     # CAN FD's data bit rate over its nominal one, tried in turn (the CRC tells)


def read(ch: Channel, n: int, u: float, idle: int = 1, sp: float = 0.75, fd_ratios: tuple | None = None,
         sp_data: float = 0.75, give_up: int | None = None) -> dict:
    """The frames and errors a node at bit time u (ticks) reads: {"frames", "errors" [{"kind", "tick", "field"}],
    "lead_in" (ticks before the first frame it could start), "cut_at_end", "error_flags", "overloads"}. fd_ratios:
    CAN FD's frames read too, each at the first of these data rates (u / ratio) its stuff count and CRC hold at
    (`data_ratio`); None: classic CAN alone. give_up: stop after this many errors with no frame read yet (a line
    that is no CAN, or CAN at another bit time: "gave_up")."""
    line = _Line(ch, idle)
    frames, errors, overloads = [], [], []
    need, after, pos = 11, float("-inf"), 0.0    # recessive bits before a start of frame, counted from `after`
    # (a line recessive from the capture's start was idle before it: its first frame needs no 11 bits seen)
    lead_in, cut = None, False
    # after an error (or at the start) a frame starts only after 11 recessive bits: those falls found at once, not
    # every fall of a line of no CAN walked to them
    idle_falls = line.idle_falls((11 - 0.5) * u)
    while True:
        if need == 11 and after <= 0.0:
            k = int(np.searchsorted(idle_falls, pos, side="left"))
            if k == len(idle_falls):
                break
            pos = max(pos, float(idle_falls[k]))
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
            fr, end = _read_any(line, f, u, sp, n, fd_ratios, sp_data)
        except _Cut:
            cut = True
            break
        except _Error as x:
            errors.append({"kind": x.kind, "tick": int(x.tick), "field": x.field})
            need, after, pos = 11, 0.0, x.tick
            if give_up is not None and not frames and len(errors) >= give_up:
                break
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
            **({"gave_up": True} if give_up is not None and not frames and len(errors) >= give_up else {}),
            "error_flags": flags, "overloads": overloads}


def _read_any(line, sof, u, sp, n, fd_ratios, sp_data):
    """A frame read classic, or with CAN FD's frames at each data rate in turn: the first that holds; else the error
    of the reading that got furthest."""
    if fd_ratios is None:
        return _read_frame(line, sof, u, sp, n)
    worst, cut = None, False
    first, seen = {}, {}
    for r, bs in ((r, bs) for r in fd_ratios for bs in (False, True)):
        if first and not first.get("brs") and r != fd_ratios[0]:
            break                   # no BRS: every data rate reads the same
        if bs and not seen.get("five"):
            continue                # no five equal bits at the data's end (read at this rate, just before): no
                                    # second reading of the boundary
        seen = {}
        try:
            fr, end = _read_frame(line, sof, u, sp, n, fd_ratio=r, sp_data=sp_data, boundary_stuff=bs, seen=seen)
        except _Error as x:
            if not seen:
                raise               # a classic frame's error, or one before FDF: the same at every data rate
            first = first or seen
            if worst is None or x.tick > worst.tick:
                worst = x
            continue
        except _Cut:
            if not seen:
                raise
            first = first or seen
            cut = True              # (a data rate too slow runs past the capture's end)
            continue
        if fr.get("fd") and fr.get("brs"):
            fr["data_ratio"] = r
        return fr, end
    if cut:
        raise _Cut()
    raise worst


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
