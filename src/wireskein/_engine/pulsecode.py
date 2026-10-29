"""Protocols of one line told by pulse widths: RGB LEDs (WS2812 and its kind), 1-Wire, IR remote codes (NEC).

Each decoder takes a Channel and gives what the line carried with every width it read (ticks), so a check can hold
each against a datasheet's limits and the analyzer can score how well the line fits.
"""

from __future__ import annotations

import numpy as np

from .model import Channel


def pulses(ch: Channel, n: int, active: int) -> tuple[np.ndarray, np.ndarray]:
    """(start, width) of every whole pulse at `active` (one cut by the window's ends is left out)."""
    e = np.asarray(ch.edges, np.int64)
    after = ch.level_at(e)
    lead, trail = e[after == active], e[after != active]
    j = np.searchsorted(trail, lead, side="right")
    ok = j < len(trail)
    return lead[ok], trail[j[ok]] - lead[ok]


def _split(widths: np.ndarray, ratio: float = 1.3) -> float | None:
    """The width between two kinds of pulse: the largest ratio between neighbouring widths, if at least `ratio`."""
    u = np.unique(widths)
    if len(u) < 2:
        return None
    r = u[1:] / np.maximum(u[:-1], 1)
    k = int(np.argmax(r))
    return float(np.sqrt(u[k] * u[k + 1])) if r[k] >= ratio else None


def _bytes(bits: list[int], lsb_first: bool) -> tuple[bytes, int]:
    out = bytearray()
    for i in range(0, len(bits) - 7, 8):
        b = bits[i:i + 8]
        out.append(sum(v << (k if lsb_first else 7 - k) for k, v in enumerate(b)))
    return bytes(out), len(bits) % 8


# ---------------- RGB LEDs ----------------

def rgb_led(ch: Channel, n: int, rate: float) -> dict:
    """A WS2812-like line (idle low, a bit = a high of T0H or T1H then a low, MSB first): frames between lows of
    more than 5 bit periods. The split between 0 and 1 is the widest gap between the highs' widths; when every high
    is alike, a high under half the bit period is a 0."""
    st, w = pulses(ch, n, 1)
    out = {"frames": [], "t0h": [], "t1h": [], "period": [], "resets": [], "pauses": []}
    if len(st) < 2:
        return out
    per = np.diff(st)
    bit = float(np.median(per))
    split = _split(w)
    if split is None or split > bit:
        split = bit / 2
    one = w > split
    cut = np.flatnonzero(per > 5 * bit) + 1
    bounds = np.concatenate(([0], cut, [len(st)]))
    for a, b in zip(bounds[:-1], bounds[1:]):
        bits = one[a:b].astype(int).tolist()
        data, left = _bytes(bits, lsb_first=False)
        out["frames"].append({"t": int(st[a]), "bits": int(b - a), "data": data.hex(), "leftover_bits": left})
        for k in range(a, b):
            out["t1h" if one[k] else "t0h"].append((int(st[k]), int(w[k])))
            if k + 1 < b:
                out["period"].append((int(st[k]), int(st[k + 1] - st[k])))
                if per[k] > 2 * bit:
                    out["pauses"].append((int(st[k]), int(per[k])))
        end = int(st[b - 1] + w[b - 1])
        nxt = int(st[b]) if b < len(st) else None
        if nxt is not None:
            out["resets"].append((end, nxt - end))
        elif ch.level_at(np.array([n - 1]))[0] == 0:
            out["tail_low"] = (end, n - end)          # the low after the last frame, cut by the window
    out["split"] = split
    out["bit"] = bit
    return out


# ---------------- 1-Wire ----------------

# Maxim AN126 / DS18B20, seconds: reset low, presence wait (from release), presence low, slot (low start to the
# next one: tSLOT + tREC), recovery (high between slots), write-0 low, write-1 / read low
ONEWIRE = {
    "standard": {"reset": (480e-6, None), "wait": (15e-6, 60e-6), "presence": (60e-6, 240e-6),
                 "slot": (61e-6, None), "recovery": (1e-6, None), "low_0": (None, 120e-6), "low_1": (1e-6, 15e-6),
                 "split": 15e-6, "reset_detect": 288e-6},
    "overdrive": {"reset": (70e-6, 80e-6), "wait": (2e-6, 6e-6), "presence": (8e-6, 24e-6),
                  "slot": (7e-6, None), "recovery": (1e-6, None), "low_0": (None, 16e-6), "low_1": (1e-6, 2e-6),
                  "split": 2e-6, "reset_detect": 48e-6},
}


def onewire(ch: Channel, n: int, rate: float, speed: str = "standard") -> dict:
    """A 1-Wire line (idle high, every pulse low). A low of at least `reset_detect` is a reset; the next low that
    starts within the presence window after it is the presence pulse; every other low is a slot, 1 when shorter
    than `split` (write 1, or read 1), else 0 (write 0, or a read 0 held by the slave), LSB first. Who drove a low
    is not on the line: a read slot reads as the bit it carried."""
    sp = ONEWIRE[speed]
    st, w = pulses(ch, n, 0)
    out = {"resets": [], "low_0": [], "low_1": [], "slot": [], "recovery": [], "before_reset": 0}
    cur = None
    bits: list[int] = []
    last = None                                    # (start, width) of the previous slot

    def close():
        if cur is not None:
            cur["data"], cur["leftover_bits"] = (lambda d: (d[0].hex(), d[1]))(_bytes(bits, lsb_first=True))
            cur["bits"] = len(bits)

    for s, x in zip(st.tolist(), w.tolist()):
        if x >= sp["reset_detect"] * rate:
            close()
            cur = {"t": s, "reset": x, "presence": False}
            out["resets"].append(cur)
            bits, last = [], None
            continue
        if cur is None:
            out["before_reset"] += 1
            continue
        rel = cur["t"] + cur["reset"]
        if not cur["presence"] and not bits and s - rel <= 2 * sp["wait"][1] * rate and x >= sp["split"] * rate:
            cur.update(presence=True, wait=s - rel, presence_low=x)
            continue
        one = x < sp["split"] * rate
        bits.append(int(one))
        out["low_1" if one else "low_0"].append((s, x))
        if last is not None and s - last[0] < 4 * sp["slot"][0] * rate:
            out["slot"].append((last[0], s - last[0]))
            out["recovery"].append((last[0] + last[1], s - last[0] - last[1]))
        last = (s, x)
    close()
    return out


# ---------------- IR remote codes ----------------

NEC = {"leader": 9000e-6, "space": 4500e-6, "repeat_space": 2250e-6, "mark": 562.5e-6, "space_0": 562.5e-6,
       "space_1": 1687.5e-6}


def ir_marks(ch: Channel, n: int, rate: float) -> dict:
    """The marks of an IR line: a carrier's bursts merged (the line as the LED is driven: idle low), or the active
    lows of a receiver's output (idle high). A line is modulated when its active pulses are mostly shorter than
    100 us: bursts are pulses less than 150 us apart."""
    idle = int(ch.initial)
    st, w = pulses(ch, n, 1 - idle)
    out = {"idle": idle, "marks": np.zeros((0, 2), np.int64)}
    if len(st) == 0:
        return out
    if np.median(w) < 100e-6 * rate and len(st) > 4:
        gap = st[1:] - (st[:-1] + w[:-1])
        inside = gap <= 150e-6 * rate
        b = np.concatenate(([0], np.flatnonzero(~inside) + 1, [len(st)]))
        if inside.any():
            per = float(np.median(np.diff(st)[inside]))
            out["carrier_hz"] = rate / per
            out["carrier_duty"] = float(np.median(w[:-1][inside])) / per
        marks = np.stack([st[b[:-1]], st[b[1:] - 1] + w[b[1:] - 1]], 1)
    else:
        marks = np.stack([st, st + w], 1)
    out["marks"] = marks
    return out


def nec(ch: Channel, n: int, rate: float, tol: float = 0.25) -> dict:
    """NEC frames: a 9 ms mark, then 4.5 ms (32 bits follow: a 562.5 us mark, then 562.5 us for 0 or 1687.5 us for
    1, LSB first; address, inverted address or the address's high byte, command, inverted command) or 2.25 ms (a
    repeat). Marks and spaces are told apart within `tol` (relative) of their nominal length."""
    m = ir_marks(ch, n, rate)
    out = {k: v for k, v in m.items() if k != "marks"}
    out.update(frames=[], leader=[], space=[], repeat_space=[], mark=[], space_0=[], space_1=[])
    mk = m["marks"]

    def near(v, k):
        return abs(v / rate / NEC[k] - 1) <= tol

    i = 0
    while i < len(mk):
        s, e = int(mk[i, 0]), int(mk[i, 1])
        if not near(e - s, "leader") or i + 1 >= len(mk):
            i += 1
            continue
        sp = int(mk[i + 1, 0]) - e
        if near(sp, "repeat_space"):
            out["leader"].append((s, e - s))
            out["repeat_space"].append((e, sp))
            out["mark"].append((int(mk[i + 1, 0]), int(mk[i + 1, 1] - mk[i + 1, 0])))
            out["frames"].append({"t": s, "repeat": True})
            i += 2
            continue
        if not near(sp, "space"):
            i += 1
            continue
        out["leader"].append((s, e - s))
        out["space"].append((e, sp))
        bits, j = [], i + 1
        while j < len(mk) and len(bits) < 32:
            ms, me = int(mk[j, 0]), int(mk[j, 1])
            out["mark"].append((ms, me - ms))
            if j + 1 >= len(mk):
                break
            gap = int(mk[j + 1, 0]) - me
            if gap / rate > 2 * NEC["space_1"]:
                break
            one = gap / rate > (NEC["space_0"] + NEC["space_1"]) / 2
            out["space_1" if one else "space_0"].append((me, gap))
            bits.append(int(one))
            j += 1
        if len(bits) == 32 and j < len(mk):
            out["mark"].append((int(mk[j, 0]), int(mk[j, 1] - mk[j, 0])))          # the stop mark
        data, _ = _bytes(bits, lsb_first=True)
        f = {"t": s, "repeat": False, "bits": len(bits)}
        if len(data) == 4:
            a, na, c, nc = data
            ext = a ^ na != 0xFF
            f.update(addr=(a | na << 8) if ext else a, ext=ext, cmd=c, cmd_ok=c ^ nc == 0xFF)
        out["frames"].append(f)
        i = j + 1
    return out
