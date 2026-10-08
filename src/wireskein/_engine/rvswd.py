"""WCH RVSWD and SWIO, and the RISC-V Debug Module on their DMI transactions.

RVSWD frames are declared in decl/rvswd.toml; this module gives the re-framing
at stop intervals the declaration names (frame.reframe = "rvswd_stop"), the
SWIO plugin (one wire, pulse widths) and the Debug Module above both.

Frame layout (53 clocks, from wch-protocols tools/dmi_decode.py): addr7, R/W
(1 = write), header parity, park, ctl4, data32, data parity, park, status2,
pad2, stop. A 54-clock frame is a read with one extra turnaround clock (data
at bits 15..46).
"""

from __future__ import annotations

import numpy as np

from .stack import Node, Stream

DM_NAMES = {0x04: "data0", 0x05: "data1", 0x10: "dmcontrol", 0x11: "dmstatus", 0x12: "hartinfo", 0x16: "abstractcs",
            0x17: "command", 0x18: "abstractauto", 0x38: "sbcs", 0x39: "sbaddress0", 0x3c: "sbdata0", 0x40: "haltsum0",
            0x7C: "wch_7c", 0x7D: "wch_7d", 0x7E: "wch_7e", 0x7F: "wch_7f", **{0x20 + i: f"progbuf{i}" for i in range(8)}}


def _int(bits) -> int:
    v = 0
    for b in bits:
        v = (v << 1) | int(b)
    return v


def reframe_rolling(fr, mult: float = 3.0, w: int = 33) -> list[tuple[int, int]]:
    """Alternative framing for fast transfers: a gap is an interval over `mult`
    times the moving median of the surrounding w intervals (frames contain
    turnaround intervals that are long compared with their direct neighbours)."""
    from numpy.lib.stride_tricks import sliding_window_view
    t = fr.source.t
    if len(t) < w + 2:
        return reframe(fr)
    iv = np.diff(t).astype(np.float64)
    med = np.median(sliding_window_view(np.pad(iv, (w // 2, w // 2), mode="edge"), w), axis=1)
    cut = np.flatnonzero(iv > mult * med) + 1
    b = np.concatenate(([0], cut, [len(t)]))
    out = []
    for x, y in zip(b[:-1].tolist(), b[1:].tolist()):
        if y - x >= 3 and t[y - 1] - t[y - 2] > 1.8 * np.median(np.diff(t[x:y])):
            y -= 1
        out.append((x, y))
    return out


def reframe(fr) -> list[tuple[int, int]]:
    """RVSWD knowledge: a frame ends with a long clock interval (about 2.7x the
    period) before a final stop clock, and frames may follow each other with a
    gap only ~3x the period. Split at intervals > 2.5x the frame's median and
    drop a trailing stop clock that follows an interval > 1.8x the median."""
    t = fr.source.t
    out = []
    for a, b in fr.bounds.tolist():
        if b - a < 8:
            out.append((a, b))
            continue
        iv = np.diff(t[a:b]).astype(np.float64)
        med = float(np.median(iv))
        cuts = [a, *(a + 1 + np.flatnonzero(iv > 2.5 * med)).tolist(), b]
        for x, y in zip(cuts[:-1], cuts[1:]):
            if y - x >= 3 and t[y - 1] - t[y - 2] > 1.8 * med:
                y -= 1
            if y > x:
                out.append((x, y))
    return out


def memory_log(tx: list[dict]) -> list[list]:
    """Abstract commands paired with data0/data1: memory and register accesses."""
    regs, out = {}, []
    for x in tx:
        op, addr, data = x["op"], x["addr"], x["data"]
        if op == "BURST":
            out.append(["BURST", regs.get(0x05), data])
            continue
        if op == "W" and addr in (0x04, 0x05):
            regs[addr] = data
        elif op == "R" and addr == 0x04:
            if out and out[-1][0] in ("MEMR", "REGR") and out[-1][2] is None:
                out[-1][2] = data
        elif op == "W" and addr == 0x17:
            cmdtype = data >> 24
            write = (data >> 16) & 1
            if cmdtype == 2:
                a = regs.get(0x05)
                out.append(["MEMW" if write else "MEMR", a, regs.get(0x04) if write else None])
                if (data >> 19) & 1 and a is not None:
                    regs[0x05] = a + (1 << ((data >> 20) & 7))
            elif cmdtype == 0:
                out.append(["REGW" if write else "REGR", data & 0xFFFF, regs.get(0x04) if write else None])
    return out


def dm_node(parent: Node) -> Node | None:
    tx = parent.output.items
    if not tx:
        return None
    known = float(np.mean([x["addr"] in DM_NAMES for x in tx if x["op"] != "BURST"] or [0]))
    log = memory_log(tx)
    has_ctrl = any(x["addr"] == 0x10 for x in tx) and any(x["addr"] == 0x11 for x in tx)
    score = known * (0.5 + 0.5 * has_ctrl)
    return Node("riscv_dm", {}, parent.roles, Stream("dm.accesses", log),
                {"dm_addr": known, "accesses": len(log), "dmcontrol_dmstatus": has_ctrl}, parent,
                layer_score=score, total=score)


class SwioPlugin:
    """WCH SWIO (1-wire SDI) on pulse symbols: short pulse = 1, long = 0.
    41 pulses: start(1) addr7 R/W(1 = write) data32; 33 pulses: 0 + data32
    (fast-read continuation, data0). Carries the same DMI transactions as RVSWD,
    so the same Debug Module plugin sits on top. SWIO has no parity: the
    evidence is the frame structure and the DM register addresses."""
    name = "swio"
    consumes = ("pulses",)

    def run(self, c, ps, g):
        bits_all = ps.short.astype(np.int8)
        tx, lens = [], []
        for a, b in ps.bounds.tolist():
            f = bits_all[a:b]
            lens.append(b - a)
            if b - a == 41 and f[0] == 1:
                tx.append({"t": int(ps.start[a]), "op": "W" if f[8] else "R", "addr": _int(f[1:8]), "data": _int(f[9:]),
                           "status": 0, "ok": True})
            elif b - a == 33 and f[0] == 0:
                tx.append({"t": int(ps.start[a]), "op": "R", "addr": 0x04, "data": _int(f[1:]), "status": 0, "ok": True,
                           "fast": True})
        if len(tx) < 2:
            return []
        lens = np.asarray(lens)
        structured = float(np.isin(lens, (41, 33)).mean())
        # share of pulses inside 41/33 frames: other traffic on the pin (e.g. a
        # debugger probing in the 2-wire form first) must not dilute the evidence
        pulses = float(lens[np.isin(lens, (41, 33))].sum() / max(1, lens.sum()))
        known = float(np.mean([x["addr"] in DM_NAMES for x in tx]))
        m = {"frames": int(len(lens)), "structured": structured, "pulse_share": pulses, "dmi": len(tx),
             "dm_addr": known, "split_ns": ps.split / c.cap.rate * 1e9}
        score = known * (0.5 + 0.5 * pulses) * (1 - 0.5 ** (len(tx) / 4))
        return [Node("swio", {"idle": ps.idle}, {"dio": ps.pin}, Stream("dmi", tx), m, layer_score=score, total=score)]
