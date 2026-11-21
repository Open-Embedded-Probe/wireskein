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

RGB_WIDTH_SHARE = 0.02    # a high width taken for the split when this share of the pulses has it


def rgb_led(ch: Channel, n: int, rate: float) -> dict:
    """A WS2812-like line (idle low, a bit = a high of T0H or T1H then a low, MSB first): frames between lows of
    more than 5 bit periods. The split between 0 and 1 is the widest gap between the highs' widths; when every high
    is alike, every bit is one kind: a 1 when the highs are over half the bit period."""
    st, w = pulses(ch, n, 1)
    out = {"frames": [], "t0h": [], "t1h": [], "period": [], "resets": [], "pauses": []}
    if len(st) < 2:
        return out
    per = np.diff(st)
    bit = float(np.median(per))
    # the split from the widths most pulses have: a few leftovers of a noise (a couple of ticks) would put the widest
    # ratio below every real high, and every bit would read as a 1
    u, cnt = np.unique(w, return_counts=True)
    split = _split(u[cnt >= max(2, RGB_WIDTH_SHARE * len(w))])
    if split is None or split > bit:
        # every high alike: all one kind of bit, a 1 when the highs are over half the period (not split at half: a
        # clock's highs of half the period would fall either side of it, and read as varied bits)
        split = 0.0 if len(w) and float(np.median(w)) > bit / 2 else float(bit)
    one = w > split
    cut = np.flatnonzero(per > 5 * bit) + 1
    bounds = np.concatenate(([0], cut, [len(st)]))
    # each pulse's high, and the period to the next one inside its frame: made at once (in pulse order, as a walk
    # over the frames gives them)
    st_l, w_l = st.tolist(), w.tolist()
    out["t1h"] = [(st_l[k], w_l[k]) for k in np.flatnonzero(one).tolist()]
    out["t0h"] = [(st_l[k], w_l[k]) for k in np.flatnonzero(~one).tolist()]
    inner = np.ones(len(st) - 1, bool)
    inner[bounds[1:-1] - 1] = False                       # the last pulse of a frame has no period in it
    per_l = per.tolist()
    out["period"] = [(st_l[k], per_l[k]) for k in np.flatnonzero(inner).tolist()]
    out["pauses"] = [(st_l[k], per_l[k]) for k in np.flatnonzero(inner & (per > 2 * bit)).tolist()]
    for a, b in zip(bounds[:-1], bounds[1:]):
        bits = one[a:b].astype(int).tolist()
        data, left = _bytes(bits, lsb_first=False)
        out["frames"].append({"t": int(st[a]), "bits": int(b - a), "data": data.hex(), "leftover_bits": left})
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


OVERDRIVE_ROM = (0x3C, 0x69)    # Overdrive Skip ROM, Overdrive Match ROM: the devices at overdrive speed after it
OVERDRIVE_LOOK = 8              # lows after such a command whose pace tells the switch


def onewire(ch: Channel, n: int, rate: float, speed: str = "standard") -> dict:
    """A 1-Wire line (idle high, every pulse low). A low of at least `reset_detect` is a reset; the next low that
    starts within the presence window after it is the presence pulse; every other low is a slot, 1 when shorter
    than `split` (write 1, or read 1), else 0 (write 0, or a read 0 held by the slave), LSB first. Who drove a low
    is not on the line: a read slot reads as the bit it carried.

    At standard speed an Overdrive Skip ROM or Overdrive Match ROM (the reset's first byte) puts the devices at
    overdrive speed: the rest is read at it (the reset's "overdrive_after_bits"), its resets too ("speed"), until a
    reset of standard length. Only the slots at `speed` go to "slot" / "recovery" (the pace of the master's slots)."""
    st, w = pulses(ch, n, 0)
    out = {"resets": [], "low_0": [], "low_1": [], "slot": [], "recovery": [], "before_reset": 0}
    cur = None
    bits: list[int] = []
    last = None                                    # (start, width) of the previous slot
    mode = speed
    std_reset = ONEWIRE["standard"]["reset_detect"] * rate

    def close():
        if cur is not None:
            cur["data"], cur["leftover_bits"] = (lambda d: (d[0].hex(), d[1]))(_bytes(bits, lsb_first=True))
            cur["bits"] = len(bits)

    starts, widths = st.tolist(), w.tolist()

    def paced_overdrive(i):
        """The lows after the i-th at overdrive: of the next ones (up to OVERDRIVE_LOOK, to a reset of standard
        length) most spaced closer than a standard slot, or every one as short as overdrive's lows with both bits
        among them (a master may pace overdrive slots as far apart as standard ones: sigrok-dumps' SoCkit). A reset's
        first byte of 0x3C or 0x69 that the master follows with standard slots was data, not the switch."""
        od = ONEWIRE["overdrive"]
        gaps, lows = [], []
        for j in range(i + 1, min(len(starts), i + 1 + OVERDRIVE_LOOK)):
            if widths[j] >= std_reset:
                break
            gaps.append(starts[j] - starts[j - 1])
            lows.append(widths[j])
        if not gaps:
            return False
        if float(np.median(gaps)) < ONEWIRE["standard"]["slot"][0] * rate:
            return True
        lo = np.asarray(lows, float) / rate
        return bool(np.all(lo <= od["low_0"][1]) and np.any(lo < od["split"]) and np.any(lo >= od["split"]))

    for i, (s, x) in enumerate(zip(starts, widths)):
        sp = ONEWIRE[mode]
        if x >= sp["reset_detect"] * rate:
            close()
            if x >= std_reset:
                mode = speed
                sp = ONEWIRE[mode]
            cur = {"t": s, "reset": x, "presence": False, **({"speed": mode} if mode != speed else {})}
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
        if mode == speed:
            out["low_1" if one else "low_0"].append((s, x))
            if last is not None and s - last[0] < 4 * sp["slot"][0] * rate:
                out["slot"].append((last[0], s - last[0]))
                out["recovery"].append((last[0] + last[1], s - last[0] - last[1]))
        last = (s, x)
        if (mode == "standard" and len(bits) == 8 and _bytes(bits, lsb_first=True)[0][0] in OVERDRIVE_ROM
                and paced_overdrive(i)):
            mode, last = "overdrive", None
            cur["overdrive_after_bits"] = 8
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
