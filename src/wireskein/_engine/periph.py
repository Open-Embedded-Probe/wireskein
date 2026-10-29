"""Peripherals read from their lines for the checks: LIN (break, sync, identifier, checksum), I2S (format, sample
rate, samples), a quadrature encoder (direction and count)."""

from __future__ import annotations

import numpy as np

from .gen import lin_checksum, lin_pid
from .model import Channel


# ---------------- LIN ----------------

def lin(ch: Channel, n: int, rate: float, baud: float, min_break_bits: float = 11.0) -> dict:
    """LIN frames on a line idling high: a low of at least min_break_bits at baud is a break, the high after it the
    delimiter; then characters (8N1) up to the next break or a pause of more than 2 characters: sync, protected
    identifier, data, checksum (the last). Each frame's bit time is measured from its sync field (0x55: its five
    falling edges span 8 bits), as a slave synchronizes, and its characters are read at that rate."""
    u0 = rate / baud
    st, ln, lv = ch.runs(n)
    out = {"frames": []}
    brk = np.flatnonzero((lv == 0) & (ln >= min_break_bits * u0))
    starts = st[brk]
    e = np.asarray(ch.edges, np.int64)
    after = ch.level_at(e)
    falls = e[after == 0]
    for k, i in enumerate(brk):
        if i == 0 or i + 1 >= len(st):
            continue                                   # cut by the window
        b_end = int(st[i] + ln[i])
        nxt = int(starts[k + 1]) if k + 1 < len(starts) else n
        f = {"t": int(st[i]), "break": int(ln[i]), "delimiter": int(ln[i + 1])}
        fl = falls[(falls > b_end) & (falls < nxt)]
        if len(fl) >= 5 and fl[4] - fl[0] < 12 * u0:
            u = (fl[4] - fl[0]) / 8
        else:
            u = u0
        f["bit_ticks"] = float(u)
        chars, t = [], b_end
        while True:
            j = np.searchsorted(fl, t)
            if j >= len(fl) or (chars and fl[j] - t > 20 * u):
                break
            s0 = int(fl[j])
            if s0 + 10 * u > nxt:
                break
            b = ch.level_at((s0 + (np.arange(10) + 0.5) * u).astype(np.int64)).astype(int)
            chars.append({"t": s0, "value": int(sum(int(v) << k2 for k2, v in enumerate(b[1:9]))),
                          "framing": bool(b[0] == 0 and b[9] == 1)})
            t = s0 + 9.5 * u
        f["chars"] = chars
        if len(chars) >= 2:
            v = [c["value"] for c in chars]
            f["sync"] = v[0]
            f["pid"] = v[1]
            f["id"] = v[1] & 0x3F
            f["pid_ok"] = lin_pid(v[1] & 0x3F) == v[1]
            if len(v) >= 3:
                data, cs = bytes(v[2:-1]), v[-1]
                f["data"] = data.hex()
                f["checksum"] = cs
                f["checksum_model"] = ("enhanced" if lin_checksum(data, v[1]) == cs else
                                       "classic" if lin_checksum(data, None) == cs else None)
            f["framing_ok"] = all(c["framing"] for c in chars)
        out["frames"].append(f)
    return out


# ---------------- I2S ----------------

LEFT_WS = {"i2s": 0, "left": 1, "right": 1}       # the WS level of the left channel per format


def i2s(bclk: Channel, ws: Channel, sd: Channel, n: int, rate: float, fmt: str = "i2s", bits: int | None = None,
        left_ws: int | None = None) -> dict:
    """I2S-like audio: SD and WS read on BCLK's rising edges. A slot is the rising edges between WS changes;
    "i2s" (Philips) puts a word's MSB one BCLK after WS changes, "left" (left-justified) at the change, "right"
    (right-justified) ends the word at the slot's last bit (bits needed). Words are two's complement, MSB first,
    `bits` long (default: the slot). Frames are (left, right) from the first whole slot."""
    ce = np.asarray(bclk.edges, np.int64)
    r = ce[bclk.level_at(ce) == 1]
    out = {"frames": [], "slot_bits": None}
    if len(r) < 8:
        return out
    w = ws.level_at(r).astype(np.int8)
    d = sd.level_at(r).astype(np.int8)
    ch_at = np.flatnonzero(w[1:] != w[:-1]) + 1            # first rising edge with the new WS
    out["bclk_hz"] = rate / float(np.median(np.diff(r)))
    if len(ch_at) < 3:
        return out
    slot = int(np.median(np.diff(ch_at)))
    out["slot_bits"] = slot
    we = np.asarray(ws.edges, np.int64)
    if len(we) >= 3:
        lvl = ws.level_at(we)
        rises = we[lvl == 1]
        if len(rises) >= 2:
            out["fs_hz"] = rate / float(np.median(np.diff(rises)))
        # WS and SD should change while BCLK is low (after its falling edge), to be stable at the rising one
        out["ws_changes_bclk_low"] = float(np.mean(bclk.level_at(we) == 0))
    se = np.asarray(sd.edges, np.int64)
    if len(se):
        out["sd_changes_bclk_low"] = float(np.mean(bclk.level_at(se) == 0))
    nb = bits or slot
    left = LEFT_WS[fmt] if left_ws is None else left_ws
    words = []
    for a, b in zip(ch_at[:-1], ch_at[1:]):
        if fmt == "i2s":
            k0 = a + 1
        elif fmt == "left":
            k0 = a
        else:
            k0 = b - nb
        if k0 < a or k0 + nb > len(d) or (fmt != "right" and k0 + nb > b + (1 if fmt == "i2s" else 0)):
            continue
        v = 0
        for x in d[k0:k0 + nb]:
            v = (v << 1) | int(x)
        if v >= 1 << (nb - 1):
            v -= 1 << nb
        words.append((int(w[a]), int(r[a]), v))
    # pair a left word with the right word after it
    i = 0
    while i + 1 < len(words):
        if words[i][0] == left and words[i + 1][0] != left:
            out["frames"].append({"t": words[i][1], "left": words[i][2], "right": words[i + 1][2]})
            i += 2
        else:
            i += 1
    return out


# ---------------- quadrature ----------------

# (A, B) as a 2-bit state A << 1 | B; A leading B: 00 -> 10 -> 11 -> 01 -> 00 is forward
_STEP = {(0b00, 0b10): 1, (0b10, 0b11): 1, (0b11, 0b01): 1, (0b01, 0b00): 1,
         (0b10, 0b00): -1, (0b11, 0b10): -1, (0b01, 0b11): -1, (0b00, 0b01): -1}


def quadrature(a: Channel, b: Channel, n: int) -> dict:
    """A quadrature encoder's two lines: +1 for each step with A leading B, -1 for each with B leading, an error
    when both change on one tick (a step skipped: too fast for the capture, or a fault)."""
    t = np.unique(np.concatenate([np.asarray(a.edges, np.int64), np.asarray(b.edges, np.int64)]))
    t = t[(t > 0) & (t < n)]
    s = np.concatenate(([0], t))
    st = (a.level_at(s).astype(int) << 1) | b.level_at(s).astype(int)
    fwd = back = err = 0
    errs = []
    reversals = 0
    last = 0
    for k in range(1, len(st)):
        v = _STEP.get((int(st[k - 1]), int(st[k])))
        if v is None:
            err += 1
            if len(errs) < 16:
                errs.append(int(s[k]))
            continue
        if v > 0:
            fwd += 1
        else:
            back += 1
        if last and v != last:
            reversals += 1
        last = v
    iv = np.diff(t)
    return {"steps": fwd - back, "forward": fwd, "backward": back, "errors": err, "error_at": errs,
            "reversals": reversals, "min_step_ticks": int(iv.min()) if len(iv) else None}


# ---------------- SPI bytes, SD in SPI mode ----------------

def spi_bytes(clk: Channel, mosi: Channel, miso: Channel, n: int, mode: int, cs: Channel | None = None,
              cs_active: int = 0) -> list[tuple[int, int, int, int]]:
    """(tick, mosi, miso, window) per byte, MSB first: both lines read on the sampling edge of `mode` (rising for
    0 and 3) inside each CS window (without CS: everywhere), eight edges a byte from the window's start."""
    cpol, cpha = mode >> 1, mode & 1
    ce = np.asarray(clk.edges, np.int64)
    lv = clk.level_at(ce)
    # leading edge leaves cpol; CPHA 0 samples on it, CPHA 1 on the trailing one
    samp = ce[(lv != cpol) if cpha == 0 else (lv == cpol)]
    if cs is not None:
        e = np.asarray(cs.edges, np.int64)
        after = cs.level_at(e)
        on, off = e[after == cs_active], e[after != cs_active]
        wins = ([(0, int(off[0]) if len(off) else n)] if cs.initial == cs_active else [])
        for t in on:
            j = np.searchsorted(off, t, side="right")
            wins.append((int(t), int(off[j]) if j < len(off) else n))
    else:
        wins = [(0, n)]
    out = []
    for w, (a, b) in enumerate(wins):
        s = samp[(samp > a) & (samp < b)]
        mo, mi = mosi.level_at(s), miso.level_at(s)
        for k in range(0, len(s) - 7, 8):
            vo = vi = 0
            for j in range(8):
                vo, vi = vo << 1 | int(mo[k + j]), vi << 1 | int(mi[k + j])
            out.append((int(s[k]), vo, vi, w))
    return out


def crc7(data: bytes) -> int:
    c = 0
    for b in data:
        for k in range(7, -1, -1):
            bit = (b >> k) & 1
            top = (c >> 6) & 1
            c = (c << 1) & 0x7F
            if top ^ bit:
                c ^= 0x09
    return c


def crc16(data: bytes) -> int:
    c = 0
    for b in data:
        c ^= b << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021) if c & 0x8000 else c << 1
            c &= 0xFFFF
    return c


SD_EXTRA = {8: 4, 58: 4, 13: 1}                   # response bytes after R1: R7, R3, R2
SD_BUSY = {12, 28, 29, 38}                        # R1b
SD_READ = {17: None, 18: None, 9: 16, 10: 16}     # data blocks the card sends (None: the block length)
SD_AREAD = {13: 64, 22: 4, 51: 8}
SD_WRITE = {24: 0xFE, 25: 0xFC}


def sd_spi(rows: list[tuple[int, int, int, int]]) -> dict:
    """SD commands in SPI mode on (tick, mosi, miso, window) bytes: a command is 6 bytes from the host (01 and the
    index, a 32-bit argument, CRC7 and the end bit), its R1 the first byte from the card with bit 7 clear within 8
    bytes after it (NCR); R7 / R3 / R2 bytes follow; R1b then busy (0x00) bytes. After a read command the card's
    blocks (token 0xFE, data, CRC16) until CMD12; after a write command the host's blocks (0xFE / 0xFC, data,
    CRC16) each answered by a data response and busy, until 0xFD. An ACMD is the command after CMD55. idle_ok: MISO
    was 0xFF during the command and MOSI 0xFF from its end through the response."""
    mo = [r[1] for r in rows]
    mi = [r[2] for r in rows]
    out = {"commands": [], "unparsed": 0}
    blk = 512
    acmd_next = False
    i = 0
    N = len(rows)

    def read_block(j, size):
        """The card's block from j: (block, next index) or (None, j) when no token within reach."""
        k = j
        while k < N and mi[k] == 0xFF and k - j < 100000:
            if mo[k] & 0xC0 == 0x40:                       # the host gave up waiting: a new command
                return None, k
            k += 1
        if k >= N:
            return None, k
        if mi[k] != 0xFE:
            return {"t": rows[k][0], "token": mi[k], "error_token": True}, k + 1
        d = bytes(mi[k + 1:k + 1 + size])
        c = mi[k + 1 + size:k + 3 + size]
        b = {"t": rows[k][0], "token": 0xFE, "len": len(d), "data": d.hex(), "nac_bytes": k - j}
        if len(d) == size and len(c) == 2:
            b["crc_ok"] = crc16(d) == (c[0] << 8 | c[1])
        else:
            b["cut"] = True
        return b, k + 3 + size

    while i + 6 <= N:
        b0 = mo[i]
        if b0 & 0xC0 != 0x40:
            i += 1
            continue
        cmdb = bytes(mo[i:i + 6])
        c = {"t": rows[i][0], "cmd": b0 & 0x3F, "acmd": acmd_next, "arg": int.from_bytes(cmdb[1:5], "big"),
             "crc_ok": crc7(cmdb[:5]) == cmdb[5] >> 1, "end_bit": bool(cmdb[5] & 1)}
        acmd_next = False
        j = i + 6
        k = j
        while k < N and k - j < 9 and mi[k] & 0x80:
            k += 1
        if k >= N or mi[k] & 0x80:
            c["r1"] = None
            out["commands"].append(c)
            i = j
            continue
        c["ncr"] = k - j
        c["r1"] = mi[k]
        k += 1
        cmd = c["cmd"]
        extra = (1 if cmd == 13 else 0) if c["acmd"] else SD_EXTRA.get(cmd, 0)      # ACMD13: R2
        if extra:
            c["resp"] = bytes(mi[k:k + extra]).hex()
            k += extra
        # the card says nothing during the command, the host sends 0xFF while it waits and reads the response
        c["idle_ok"] = all(v == 0xFF for v in mi[i:i + 6]) and all(v == 0xFF for v in mo[j:k])
        if cmd in SD_BUSY and not c["acmd"]:
            b = k
            while k < N and mi[k] == 0x00:
                k += 1
            c["busy_bytes"] = k - b
        if cmd == 55 and not c["acmd"] and c["r1"] is not None and c["r1"] & 0xFE == 0:
            acmd_next = True
        if cmd == 16 and not c["acmd"] and c["r1"] == 0:
            blk = c["arg"]
        if c["r1"] == 0:
            size = (SD_AREAD.get(cmd) if c["acmd"] else
                    (SD_READ[cmd] or blk) if cmd in SD_READ else None)
            if size:
                c["blocks"] = []
                while True:
                    b, k2 = read_block(k, size)
                    if b is None:
                        k = k2
                        break
                    c["blocks"].append(b)
                    k = k2
                    if cmd != 18 or c["acmd"] or b.get("error_token"):
                        break
            elif cmd in SD_WRITE and not c["acmd"]:
                c["blocks"] = []
                while k < N:
                    while k < N and mo[k] == 0xFF:
                        k += 1
                    if k >= N or mo[k] not in (0xFE, 0xFC, 0xFD):
                        break
                    if mo[k] == 0xFD:                       # stop tran: busy follows
                        b = k + 1
                        k += 2
                        while k < N and mi[k] == 0x00:
                            k += 1
                        c["stop_busy_bytes"] = k - b
                        break
                    d = bytes(mo[k + 1:k + 1 + blk])
                    cr = mo[k + 1 + blk:k + 3 + blk]
                    b = {"t": rows[k][0], "token": mo[k], "len": len(d), "data": d.hex(),
                         "crc_ok": len(cr) == 2 and crc16(d) == (cr[0] << 8 | cr[1])}
                    k += 3 + blk
                    while k < N and mi[k] == 0xFF and mo[k] == 0xFF:
                        k += 1
                    if k < N:
                        b["data_response"] = mi[k]
                        k += 1
                        s0 = k
                        while k < N and mi[k] == 0x00:
                            k += 1
                        b["busy_bytes"] = k - s0
                    c["blocks"].append(b)
                    if cmd == 24 or (b.get("data_response", 0x05) & 0x1F) != 0x05:
                        break
        out["commands"].append(c)
        i = max(k, j)
    return out
