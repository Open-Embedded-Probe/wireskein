"""USB low speed (1.5 Mb/s) and full speed (12 Mb/s) on D+ and D-: the pair's states J, K and SE0 (both low), packets
from the first K after idle (J) to an end of packet (SE0), NRZI (a change is 0), a 0 stuffed after six 1s, SYNC,
the PID with its complement, and the packet's fields: a token's address, endpoint and CRC5, a start of frame's
number, data with its CRC16, a handshake. Full speed idles with D+ high, low speed with D- high: the idle state tells
which line is D+ once the speed is known (by the bit time)."""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

FS, LS = 12e6, 1.5e6
PIDS = {0x1: "OUT", 0x9: "IN", 0x5: "SOF", 0xD: "SETUP", 0x3: "DATA0", 0xB: "DATA1", 0x7: "DATA2", 0xF: "MDATA",
        0x2: "ACK", 0xA: "NAK", 0xE: "STALL", 0x6: "NYET", 0xC: "PRE", 0x8: "SPLIT", 0x4: "PING"}
TOKENS = {"OUT", "IN", "SETUP", "PING"}
DATA = {"DATA0", "DATA1", "DATA2", "MDATA"}
USB_MIN_SAMPLES = 2.0          # samples a bit at least
USB_SKEW = 0.4                 # an SE0 / both high shorter than this share of a bit: the pair's skew in a change
USB_COUNTS = 0.25              # the line with fewer changes has this share of the other's at least
USB_OPPOSITE = 0.6             # a pair is at opposite levels this share of the time (resets and a detached
                               # device hold both low or both high a while) ...
USB_SKEW_S = 150e-9            # D+ and D- changing this far apart still change together
USB_TOGETHER = 0.8             # ... and this share of each line's changes is within a sample of the other's
USB_PACKETS = 4                # packets for half the evidence of their count (q)
EVIDENCE_BITS = 8


def crc5(bits: list[int]) -> int:
    """CRC5 of the bits as sent (LSB first): x^5 + x^2 + 1, register all 1s, the residual check left to the caller:
    the value to send (inverted, LSB first as a number)."""
    reg = 0x1F
    for b in bits:
        fb = (reg >> 4) ^ b
        reg = ((reg << 1) & 0x1F) ^ (0x05 if fb else 0)
    reg ^= 0x1F
    return int("".join(str((reg >> (4 - i)) & 1) for i in range(5))[::-1], 2)


def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for x in data:
        crc ^= x
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc ^ 0xFFFF


def _states(dp, dm, n: int):
    """(ticks where the pair's state changes, the state from each: 'J'/'K' as D+ high / low, 'S' SE0, 'X' both
    high) with D+ / D- as given: J and K here are full speed's (J = D+ high)."""
    cuts = np.unique(np.concatenate([[0], np.asarray(dp.edges, np.int64), np.asarray(dm.edges, np.int64)]))
    a, b = dp.level_at(cuts), dm.level_at(cuts)
    st = np.where((a == 1) & (b == 0), 1, np.where((a == 0) & (b == 1), 2, np.where((a == 0) & (b == 0), 0, 3)))
    keep = np.concatenate([[True], st[1:] != st[:-1]])
    return cuts[keep], st[keep]


def _deskew(t: np.ndarray, st: np.ndarray, n: int, u: float):
    """The pair's states with the moments of a change's skew left out: an SE0 or both-high shorter than USB_SKEW of
    a bit (D+ and D- crossing a sample apart) joins the state after it."""
    end = np.append(t[1:], n)
    short = ((st == 0) | (st == 3)) & (end - t < USB_SKEW * u)
    t, st = t[~short], st[~short]
    keep = np.concatenate([[True], st[1:] != st[:-1]])
    return t[keep], st[keep]


def _bits(dur: list[float], u: float) -> list[int] | None:
    """NRZI bits of the runs of J / K (in ticks) of one packet, bit-unstuffed: each run starts with a 0 (a change)
    and goes on with 1s; None on a stuffing error (seven 1s)."""
    raw = []
    for d in dur:
        k = max(1, int(round(d / u)))
        raw += [0] + [1] * (k - 1)
    out, ones = [], 0
    for b in raw:
        if ones == 6:
            if b != 0:
                return None
            ones = 0
            continue
        out.append(b)
        ones = ones + 1 if b else 0
    return out


def _value(bits: list[int]) -> int:
    return sum(b << i for i, b in enumerate(bits))


def packets(dp, dm, n: int, rate: float, speed: float, states=None) -> list[dict]:
    """The packets on a pair at `speed` with dp / dm as given (full speed's J: D+ high; low speed's: D- high);
    `states` the pair's (_states) when known."""
    u = rate / speed
    t, st = _deskew(*(states if states is not None else _states(dp, dm, n)), n, u)
    idle = 1 if speed == FS else 2                  # J
    out = []
    i = 0
    m = len(st)
    while i < m - 1:
        # a packet starts where the idle J turns to K
        if not (st[i] == idle and st[i + 1] == 3 - idle):
            i += 1
            continue
        j = i + 1
        dur = []
        while j < m and st[j] in (1, 2):
            end = t[j + 1] if j + 1 < m else n
            dur.append(float(end - t[j]))
            j += 1
        s0 = int(t[i + 1])
        if j >= m or st[j] != 0:
            i = j
            continue
        e = int(t[j])
        # SE0 for 1-3 bits: an end of packet; much longer is a reset
        se0 = float((t[j + 1] if j + 1 < m else n) - t[j])
        i = j
        if se0 > 4 * u * (8 if speed == FS else 1) or len(dur) < 2:
            continue
        bits = _bits(dur, u)
        if bits is None or len(bits) < 16:
            out.append({"s": s0, "e": e, "ok": False, "error": "stuff"})
            continue
        sync, pid_bits, rest = bits[:8], bits[8:16], bits[16:]
        # (the last bit before SE0 may be cut short: a dribble bit dropped as receivers do)
        if sync != [0, 0, 0, 0, 0, 0, 0, 1]:
            out.append({"s": s0, "e": e, "ok": False, "error": "sync"})
            continue
        pid = _value(pid_bits[:4])
        if _value(pid_bits[4:]) != pid ^ 0xF or pid not in PIDS:
            out.append({"s": s0, "e": e, "ok": False, "error": "pid"})
            continue
        name = PIDS[pid]
        row = {"s": s0, "e": e, "pid": name}
        if name in TOKENS or name == "SOF":
            rest = rest[:16]
            if len(rest) < 16:
                row.update(ok=False, error="short")
            else:
                ok = crc5(rest[:11]) == _value(rest[11:16])
                if name == "SOF":
                    row["frame"] = _value(rest[:11])
                else:
                    row.update(addr=_value(rest[:7]), endp=_value(rest[7:11]))
                row["ok"] = ok
        elif name in DATA:
            nbytes = len(rest) // 8
            data = bytes(_value(rest[8 * k:8 * k + 8]) for k in range(nbytes))
            if nbytes < 2:
                row.update(ok=False, error="short")
            else:
                row.update(data=data[:-2].hex(), ok=crc16(data[:-2]) == int.from_bytes(data[-2:], "little"))
        else:
            row["ok"] = True
        out.append(row)
    return out


REQUESTS = {0: "GET_STATUS", 1: "CLEAR_FEATURE", 3: "SET_FEATURE", 5: "SET_ADDRESS", 6: "GET_DESCRIPTOR",
            7: "SET_DESCRIPTOR", 8: "GET_CONFIGURATION", 9: "SET_CONFIGURATION", 10: "GET_INTERFACE",
            11: "SET_INTERFACE", 12: "SYNCH_FRAME"}
HANDSHAKES = {"ACK", "NAK", "STALL", "NYET"}


def setup_request(data: bytes) -> dict:
    """A SETUP's eight bytes as their fields (USB 2.0 9.3); a standard request named."""
    rt, req = data[0], data[1]
    out = {"bmRequestType": rt, "bRequest": req, "wValue": int.from_bytes(data[2:4], "little"),
           "wIndex": int.from_bytes(data[4:6], "little"), "wLength": int.from_bytes(data[6:8], "little"),
           "direction": "in" if rt & 0x80 else "out", "type": ("standard", "class", "vendor", "reserved")[(rt >> 5) & 3]}
    if out["type"] == "standard" and req in REQUESTS:
        out["request"] = REQUESTS[req]
    return out


def transactions(pk: list[dict]) -> list[dict]:
    """Packets grouped as the host's transactions: a token (IN / OUT / SETUP / PING), its data packet if any, the
    handshake ending it; a SOF alone. A packet that failed its checks ends the transaction it is in."""
    out, cur = [], None

    def close():
        nonlocal cur
        if cur is not None:
            out.append(cur)
        cur = None
    for p in pk:
        pid = p.get("pid")
        if not p.get("ok"):
            close()
            continue
        if pid == "SOF":
            close()
            out.append({"s": p["s"], "e": p["e"], "token": "SOF", "frame": p["frame"]})
        elif pid in TOKENS:
            close()
            cur = {"s": p["s"], "e": p["e"], "token": pid, "addr": p["addr"], "endp": p["endp"]}
        elif pid in DATA and cur is not None and "data" not in cur:
            cur.update(e=p["e"], data_pid=pid, data=p["data"])
            if cur["token"] == "SETUP" and len(p["data"]) == 16:
                cur["setup"] = setup_request(bytes.fromhex(p["data"]))
        elif pid in HANDSHAKES and cur is not None:
            cur.update(e=p["e"], handshake=pid)
            close()
        else:
            close()
    close()
    return out


def _pair_opposite(a, b, n: int) -> float:
    """The share of the time two lines are at opposite levels, from their levels at 4096 ticks across the capture
    (sorting both lines' changes together is what this saves)."""
    x = np.linspace(0, max(0, n - 1), 4096).astype(np.int64)
    return float(np.mean(a.level_at(x) != b.level_at(x)))


def usb_pair(a, b, n: int, rate: float = 0.0) -> bool:
    """Two lines of a differential pair: opposite most of the time, changing together (skewed by a sample or two)."""
    ea, eb = np.asarray(a.edges, np.int64), np.asarray(b.edges, np.int64)
    if len(ea) < 16 or len(eb) < 16:
        return False
    # (low speed's edges are slow: D+ and D- cross up to tens of nanoseconds apart)
    tick = max(2 * max(a.step, b.step, 1), USB_SKEW_S * rate)

    def near(x, y):
        i = np.clip(np.searchsorted(y, x), 1, len(y) - 1)
        return float(np.mean(np.minimum(np.abs(x - y[i - 1]), np.abs(y[i] - x)) <= tick))
    # the line with fewer changes keeps to the other (low speed's keep-alives are SE0s: D- alone changes); that
    # (a search) first, the levels' time (a sort of both lines' changes) only for a pair that passes it
    few, many = (ea, eb) if len(ea) <= len(eb) else (eb, ea)
    # (a fast clock has an edge near any line's: the pair's counts are of a size)
    if len(few) < USB_COUNTS * len(many):
        return False
    return near(few, many) >= USB_TOGETHER and _pair_opposite(a, b, n) >= USB_OPPOSITE


def read_pair(a, b, n: int, rate: float):
    """The best reading of two lines as a USB pair: (packets, speed, dp name, dm name) or None."""
    if not usb_pair(a, b, n, rate):
        return None
    best = None
    t, st = _states(a, b, n)
    swapped = np.where(st == 1, 2, np.where(st == 2, 1, st))       # (J and K the other way round)
    for speed in (FS, LS):
        if rate / speed < USB_MIN_SAMPLES:
            continue
        for dp, dm, sts in ((a, b, (t, st)), (b, a, (t, swapped))):
            pk = packets(dp, dm, n, rate, speed, sts)
            good = [p for p in pk if p.get("ok")]
            if best is None or len(good) > best[0]:
                best = (len(good), pk, speed, dp.name, dm.name)
    if best is None or best[0] == 0:
        return None
    return best[1], best[2], best[3], best[4]


class UsbPlugin:
    """USB low / full speed on two lines found together (the survey's same-change pairs, opposite at rest)."""
    name = "usb"

    def run(self, c, a: str, b: str):
        cap = c.cap
        r = read_pair(cap.channel(a), cap.channel(b), cap.n_samples, cap.rate)
        if r is None:
            return []
        pk, speed, dp, dm = r
        good = [p for p in pk if p.get("ok")]
        # evidence in bits: each packet's PID check (4), a token's or SOF's CRC5 (5), data's CRC16 (16); a packet
        # sent again counts once
        seen, info = set(), 0
        for p in good:
            key = (p.get("pid"), p.get("addr"), p.get("endp"), p.get("frame"), p.get("data"))
            if key in seen:
                continue
            seen.add(key)
            info += 4 + (5 if "frame" in p or "addr" in p else 0) + (16 if "data" in p else 0)
        share = len(good) / max(1, len(pk))
        score = (1 - 0.5 ** (info / EVIDENCE_BITS)) * share * (1 - 0.5 ** (len(good) / USB_PACKETS))
        params = {"speed": "full" if speed == FS else "low", "bitrate": speed}
        m = {"packets": len(pk), "ok": len(good), "evidence_bits": float(info), "share": share}
        return [Node("usb", params, {"dp": dp, "dm": dm}, Stream("usb.packets", pk), m,
                     layer_score=score, total=score)]
