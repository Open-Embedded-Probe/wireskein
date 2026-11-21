"""cJTAG (IEEE 1149.7) on two lines, TCKC and TMSC, in the OScan1 format: JTAG's TAP driven by bits on TMSC, three
TCKC cycles each JTAG cycle (nTDI, TMS, TDO, sampled on TCKC's rise; TDO driven by the target). Control is by
escapes: TMSC toggling while TCKC stays high, the count telling which (4-5 a deselection, 6-7 a selection, 8 or more
a reset to the standard four-wire mode, TMSC then TMS alone). A selection is followed by the online activation: 12
bits on TCKC (OAC, EC, CP, 4 each, LSB first); an OAC of OScan1 starts its cycles.

The TAP is walked as in jtag.py over every JTAG cycle (the four-wire cycles' TMS too), the scans read from the OScan1
cycles; the evidence is the OAC of OScan1 after a selection and, as for JTAG, each IR scan's capture (1 then 0 on
TDO) and IR scans of one length."""
from __future__ import annotations

import numpy as np

from .jtag import EVIDENCE_BITS, JTAG_SCANS, _value, scans, walk
from .stack import Node, Stream

ESC_DESELECT, ESC_SELECT, ESC_RESET = 4, 6, 8     # TMSC's toggles in one TCKC high: at least this many
OAC_OSCAN1 = 0b1100                               # the OAC naming OScan1 (sent LSB first: 0 0 1 1)
OAC_BITS = 12                                     # OAC, EC, CP


def read_cjtag(ckc, msc, n: int) -> dict | None:
    """The JTAG cycles of a cJTAG line pair: {"t", "tms", "tdi", "tdo", "oscan1" (the cycle is OScan1's),
    "escapes" [(tick, toggles)], "activations" [(tick, oac, ec, cp)]} or None without a selection escape."""
    ke = np.asarray(ckc.edges, np.int64)
    if len(ke) < 8:
        return None
    lv = ckc.level_at(ke)
    rise, fall = ke[lv == 1], ke[lv == 0]
    # each rise's high: to the next fall (the capture's end for the last)
    nxt = np.searchsorted(fall, rise, side="right")
    hi_end = np.where(nxt < len(fall), fall[np.minimum(nxt, len(fall) - 1)], n)
    me = np.asarray(msc.edges, np.int64)
    toggles = np.searchsorted(me, hi_end, side="left") - np.searchsorted(me, rise, side="right")
    if not np.any((toggles >= ESC_SELECT) & (toggles < ESC_RESET)):
        return None
    bit = msc.level_at(rise).astype(np.int8)
    t, tms, tdi, tdo, osc = [], [], [], [], []
    escapes, acts = [], []
    mode, i, k = "standard", 0, len(rise)
    while i < k:
        tg = int(toggles[i])
        if tg >= ESC_DESELECT:
            escapes.append((int(rise[i]), tg))
            if tg >= ESC_RESET:
                mode = "standard"
            elif tg >= ESC_SELECT:
                if i + OAC_BITS < k and not np.any(toggles[i + 1:i + 1 + OAC_BITS] >= ESC_DESELECT):
                    b = bit[i + 1:i + 1 + OAC_BITS].tolist()
                    oac, ec, cp = (sum(x << j for j, x in enumerate(b[q:q + 4])) for q in (0, 4, 8))
                    acts.append((int(rise[i + 1]), oac, ec, cp))
                    mode = "oscan1" if oac == OAC_OSCAN1 else "other"
                    i += 1 + OAC_BITS
                    continue
            else:
                mode = "offline"
            i += 1
            continue
        if mode == "standard":
            t.append(int(rise[i])); tms.append(int(bit[i])); tdi.append(0); tdo.append(0); osc.append(False)
            i += 1
        elif mode == "oscan1":
            # a JTAG cycle is three TCKC cycles with no escape among them
            if i + 2 >= k or np.any(toggles[i + 1:i + 3] >= ESC_DESELECT):
                i += 1
                continue
            t.append(int(rise[i])); tdi.append(1 - int(bit[i])); tms.append(int(bit[i + 1])); tdo.append(int(bit[i + 2]))
            osc.append(True)
            i += 3
        else:
            i += 1
    return {"t": np.asarray(t, np.int64), "tms": np.asarray(tms, np.int8), "tdi": np.asarray(tdi, np.int8),
            "tdo": np.asarray(tdo, np.int8), "oscan1": np.asarray(osc, bool), "escapes": escapes, "activations": acts}


class CjtagPlugin:
    """cJTAG's OScan1 on a clocked pair: TCKC the clock, TMSC its data line."""
    name = "jtag"
    consumes = ("frames.gap",)

    def run(self, c, fr, g):
        sb = fr.source
        out = []
        for d in sb.data:
            r = read_cjtag(c.cap.channel(sb.clock), c.cap.channel(d), c.cap.n_samples)
            if r is None or not r["oscan1"].any():
                continue
            st = walk(r["tms"])
            sc = [s for s in scans(st, r["t"], r["tdi"], r["tdo"])
                  if r["oscan1"][min(np.searchsorted(r["t"], s["s"]), len(r["t"]) - 1)]]
            if not sc:
                continue
            ir = [len(s["tdi"]) for s in sc if s["type"] == "IR"]
            ir_len = max(set(ir), key=ir.count) if ir else None
            same = sum(x == ir_len for x in ir)
            ok01 = sum(s["tdo"][:2] == [1, 0] for s in sc if s["type"] == "IR" and len(s["tdo"]) >= 2)
            n_osc = sum(oac == OAC_OSCAN1 for _, oac, _, _ in r["activations"])
            # evidence in bits: an OAC of OScan1 after a selection escape (4), each IR capture's 1 0 (2), an IR
            # scan of the same length again (3)
            info = 4 * n_osc + 2 * ok01 + 3 * max(0, same - 1)
            score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * (1 - 0.5 ** (len(sc) / JTAG_SCANS))
            items = [{"s": s["s"], "type": s["type"], "bits": len(s["tdi"]), "tdi": _value(s["tdi"]),
                      "tdo": _value(s["tdo"])} for s in sc]
            mets = {"scans": len(sc), "ir_scans": len(ir), "ir_bits": ir_len, "ir_same": same, "ir_capture_ok": ok01,
                    "escapes": len(r["escapes"]), "activations": len(r["activations"]), "oscan1": n_osc,
                    "evidence_bits": float(info)}
            out.append(Node("jtag", {"sample_edge": "rise", "cjtag": "oscan1", **({"ir_bits": ir_len} if ir_len else {})},
                            {"tckc": sb.clock, "tmsc": d}, Stream("jtag.scans", items), mets,
                            layer_score=score, total=score))
        return sorted(out, key=lambda x: -x.total)[:1]
