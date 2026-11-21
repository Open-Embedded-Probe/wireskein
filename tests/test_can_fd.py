"""CAN FD (ISO 11898-1:2015) read by analyze: FD frames (FDF recessive) beside classic ones, the data phase at a
switched rate (BRS), data of up to 64 bytes, the stuff count and CRC-17 / CRC-21. The lines are made by an encoder
written here, apart from the reader (the CRCs as registers of bits, the stuffing from the bits as sent)."""

import numpy as np

from wireskein import decode
from wireskein._engine.model import Capture, Channel

RATE = 80e6
LENGTHS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64]


def bits_of(v, n):
    return [(v >> (n - 1 - i)) & 1 for i in range(n)]


def crc_reg(bits, width, taps):
    """A CRC register of `width` bits started as 1 then zeros (ISO's CAN FD), its feedback into `taps` (the
    polynomial's terms below the top), the value read out MSB first."""
    reg = [1] + [0] * (width - 1)
    for b in bits:
        fb = b ^ reg[0]
        reg = reg[1:] + [0]
        if fb:
            for t in taps:
                reg[width - 1 - t] ^= 1
    return reg


CRC17 = [16, 14, 13, 11, 6, 4, 3, 1, 0]                       # x^17 + x^16 + x^14 + x^13 + x^11 + x^6 + x^4 + x^3 + x + 1
CRC21 = [20, 13, 11, 7, 4, 3, 0]                              # x^21 + x^20 + x^13 + x^11 + x^7 + x^4 + x^3 + 1
CRC15 = [14, 10, 8, 7, 4, 3, 0]


def stuff(body):
    """Dynamic stuffing: after five equal bits their complement. (bits as sent, stuff bits inserted)"""
    out, run, last = [], 0, None
    for b in body:
        out.append(b)
        run = run + 1 if b == last else 1
        last = b
        if run == 5:
            out.append(1 - b)
            last, run = 1 - b, 1
    return out, len(out) - len(body)


def fd_frame(ident, data, brs=True, ext=None):
    """(bits before the switch, bits at the data rate, bits after it): SOF to BRS, ESI to the CRC delimiter, ACK on."""
    ext = ident > 0x7FF if ext is None else ext
    dlc = LENGTHS.index(len(data))
    if ext:
        head = [0] + bits_of(ident >> 18, 11) + [1, 1] + bits_of(ident & 0x3FFFF, 18) + [0, 1, 0, int(brs)]
    else:
        head = [0] + bits_of(ident, 11) + [0, 0, 1, 0, int(brs)]   # RRS, IDE, FDF, res, BRS
    tail = [0] + bits_of(dlc, 4) + [b for x in data for b in bits_of(x, 8)]   # ESI, DLC, data
    sent, n_stuff = stuff(head + tail)
    hs, _ = stuff(head)
    # BRS is head's last bit: a stuff bit after it (its five equal bits ending at BRS) is the data rate's
    split = len(hs) - (1 if len(hs) >= 6 and len(set(hs[-6:-1])) == 1 and hs[-1] != hs[-2] else 0)
    gray = [(0, 0, 0), (0, 0, 1), (0, 1, 1), (0, 1, 0), (1, 1, 0), (1, 1, 1), (1, 0, 1), (1, 0, 0)][n_stuff % 8]
    sc = list(gray) + [sum(gray) & 1]
    width, taps = (17, CRC17) if len(data) <= 16 else (21, CRC21)
    crc = crc_reg(sent + sc, width, taps)
    field = []
    prev = sent[-1]
    seq = sc + crc
    for i, b in enumerate(seq):
        if i == 0 or (i >= 4 and (i - 4) % 4 == 0):
            field.append(1 - prev)                                  # a fixed stuff bit
            prev = 1 - prev
        field.append(b)
        prev = b
    fast = sent[split:] + field + [1]                               # ... the CRC delimiter
    return sent[:split], fast, [0, 1] + [1] * 7                     # ACK, its delimiter, EOF


def classic_frame(ident, data):
    body = [0] + bits_of(ident, 11) + [0, 0, 0] + bits_of(len(data), 4) + [b for x in data for b in bits_of(x, 8)]
    crc = crc_reg(body, 15, CRC15)
    # (the classic CRC starts at 0: recompute so)
    reg = [0] * 15
    for b in body:
        fb = b ^ reg[0]
        reg = reg[1:] + [0]
        if fb:
            for t in CRC15:
                reg[14 - t] ^= 1
    crc = reg
    sent, _ = stuff(body + crc)
    return sent + [1, 0, 1] + [1] * 7


class Line:
    def __init__(self, nominal, data_rate, sp=0.75):
        self.tn, self.td, self.sp = 1 / nominal, 1 / data_rate, sp
        self.segs = [(1, 20 * self.tn)]

    def fd(self, ident, data, brs=True, ifs=10):
        slow, fast, after = fd_frame(ident, data, brs)
        if not brs:
            self.segs += [(b, self.tn) for b in slow + fast + after]
        else:
            # the rate changes at BRS's sample point and back at the CRC delimiter's
            self.segs += [(b, self.tn) for b in slow[:-1]] + [(slow[-1], self.sp * self.tn + (1 - self.sp) * self.td)]
            self.segs += [(b, self.td) for b in fast[:-1]] + [(fast[-1], self.sp * self.td + (1 - self.sp) * self.tn)]
            self.segs += [(b, self.tn) for b in after]
        self.segs += [(1, ifs * self.tn)]
        return self

    def classic(self, ident, data, ifs=10):
        self.segs += [(b, self.tn) for b in classic_frame(ident, data)] + [(1, ifs * self.tn)]
        return self

    def capture(self):
        t, edges, lvl = 0.0, [], 1
        for level, d in self.segs + [(1, 20 * self.tn)]:
            if level != lvl:
                edges.append(int(round(t * RATE)))
                lvl = level
            t += d
        return Capture(RATE, int(round(t * RATE)), [Channel("CAN", 1, np.array(edges, np.int64))])


def _claims(cap):
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    return [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]


def test_fd_frames_beside_classic_ones():
    rng = np.random.default_rng(7)
    want = []
    ln = Line(500e3, 2e6)
    for k in range(8):
        n = [0, 3, 8, 12, 20, 64, 16, 48][k]
        data = bytes(rng.integers(0, 256, n).tolist())
        ident = int(rng.integers(0, 0x7FF)) if k % 3 else int(rng.integers(0x800, 0x1FFFFFFF))
        ln.fd(ident, data, brs=k != 2)
        want.append((ident, data.hex(), True))
        cid = int(rng.integers(0, 0x7FF))
        cdata = bytes(rng.integers(0, 256, 4).tolist())
        ln.classic(cid, cdata)
        want.append((cid, cdata.hex(), False))
    (c,) = _claims(ln.capture())
    assert c["protocol"] == "can" and c["verdict"] == "confirmed"
    assert [(f["id"], f["data"], f.get("fd", False)) for f in c["result"]["frames"]] == want
    assert {f.get("data_ratio") for f in c["result"]["frames"] if f.get("brs")} == {4}


def test_data_phase_at_eight_times():
    rng = np.random.default_rng(8)
    ln = Line(250e3, 2e6)
    frames = [(int(rng.integers(0, 0x7FF)), bytes(rng.integers(0, 256, 32).tolist())) for _ in range(4)]
    for i, d in frames:
        ln.fd(i, d)
    (c,) = _claims(ln.capture())
    assert [(f["id"], f["data"]) for f in c["result"]["frames"]] == [(i, d.hex()) for i, d in frames]
    assert {f.get("data_ratio") for f in c["result"]["frames"]} == {8}


def test_classic_alone_is_the_declared_reading():
    rng = np.random.default_rng(9)
    ln = Line(500e3, 2e6)
    for _ in range(6):
        ln.classic(int(rng.integers(0, 0x7FF)), bytes(rng.integers(0, 256, 6).tolist()))
    (c,) = _claims(ln.capture())
    assert c["protocol"] == "can" and not c["params"].get("fd")
