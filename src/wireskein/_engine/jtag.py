"""JTAG (IEEE 1149.1): TCK, TMS walking the TAP's state machine, TDI and TDO shifting in its Shift-IR / Shift-DR
states. The survey sees TMS, TDI and TDO as data lines of TCK; the plugin tries each as TMS and walks the TAP over
the bits sampled on TCK's rise. Every TMS sequence is a legal walk, so the evidence is in what the walk explains:
TDI's and TDO's changes falling in the shift states, IR scans of one length, and each IR scan's first two bits out
on TDO being 1 then 0 (the IR's capture value, required by the standard: it also tells TDO from TDI). Where the walk
starts is not known until TMS has narrowed the 16 states to one (five 1s lead any state to Test-Logic-Reset).
"""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

TLR, RTI, SDS, CDR, SDR, E1DR, PDR, E2DR, UDR, SIS, CIR, SIR, E1IR, PIR, E2IR, UIR = range(16)
NAMES = ["TEST-LOGIC-RESET", "RUN-TEST/IDLE", "SELECT-DR-SCAN", "CAPTURE-DR", "SHIFT-DR", "EXIT1-DR", "PAUSE-DR",
         "EXIT2-DR", "UPDATE-DR", "SELECT-IR-SCAN", "CAPTURE-IR", "SHIFT-IR", "EXIT1-IR", "PAUSE-IR", "EXIT2-IR",
         "UPDATE-IR"]
NEXT = np.array([  # [state][tms]
    (RTI, TLR), (RTI, SDS), (CDR, SIS), (SDR, E1DR), (SDR, E1DR), (PDR, UDR), (PDR, E2DR), (SDR, UDR),
    (RTI, SDS), (CIR, TLR), (SIR, E1IR), (SIR, E1IR), (PIR, UIR), (PIR, E2IR), (SIR, UIR), (RTI, SDS)], np.int8)

JTAG_SCANS = 4            # scans for half the evidence of their count (q)
JTAG_CAPTURES = 2         # IR scans with TDO's 1 0 seen, at least, for a claim
JTAG_IN_SHIFT = 0.9       # TDI's / TDO's changes in the shift states, this share for full weight
EVIDENCE_BITS = 8


def walk(tms: np.ndarray) -> np.ndarray:
    """The TAP's state before each sampled TMS bit; -1 while the state is not yet known (more than one possible)."""
    out = np.full(len(tms), -1, np.int8)
    states = set(range(16))
    cur = -1
    for i, b in enumerate(tms.tolist()):
        if cur < 0:
            if len(states) == 1:
                cur = next(iter(states))
            else:
                states = {int(NEXT[s, b]) for s in states}
                continue
        out[i] = cur
        cur = int(NEXT[cur, b])
    return out


def scans(st: np.ndarray, t: np.ndarray, tdi: np.ndarray | None, tdo: np.ndarray | None) -> list[dict]:
    """The IR and DR scans: the bits shifted (on the edges whose state is Shift-IR / Shift-DR) from Capture to
    Update, LSB first as shifted, with TDI's and TDO's values."""
    out, cur = [], None
    for i, s in enumerate(st.tolist()):
        if s in (CDR, CIR):
            cur = {"s": int(t[i]), "type": "DR" if s == CDR else "IR", "tdi": [], "tdo": []}
        elif s in (SDR, SIR) and cur is not None:
            if tdi is not None:
                cur["tdi"].append(int(tdi[i]))
            if tdo is not None:
                cur["tdo"].append(int(tdo[i]))
        elif s in (UDR, UIR) and cur is not None:
            out.append(cur)
            cur = None
    return out


def _value(bits: list[int]) -> str:
    """Bits shifted LSB first, as hex (MSB first)."""
    v = sum(b << k for k, b in enumerate(bits))
    return format(v, "x")


def _changes_in_shift(row: np.ndarray, st: np.ndarray) -> tuple[int, int]:
    known = st >= 0
    ch = np.flatnonzero(np.diff(row.astype(np.int8)) != 0) + 1
    ch = ch[known[ch]]
    inside = np.isin(st[ch], (SDR, SIR)) | np.isin(st[ch - 1], (SDR, SIR))
    return int(inside.sum()), len(ch)


class JtagPlugin:
    """JTAG on a clocked group: TMS, TDI and TDO among its data lines (TDO may be missing)."""
    name = "jtag"
    consumes = ("frames.gap",)

    def run(self, c, fr, g):
        sb = fr.source
        if len(sb.data) < 2 or sb.sample_edge != "rise":
            return []
        clk = c.sv.clocks[sb.clock].clock_score
        best = None
        for mi, m in enumerate(sb.data):
            st = walk(sb.bits[mi])
            if not (st >= 0).any():
                continue
            others = [i for i in range(len(sb.data)) if i != mi]
            # which of the others is TDO: the IR scans' first two bits out are 1, 0
            ir_tdo, conc = {}, {}
            for i in others:
                inn, tot = _changes_in_shift(sb.bits[i], st)
                conc[i] = inn / tot if tot else 0.0
                sc = [s for s in scans(st, sb.t, None, sb.bits[i]) if s["type"] == "IR" and len(s["tdo"]) >= 2]
                ir_tdo[i] = (sum(s["tdo"][:2] == [1, 0] for s in sc), len(sc))
            tdo = max(others, key=lambda i: (ir_tdo[i][0], conc[i]))
            if ir_tdo[tdo][0] < JTAG_CAPTURES:
                # without TDO's IR capture the walk has IR scans of one length for evidence alone, which TMS taken
                # from a line of pulses with another's changes in its "shifts" gives too
                continue
            rest = [i for i in others if i != tdo]
            tdi = max(rest, key=lambda i: conc[i]) if rest else None
            sc = scans(st, sb.t, sb.bits[tdi] if tdi is not None else None, sb.bits[tdo] if tdo is not None else None)
            if not sc:
                continue
            ir = [len(s["tdi"] or s["tdo"]) for s in sc if s["type"] == "IR"]
            ir_len = max(set(ir), key=ir.count) if ir else None
            same = sum(x == ir_len for x in ir)
            ok01, n_ir = ir_tdo[tdo] if tdo is not None else (0, len(ir))
            # evidence in bits: the IR capture's 1 0 on TDO (2 a scan), an IR scan of the same length again (log2 of
            # the lengths it might have been, 3); the lines' changes kept to the shift states weigh it
            lines = [conc[i] for i in (tdi, tdo) if i is not None]
            kept = float(np.clip(min(lines) / JTAG_IN_SHIFT, 0, 1)) if lines else 0.0
            # and TDI's / TDO's changes kept to the shift states the walk gives (a line of another bus changes in the
            # others too): 1 a scan
            info = 2 * ok01 + 3 * max(0, same - 1) + len(sc) * kept
            known = float((st >= 0).mean())
            score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * kept * (1 - 0.5 ** (len(sc) / JTAG_SCANS)) * (0.5 + 0.5 * clk)
            roles = {"tck": sb.clock, "tms": sb.data[mi]}
            if tdi is not None:
                roles["tdi"] = sb.data[tdi]
            if tdo is not None:
                roles["tdo"] = sb.data[tdo]
            items = [{"s": s["s"], "type": s["type"], "bits": len(s["tdi"] or s["tdo"]),
                      **({"tdi": _value(s["tdi"])} if s["tdi"] else {}), **({"tdo": _value(s["tdo"])} if s["tdo"] else {})}
                     for s in sc]
            mets = {"scans": len(sc), "ir_scans": len(ir), "ir_bits": ir_len, "ir_same": same, "ir_capture_ok": ok01,
                    "in_shift": min(lines) if lines else 0.0, "known": known, "clk": clk,
                    "bit_coverage": known, "evidence_bits": float(info)}
            node = Node("jtag", {"sample_edge": sb.sample_edge, **({"ir_bits": ir_len} if ir_len else {})}, roles,
                        Stream("jtag.scans", items), mets, layer_score=score, total=score)
            if best is None or node.total > best.total:
                best = node
        return [best] if best is not None else []
