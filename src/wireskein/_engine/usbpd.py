"""USB Power Delivery on a CC line: biphase mark coding at about 300 kb/s (a change at each bit's start, another at
its middle for a 1), a preamble of 64 alternating bits, then 5-bit symbols (4b5b, LSB first): the start of packet's
ordered set (SOP, SOP', SOP'', or a reset's), the header (16 bits), the data objects (32 bits each, as many as the
header says), CRC32 and EOP. Bytes and words are little-endian."""
from __future__ import annotations

import zlib

import numpy as np

from .stack import Node, Stream

PD_RATE = (270e3, 330e3)            # the bit rate's range (300 kb/s nominal)
DATA_4B5B = {0b11110: 0x0, 0b01001: 0x1, 0b10100: 0x2, 0b10101: 0x3, 0b01010: 0x4, 0b01011: 0x5, 0b01110: 0x6,
             0b01111: 0x7, 0b10010: 0x8, 0b10011: 0x9, 0b10110: 0xA, 0b10111: 0xB, 0b11010: 0xC, 0b11011: 0xD,
             0b11100: 0xE, 0b11101: 0xF}
K_CODES = {0b11000: "S1", 0b10001: "S2", 0b00110: "S3", 0b00111: "R1", 0b11001: "R2", 0b01101: "EOP"}
ORDERED_SETS = {("S1", "S1", "S1", "S2"): "SOP", ("S1", "S1", "S3", "S3"): "SOP'", ("S1", "S3", "S1", "S3"): "SOP''",
                ("R1", "R1", "R1", "R2"): "Hard Reset", ("R1", "S1", "R1", "S3"): "Cable Reset"}
CONTROL = {1: "GoodCRC", 2: "GotoMin", 3: "Accept", 4: "Reject", 5: "Ping", 6: "PS_RDY", 7: "Get_Source_Cap",
           8: "Get_Sink_Cap", 9: "DR_Swap", 10: "PR_Swap", 11: "VCONN_Swap", 12: "Wait", 13: "Soft_Reset",
           16: "Not_Supported", 17: "Get_Source_Cap_Extended", 18: "Get_Status", 19: "FR_Swap", 20: "Get_PPS_Status",
           21: "Get_Country_Codes", 22: "Get_Sink_Cap_Extended"}
DATA = {1: "Source_Capabilities", 2: "Request", 3: "BIST", 4: "Sink_Capabilities", 5: "Battery_Status", 6: "Alert",
        7: "Get_Country_Info", 8: "Enter_USB", 15: "Vendor_Defined"}
EVIDENCE_BITS = 8


def _bits(edges: np.ndarray, rate: float) -> list[tuple[int, list[int]]]:
    """Bursts of BMC bits: [(the burst's first edge, bits)]. Intervals of a half bit pair into a 1, of a whole bit
    are a 0; a gap of several bits ends a burst."""
    out = []
    if len(edges) < 64:
        return out
    d = np.diff(edges).astype(float)
    ui = float(np.median(d[d > np.median(d) * 1.5])) if np.any(d > np.median(d) * 1.5) else float(np.median(d))
    if not (rate / PD_RATE[1] * 0.9 <= ui <= rate / PD_RATE[0] * 1.1):
        return out
    i, n = 0, len(d)
    while i < n:
        start, bits = int(round(edges[i])), []
        while i < n:
            x = d[i] / ui
            if 0.75 <= x <= 1.3:
                bits.append(0)
                i += 1
            elif 0.3 <= x < 0.75 and i + 1 < n and 0.3 <= d[i + 1] / ui < 0.75:
                bits.append(1)
                i += 2
            else:
                i += 1
                break
        if len(bits) >= 64:
            out.append((start, bits))
    return out


def _sym(bits: list[int], k: int) -> int:
    return sum(b << j for j, b in enumerate(bits[k:k + 5]))


def _unbias(edges: np.ndarray, initial: int) -> np.ndarray:
    """The edges with the line's duty bias taken out: BMC holds each level as long as the other on average, so a
    threshold off the middle (highs shorter, lows longer) shows as a difference of their means; the falls are moved
    by half of it."""
    if len(edges) < 8:
        return edges
    d = np.diff(edges).astype(float)
    lv = initial ^ ((np.arange(len(edges)) + 1) & 1)             # the level after each edge
    hi, lo = d[lv[:-1] == 1], d[lv[:-1] == 0]
    if not len(hi) or not len(lo):
        return edges
    # keep to runs inside packets (a half or a whole bit): the long idles out
    cap_ = 3 * float(np.median(d))
    lo, hi = lo[lo < cap_], hi[hi < cap_]
    if not len(lo) or not len(hi):
        return edges                               # (one level's runs all long: no BMC to balance)
    bias = (float(np.mean(lo)) - float(np.mean(hi))) / 2
    out = edges.astype(float)
    out[lv == 0] += bias                                          # a fall: a high's end
    return out


def packets(edges: np.ndarray, rate: float, initial: int = 0) -> list[dict]:
    out = []
    edges = _unbias(np.asarray(edges, np.int64), initial)
    for start, bits in _bits(edges, rate):
        # the preamble alternates 0 1 ...: the ordered set starts where that stops
        k = 0
        while k + 1 < len(bits) and bits[k] != bits[k + 1]:
            k += 1
        k = k + 1 if k % 2 == 0 else k            # (the preamble is an even 64 bits; its last bit is a 1)
        k = max(k, 1) - 1 if bits[k - 1:k] == [0] else k
        syms, j = [], k
        while j + 5 <= len(bits):
            v = _sym(bits, j)
            syms.append(K_CODES.get(v, DATA_4B5B.get(v)))
            j += 5
            if syms[-1] == "EOP":
                break
        # find the ordered set near the preamble's end (its boundary may be a bit off)
        row = None
        for off in range(-2, 3):
            jj = k + off
            if jj < 0 or jj + 20 > len(bits):
                continue
            os_ = tuple(K_CODES.get(_sym(bits, jj + 5 * q)) for q in range(4))
            if os_ in ORDERED_SETS:
                row = {"s": start, "sop": ORDERED_SETS[os_]}
                k = jj + 20
                break
        if row is None:
            continue
        if row["sop"] in ("Hard Reset", "Cable Reset"):
            row["ok"] = True
            out.append(row)
            continue
        nib, j = [], k
        while j + 5 <= len(bits):
            v = _sym(bits, j)
            j += 5
            if v in K_CODES:
                if K_CODES[v] == "EOP":
                    row["eop"] = True
                break
            if v not in DATA_4B5B:
                row["error"] = "symbol"
                break
            nib.append(DATA_4B5B[v])
        data = bytes(nib[q] | (nib[q + 1] << 4) for q in range(0, len(nib) - 1, 2))
        if len(data) < 6:
            row.update(ok=False, error=row.get("error", "short"))
            out.append(row)
            continue
        hdr = int.from_bytes(data[:2], "little")
        nobj = (hdr >> 12) & 7
        mtype = hdr & 0x1F
        body, crc = data[:2 + 4 * nobj], data[2 + 4 * nobj:6 + 4 * nobj]
        row.update(header=hdr, type=(DATA if nobj else CONTROL).get(mtype, f"type{mtype}"), id=(hdr >> 9) & 7,
                   objects=[int.from_bytes(body[2 + 4 * q:6 + 4 * q], "little") for q in range(nobj)],
                   power_role="source" if hdr & 0x100 else "sink", data_role="DFP" if hdr & 0x20 else "UFP",
                   spec_rev=((hdr >> 6) & 3) + 1)
        row["ok"] = len(crc) == 4 and zlib.crc32(body) == int.from_bytes(crc, "little") and bool(row.get("eop"))
        out.append(row)
    return out


class UsbPdPlugin:
    """USB Power Delivery on one CC line."""
    name = "usb_pd"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 1.2e6:                              # a half bit (1.7 us) needs two samples
            return []
        pk = packets(np.asarray(ch.edges, np.int64), rate, int(ch.initial))
        good = [p for p in pk if p.get("ok")]
        if len(good) < 2:
            return []
        # evidence: each packet's ordered set (4 K-codes: 8) and CRC32 (32)
        info = sum(8 + (32 if "header" in p else 0) for p in good)
        score = (len(good) / len(pk)) * (1 - 0.5 ** (info / EVIDENCE_BITS))
        return [Node("usb_pd", {}, {"cc": ch.name}, Stream("usb_pd.packets", pk),
                     {"packets": len(pk), "ok": len(good)}, layer_score=score, total=score)]
