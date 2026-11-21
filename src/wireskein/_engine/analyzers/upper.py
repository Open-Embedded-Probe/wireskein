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
        text = bytes(int(x) & 0xFF for x in v).decode("latin-1")
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
        text = bytes(int(x) & 0xFF for x in v).decode("latin-1")
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


def crc16_modbus(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


MODBUS_T15, MODBUS_T35 = 1.5, 3.5    # silences in characters: inside a message at most, between messages at least
MODBUS_JUNK = 2                      # bytes after a message's CRC left as junk (a driver turning off)


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
        # a message's characters follow with 1.5 characters of silence at most: split there (a slave may answer
        # before the 3.5 the standard asks for: sigrok-dumps' wizmodbus answers after 2.8); pieces failing their CRC
        # are joined to the next where the silence is under 3.5, and a byte or two after a CRC that holds (an RS-485
        # driver's turn-off) are left as junk
        cut = np.flatnonzero(np.diff(st) > (1 + MODBUS_T15) * cs) + 1
        pieces = list(zip(np.concatenate(([0], cut)).tolist(), np.concatenate((cut, [len(v)])).tolist()))
        ok = bad = covered = 0
        msgs = []
        i = 0
        while i < len(pieces):
            found = None
            for j in range(i, len(pieces)):
                if j > i and st[pieces[j][0]] - st[pieces[j - 1][1] - 1] > (1 + MODBUS_T35) * cs:
                    break
                lo, hi = pieces[i][0], pieces[j][1]
                for junk in range(MODBUS_JUNK + 1):
                    if hi - junk - lo < 4:
                        break
                    m = bytes(int(x) & 0xFF for x in v[lo:hi - junk])
                    if crc16_modbus(m[:-2]) == int.from_bytes(m[-2:], "little"):
                        found = (j, m)
                        break
                if found:
                    break
            if found:
                j, m = found
                ok += 1
                covered += len(m)
                msgs.append(m.hex())
                i = j + 1
            else:
                if pieces[i][1] - pieces[i][0] >= 4:
                    bad += 1
                i += 1
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
