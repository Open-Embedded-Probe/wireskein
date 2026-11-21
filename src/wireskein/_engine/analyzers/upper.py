"""Upper-layer analyzers on byte streams and text lines.

They are both decoders (useful output) and evidence for the layer below: a
passing checksum on random bytes is very unlikely, so it supports the base
hypothesis strongly; plain text is weaker evidence.
"""

from __future__ import annotations

import re

import numpy as np

from ..stack import Stream

PRINTABLE = set(range(0x20, 0x7F)) | {0x09, 0x0A, 0x0D}


def _bytes(parent) -> np.ndarray:
    return np.asarray(parent.output.items["value"], dtype=np.int64)


class Lines:
    """Printable text split into lines."""
    name = "lines"
    consumes = "bytes"
    produces = "lines"

    def propose(self, ctx, parent):
        yield {}

    def run(self, ctx, parent, p):
        v = _bytes(parent)
        if len(v) == 0:
            return parent.roles, None, {"printable": 0.0}
        printable = np.isin(v, list(PRINTABLE))
        text = (np.asarray(v, np.int64) & 0xFF).astype(np.uint8).tobytes().decode("latin-1")
        lines = [l for l in re.split(r"\r?\n", text)]
        complete = lines[:-1]
        metrics = {
            "printable": float(printable.mean()),
            "newlines": int(np.sum(v == 0x0A)),
            "lines": len(complete),
            "bytes": int(len(v)),
            # words of letters: real text has them, random printable bytes rarely do
            "word_share": float(sum(len(w) for w in re.findall(r"[A-Za-z]{3,}", text)) / len(v)),
        }
        return parent.roles, Stream("lines", complete), metrics


class Nmea:
    name = "nmea"
    consumes = "bytes"
    produces = "messages"

    def propose(self, ctx, parent):
        yield {}

    def run(self, ctx, parent, p):
        v = _bytes(parent)
        text = (np.asarray(v, np.int64) & 0xFF).astype(np.uint8).tobytes().decode("latin-1")
        ok = bad = covered = 0
        msgs = []
        for m in re.finditer(r"\$([^$*\r\n]{1,80})\*([0-9A-Fa-f]{2})", text):
            body, cs = m.group(1), int(m.group(2), 16)
            x = 0
            for ch in body.encode("latin-1"):
                x ^= ch
            if x == cs:
                ok += 1
                covered += len(m.group(0))
                msgs.append(body)
            else:
                bad += 1
        return parent.roles, Stream("messages", msgs), {
            "checks_passed": ok, "checks_failed": bad, "coverage": covered / len(v) if len(v) else 0.0}


def crc16_running(data: bytes) -> list[int]:
    """CRC-16/Modbus of every prefix of data: [crc of data[:0], data[:1], ...]."""
    crc, out = 0xFFFF, [0xFFFF]
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
        out.append(crc)
    return out


def crc16_modbus(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


MODBUS_T15, MODBUS_T35 = 1.5, 3.5    # silences in characters: inside a message at most, between messages at least
MODBUS_JUNK = 2                      # bytes after a message's CRC left as junk (a driver turning off)
MODBUS_RESTARTS = 2                  # starts tried again after junk, in a chunk at most
MODBUS_MAX = 256                     # bytes a message at most (address, PDU of 253, CRC)


class ModbusRtu:
    """Frames split on >= 3.5 character times of silence, CRC-16 checked."""
    name = "modbus_rtu"
    consumes = "bytes"
    produces = "messages"

    def propose(self, ctx, parent):
        if "char_samples" in parent.output.meta:
            yield {}

    def run(self, ctx, parent, p):
        it = parent.output.items
        v, st = np.asarray(it["value"]), np.asarray(it["start"])
        if len(v) < 4:
            return parent.roles, None, {"checks_passed": 0}
        cs = parent.output.meta["char_samples"]
        # a message's characters follow with 1.5 characters of silence at most, messages with 3.5 or more: a chunk
        # between 3.5's may still hold a request and its answer (a slave may answer sooner: sigrok-dumps' wizmodbus
        # after 2.8) and a byte or two after a message (an RS-485 driver's turn-off). In each chunk one CRC run from
        # a message's start: it ends at a 1.5 character silence (or the chunk's end) where the two bytes before are
        # the CRC so far, junk of a byte or two after it left out; where none does, the rest of the chunk is none
        vb = (np.asarray(v, np.int64) & 0xFF).astype(np.uint8).tobytes()
        gaps = np.diff(st)
        chunk_cut = np.flatnonzero(gaps > (1 + MODBUS_T35) * cs) + 1
        piece_cut = np.flatnonzero(gaps > (1 + MODBUS_T15) * cs) + 1
        ok = bad = covered = 0
        msgs = []
        for c0, c1 in zip(np.concatenate(([0], chunk_cut)).tolist(), np.concatenate((chunk_cut, [len(v)])).tolist()):
            pos, restarts = c0, 0
            while c1 - pos >= 4:
                top = min(c1, pos + MODBUS_MAX + MODBUS_JUNK)
                a_, b_ = np.searchsorted(piece_cut, [pos + 1, top + 1])
                ends = piece_cut[a_:b_].tolist() + ([c1] if c1 <= top else [])
                if not ends:
                    break
                crcs = crc16_running(vb[pos:ends[-1]])
                found = None
                for e in ends:
                    for junk in range(MODBUS_JUNK + 1):
                        m = e - junk - pos
                        if m >= 4 and crcs[m - 2] == vb[pos + m - 2] | (vb[pos + m - 1] << 8):
                            found = (e, m)
                            break
                    if found:
                        break
                if not found:
                    # junk before a message (a character read where the line came on): start again at the next
                    # 1.5 character silence, twice at most in a chunk (a chunk of no Modbus would be tried at length)
                    nxt = [e for e in ends if e > pos and e < c1]
                    if nxt and restarts < MODBUS_RESTARTS:
                        pos, restarts = nxt[0], restarts + 1
                        continue
                    bad += 1
                    break
                e, m = found
                ok += 1
                covered += m
                msgs.append(vb[pos:pos + m].hex())
                pos = e
        return parent.roles, Stream("messages", msgs), {
            "checks_passed": ok, "checks_failed": bad, "coverage": covered / len(v)}


MARKER = re.compile(r"^(CASE_BEGIN|CASE_END|PHASE|INPUT|RESULT)\b")


class MarkerGrammar:
    """The I2CDeviceDB experiment marker lines (CASE_BEGIN/CASE_END/PHASE/...)."""
    name = "markers"
    consumes = "lines"
    produces = "markers"

    def propose(self, ctx, parent):
        yield {}

    def run(self, ctx, parent, p):
        lines = parent.output.items
        hits = [MARKER.match(l.strip()) for l in lines]
        kinds = [h.group(1) for h in hits if h]
        depth, balanced = 0, True
        for k in kinds:
            if k == "CASE_BEGIN":
                depth += 1
            elif k == "CASE_END":
                depth -= 1
                balanced &= depth >= 0
        return parent.roles, Stream("markers", kinds), {
            "matched": len(kinds), "match_rate": len(kinds) / len(lines) if lines else 0.0,
            "balanced": float(balanced and depth in (0, 1) and "CASE_BEGIN" in kinds)}
