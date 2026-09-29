"""Synthetic captures with known ground truth.

Waveforms are built in continuous time (seconds) and quantized to the sample
grid at the end, optionally with timing jitter, so the same scenario can be
rendered at different sample rates.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from .model import Capture, Channel


@dataclass
class Wave:
    initial: int
    times: list[float] = field(default_factory=list)  # toggle times
    level: int = 0

    def __post_init__(self) -> None:
        self.level = self.initial

    def set(self, t: float, level: int) -> None:
        if level != self.level:
            if self.times and t < self.times[-1]:
                raise ValueError("time went backwards")
            self.times.append(t)
            self.level = level


def quantize(waves: dict[str, Wave], rate: float, duration: float, rng: np.random.Generator,
             jitter_s: float = 0.0, glitches: dict[str, int] | None = None, t_start: float = 0.0) -> Capture:
    """t_start > 0 starts the capture mid-stream (the first frames are cut)."""
    n = int(round((duration - t_start) * rate))
    channels = []
    for name, w in waves.items():
        t = np.asarray(w.times, dtype=np.float64)
        before = int(np.sum(t <= t_start))
        initial = w.initial ^ (before & 1)
        t = t[t > t_start] - t_start
        if jitter_s:
            t = t + rng.normal(0, jitter_s, len(t))
        s = np.round(t * rate).astype(np.int64)
        s = s[(s > 0) & (s < n)]
        s = np.sort(s)
        # Two toggles on the same sample cancel out.
        uniq, counts = np.unique(s, return_counts=True)
        s = uniq[counts % 2 == 1]
        if glitches and glitches.get(name):
            g = np.sort(rng.integers(1, n - 2, glitches[name]))
            s = np.sort(np.concatenate([s, g, g + 1]))
            uniq, counts = np.unique(s, return_counts=True)
            s = uniq[counts % 2 == 1]
        channels.append(Channel(name, initial, s))
    return Capture(rate, n, channels)


# --- protocol encoders -----------------------------------------------------

def uart(w: Wave, t: float, data: bytes, baud: float, data_bits=8, parity="none", stop_bits=1.0,
         bit_order="lsb", gap_bits: float | list[float] = 0.0) -> float:
    """Encode bytes starting at t on an idle line. Returns end time.
    w.initial is the idle level; inverted UART is idle=0."""
    idle = w.initial
    bt = 1.0 / baud
    for i, b in enumerate(data):
        bits = [(b >> k) & 1 for k in range(data_bits)]
        if bit_order == "msb":
            bits.reverse()
        if parity == "even":
            bits.append(sum(bits) & 1)
        elif parity == "odd":
            bits.append(1 - (sum(bits) & 1))
        seq = [0] + bits
        for k, v in enumerate(seq):
            level = v if idle == 1 else 1 - v
            w.set(t + k * bt, level)
        t += len(seq) * bt
        w.set(t, idle)
        t += stop_bits * bt
        g = gap_bits[i] if isinstance(gap_bits, list) else gap_bits
        t += g * bt
    return t


def i2c(scl: Wave, sda: Wave, t: float, transactions: list[dict], freq: float,
        stretch: dict[int, float] | None = None, gap: float = 50e-6) -> float:
    """transactions: {addr, rw ('write'|'read'), bytes, nack_at (index or None)}.
    Write bytes are ACKed by the slave unless nack_at; read bytes are ACKed by the
    master except the last. Clock-high data is stable; SDA changes mid-low."""
    q = 1.0 / freq / 4
    bitno = 0

    def bit(v: int) -> None:
        nonlocal t, bitno
        sda.set(t + q, v)          # change data while SCL low
        extra = (stretch or {}).get(bitno, 0.0)
        scl.set(t + 2 * q + extra, 1)
        scl.set(t + 4 * q + extra, 0)
        t += 4 * q + extra
        bitno += 1

    for tr in transactions:
        # START (or repeated START): SDA high->low while SCL high
        sda.set(t, 1)
        scl.set(t + q, 1)
        sda.set(t + 2 * q, 0)
        scl.set(t + 3 * q, 0)
        t += 3 * q
        byte0 = (tr["addr"] << 1) | (1 if tr["rw"] == "read" else 0)
        payload = [byte0] + list(tr["bytes"])
        nack_at = tr.get("nack_at")
        for i, b in enumerate(payload):
            for k in range(7, -1, -1):
                bit((b >> k) & 1)
            if tr["rw"] == "read" and i > 0:
                ack = 0 if i < len(payload) - 1 else 1
            else:
                ack = 1 if nack_at == i else 0
            bit(ack)
            if nack_at == i:
                break
        if tr.get("repeated_start"):
            sda.set(t + q, 1)
            t += 2 * q
            continue
        # STOP: SDA low->high while SCL high
        sda.set(t + q, 0)
        scl.set(t + 2 * q, 1)
        sda.set(t + 3 * q, 1)
        t += 4 * q + gap
    return t


def spi(clk: Wave, mosi: Wave, miso: Wave | None, cs: Wave | None, t: float, frames: list[tuple[bytes, bytes]],
        freq: float, mode: int = 0, bit_order="msb", cs_setup: float | None = None, gap: float = 20e-6) -> float:
    """Each frame is (mosi_bytes, miso_bytes) sent in one CS assertion."""
    cpol, cpha = mode >> 1, mode & 1
    h = 0.5 / freq
    setup = cs_setup if cs_setup is not None else h
    for tx, rx in frames:
        if cs:
            cs.set(t, 0)
        t += setup
        for bi in range(len(tx)):
            for k in range(8):
                sh = 7 - k if bit_order == "msb" else k
                vo = (tx[bi] >> sh) & 1
                vi = (rx[bi] >> sh) & 1 if rx else 0
                if cpha == 0:
                    # data shifts just after the previous trailing edge
                    mosi.set(t, vo)
                    if miso:
                        miso.set(t, vi)
                    clk.set(t + h * 0.9, 1 - cpol)
                    clk.set(t + h * 1.9, cpol)
                else:
                    clk.set(t, 1 - cpol)
                    mosi.set(t + h * 0.1, vo)
                    if miso:
                        miso.set(t + h * 0.1, vi)
                    clk.set(t + h, cpol)
                t += 2 * h
        t += setup
        if cs:
            cs.set(t, 1)
        t += gap
    return t


def pwm(w: Wave, t0: float, t1: float, freq: float, duty: float) -> None:
    p = 1.0 / freq
    t = t0
    while t < t1:
        w.set(t, 1)
        w.set(t + duty * p, 0)
        t += p


def random_toggles(w: Wave, t0: float, t1: float, rate_hz: float, rng: np.random.Generator) -> None:
    n = rng.poisson((t1 - t0) * rate_hz)
    for t in np.sort(rng.uniform(t0, t1, n)):
        w.set(float(t), 1 - w.level)


def lin_pid(ident: int) -> int:
    b = [(ident >> i) & 1 for i in range(6)]
    p0 = b[0] ^ b[1] ^ b[2] ^ b[4]
    p1 = 1 - (b[1] ^ b[3] ^ b[4] ^ b[5])
    return ident | (p0 << 6) | (p1 << 7)


def lin_checksum(data: bytes, pid: int | None) -> int:
    s = pid or 0
    for x in data:
        s += x
        s = (s & 0xFF) + (s >> 8)
    return (~s) & 0xFF


def lin(w: Wave, t: float, frames: list[tuple[int, bytes]], baud: float = 19200.0) -> float:
    """LIN frames: break (13 bits low) + delimiter + sync 0x55 + PID + data + enhanced checksum."""
    bt = 1.0 / baud
    for ident, data in frames:
        w.set(t, 0)
        t += 13 * bt
        w.set(t, 1)
        t += 1 * bt
        pid = lin_pid(ident)
        t = uart(w, t, bytes([0x55, pid]) + data + bytes([lin_checksum(data, pid)]), baud, gap_bits=0.5)
        t += 20 * bt
    return t


def dmx(w: Wave, t: float, packets: list[bytes], baud: float = 250000.0) -> float:
    """DMX512 packets: break (>= 88 us low) + MAB (12 us high) + start code 0x00 + slots, 8N2."""
    for slots in packets:
        w.set(t, 0)
        t += 110e-6
        w.set(t, 1)
        t += 12e-6
        t = uart(w, t, bytes([0]) + slots, baud, stop_bits=2, gap_bits=0.3)
        t += 60e-6
    return t


def swd(clk: Wave, dio: Wave, t: float, packets: list[dict], freq: float, idle_clocks: int = 0,
        gap: float = 20e-6, line_reset: bool = True) -> float:
    """ARM SWD packets. Each packet: {"apndp", "rnw", "a" (0..3), "ack" (1 OK / 2 WAIT / 4 FAULT), "data"}.

    Bits change just after the falling edge and are sampled on the rising edge
    (both directions, as logic analyzers see them). Turnaround cycles float high
    (pull-up). idle_clocks > 0 keeps the clock running with SWDIO low between
    packets (no gap in the clock), otherwise the clock stops for `gap`.
    """
    h = 0.5 / freq

    def clock(bits):
        nonlocal t
        for v in bits:
            # data changes just after the falling edge, is stable at the rising edge
            dio.set(t + h * 0.1, v)
            clk.set(t + h, 1)
            clk.set(t + h * 2, 0)
            t += 2 * h

    def lsb(v, n):
        return [(v >> i) & 1 for i in range(n)]

    if line_reset:
        clock([1] * 56 + [0, 0])
        t += gap
    for p in packets:
        hdr = [p["apndp"], p["rnw"], p["a"] & 1, p["a"] >> 1]
        req = [1] + hdr + [sum(hdr) & 1, 0, 1]
        bits = req + [1] + lsb(p["ack"], 3)
        if p["ack"] == 1:
            d = lsb(p["data"], 32) + [sum(lsb(p["data"], 32)) & 1]
            bits += d + [1] if p["rnw"] else [1] + d
        else:
            bits += [1]
        clock(bits + [0] * idle_clocks)
        if not idle_clocks:
            dio.set(t, 0)
            t += gap
    return t


def can_crc15(bits) -> int:
    crc = 0
    for b in bits:
        nxt = b ^ ((crc >> 14) & 1)
        crc = (crc << 1) & 0x7FFF
        if nxt:
            crc ^= 0x4599
    return crc


def can(w: Wave, t: float, frames: list[dict], bitrate: float, ifs_bits: float = 3.0) -> float:
    """Classic CAN data frames {"id", "ext", "data", "ack"} on one line (TX and RX
    wired together as on the bus, recessive = 1). Stuffing after 5 equal bits
    from SOF to the end of the CRC."""
    bt = 1.0 / bitrate

    def msb(v, n):
        return [(v >> (n - 1 - i)) & 1 for i in range(n)]

    for f in frames:
        d = bytes(f["data"])
        if f["ext"]:
            head = [0] + msb(f["id"] >> 18, 11) + [1, 1] + msb(f["id"] & 0x3FFFF, 18) + [0, 0, 0]
        else:
            head = [0] + msb(f["id"], 11) + [0, 0, 0]
        body = head + msb(len(d), 4) + [b for x in d for b in msb(x, 8)]
        body += msb(can_crc15(body), 15)
        stuffed, run, last = [], 0, None
        for b in body:
            stuffed.append(b)
            run = run + 1 if b == last else 1
            last = b
            if run == 5:
                stuffed.append(1 - b)
                last, run = 1 - b, 1
        tail = [1, 0 if f.get("ack", True) else 1, 1] + [1] * 7
        for b in stuffed + tail:
            w.set(t, b)
            t += bt
        t += ifs_bits * bt
    return t
