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

from .jtag import EVIDENCE_BITS, JTAG_CAPTURES, JTAG_SCANS, _value, scans, walk
from .stack import Node, Stream

ESC_DESELECT, ESC_SELECT, ESC_RESET = 4, 6, 8     # TMSC's toggles in one TCKC high: at least this many
OAC_OSCAN1 = 0b1100                               # the OAC naming OScan1 (sent LSB first: 0 0 1 1)
CJTAG_OTHER = 0.02                                # TCKC intervals with 2-3 TMSC changes, this share at most
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
    # an escape is ESC_SELECT or more of TMSC's changes with no TCKC edge among them: none, no cJTAG (found before
    # the per-rise counts)
    at = np.searchsorted(ke, me)
    if len(at) < ESC_SELECT or not np.any(at[ESC_SELECT - 1:] == at[:len(at) - ESC_SELECT + 1]):
        return None
    # outside escapes TMSC changes once between TCKC's edges at most: two or three there in more than a few of
    # them is another pair of lines (a fast line beside a slow one)
    per = np.bincount(at)
    if np.sum((per >= 2) & (per < ESC_DESELECT)) > CJTAG_OTHER * np.sum(per >= 1):
        return None
    toggles = np.searchsorted(me, hi_end, side="left") - np.searchsorted(me, rise, side="right")
    if not np.any((toggles >= ESC_SELECT) & (toggles < ESC_RESET)):
        return None
    bit = msc.level_at(rise).astype(np.int8)
    k = len(rise)
    esc = np.flatnonzero(toggles >= ESC_DESELECT)
    parts = []                                  # (t, tms, tdi, tdo, oscan1) arrays, in order
    escapes, acts = [], []
    mode, i = "standard", 0

    def run_to(stop):
        """The JTAG cycles of the rises i .. stop - 1 (no escape among them) in the current mode."""
        if stop <= i:
            return
        if mode == "standard":
            sl = slice(i, stop)
            z = np.zeros(stop - i, np.int8)
            parts.append((rise[sl], bit[sl], z, z, np.zeros(stop - i, bool)))
        elif mode == "oscan1":
            m3 = (stop - i) // 3 * 3              # whole JTAG cycles of three TCKC cycles
            if m3:
                tri = bit[i:i + m3].reshape(-1, 3)
                parts.append((rise[i:i + m3:3], tri[:, 1], (1 - tri[:, 0]).astype(np.int8), tri[:, 2],
                              np.ones(m3 // 3, bool)))

    for e in esc.tolist() + [k]:
        if e < i:
            continue
        run_to(e)
        if e == k:
            break
        tg = int(toggles[e])
        escapes.append((int(rise[e]), tg))
        i = e + 1
        if tg >= ESC_RESET:
            mode = "standard"
        elif tg >= ESC_SELECT:
            if e + OAC_BITS < k and not np.any(toggles[e + 1:e + 1 + OAC_BITS] >= ESC_DESELECT):
                bb = bit[e + 1:e + 1 + OAC_BITS].tolist()
                oac, ec, cp = (sum(x << j for j, x in enumerate(bb[q:q + 4])) for q in (0, 4, 8))
                acts.append((int(rise[e + 1]), oac, ec, cp))
                mode = "oscan1" if oac == OAC_OSCAN1 else "other"
                i = e + 1 + OAC_BITS
        else:
            mode = "offline"
    cat = (lambda j, dt: np.concatenate([p[j] for p in parts]).astype(dt) if parts else np.zeros(0, dt))
    t, tms, tdi, tdo, osc = cat(0, np.int64), cat(1, np.int8), cat(2, np.int8), cat(3, np.int8), cat(4, bool)
    return {"t": t, "tms": tms, "tdi": tdi, "tdo": tdo, "oscan1": osc, "escapes": escapes, "activations": acts}


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
            if ok01 < JTAG_CAPTURES:
                # as JTAG: without TDO's IR capture the walk explains little (another pair's start and stop
                # conditions read as escapes: RVSWD's, its "activations" of every code)
                continue
            # evidence in bits: an OAC of OScan1 after a selection escape (4), each IR capture's 1 0 (2), an IR
            # scan of the same length again (3)
            info = 4 * n_osc + 2 * ok01 + 3 * max(0, same - 1)
            # a probe activates OScan1 each time: activations of other codes are the line's own noise
            share = n_osc / max(1, len(r["activations"]))
            score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * (1 - 0.5 ** (len(sc) / JTAG_SCANS)) * share
            items = [{"s": s["s"], "type": s["type"], "bits": len(s["tdi"]), "tdi": _value(s["tdi"]),
                      "tdo": _value(s["tdo"])} for s in sc]
            mets = {"scans": len(sc), "ir_scans": len(ir), "ir_bits": ir_len, "ir_same": same, "ir_capture_ok": ok01,
                    "escapes": len(r["escapes"]), "activations": len(r["activations"]), "oscan1": n_osc,
                    "evidence_bits": float(info)}
            out.append(Node("jtag", {"sample_edge": "rise", "cjtag": "oscan1", **({"ir_bits": ir_len} if ir_len else {})},
                            {"tckc": sb.clock, "tmsc": d}, Stream("jtag.scans", items), mets,
                            layer_score=score, total=score))
        return sorted(out, key=lambda x: -x.total)[:1]
