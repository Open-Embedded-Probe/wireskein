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
        stretch: dict[int, float] | None = None, gap: float = 50e-6, data_at: float = 0.5) -> float:
    """transactions: {addr, rw ('write'|'read'), bytes, nack_at (index or None)}.
    Write bytes are ACKed by the slave unless nack_at; read bytes are ACKed by the
    master except the last. Clock-high data is stable; SDA changes data_at of the way
    into SCL's low (0.5: mid-low; small: a short hold after SCL falls)."""
    q = 1.0 / freq / 4
    bitno = 0

    def bit(v: int) -> None:
        nonlocal t, bitno
        sda.set(t + 2 * q * data_at, v)   # change data while SCL low
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
        gap: float = 20e-6, line_reset: bool = True, target_on_rise: bool = False) -> float:
    """ARM SWD packets. Each packet: {"apndp", "rnw", "a" (0..3), "ack" (1 OK / 2 WAIT / 4 FAULT), "data"}.

    Bits change just after the falling edge and are sampled on the rising edge
    (both directions, as logic analyzers see them). Turnaround cycles float high
    (pull-up). idle_clocks > 0 keeps the clock running with SWDIO low between
    packets (no gap in the clock), otherwise the clock stops for `gap`. target_on_rise: the target's bits (ACK,
    read data) change on the rising edge that clocks them out, the one before the edge they are read on (an
    STLink's target as captured).
    """
    h = 0.5 / freq

    def clock(bits, target=()):
        nonlocal t
        for k, v in enumerate(bits):
            if k in target:
                # driven on the rising edge before: stable through this bit's rising edge
                dio.set(t - h, v)
            else:
                # data changes just after the falling edge, is stable at the rising edge (with the target's on the
                # rise, a little later: a host's output delay, off the falling edge's sample)
                dio.set(t + h * (0.4 if target else 0.1), v)
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
        target = set(range(9, 12))
        if p["ack"] == 1:
            d = lsb(p["data"], 32) + [sum(lsb(p["data"], 32)) & 1]
            if p["rnw"]:
                target |= set(range(12, 45))
            bits += d + [1] if p["rnw"] else [1] + d
        else:
            bits += [1]
        clock(bits + [0] * idle_clocks, target if target_on_rise else ())
        if not idle_clocks:
            dio.set(t, 0)
            t += gap
    return t


def mdio(mdc: Wave, md: Wave, t: float, frames: list[dict], freq: float, preamble: int = 32, gap: float = 10e-6) -> float:
    """MDIO frames (IEEE 802.3). Each {"clause": 22 | 45, "op", "phy", "reg" (clause 22) or "dev" (45), "data"}; op
    as on the wire (22: 1 write, 2 read; 45: 0 address, 1 write, 3 read, 2 read and step). The controller changes MDIO
    after MDC's falling edge, the PHY its data (a read) after the rising edge before; both are read on the rising
    edge. The line
    idles high (pulled up), the clock stops between frames."""
    h = 0.5 / freq

    def bit(v, by_phy=False):
        nonlocal t
        md.set(t - h * 0.8 if by_phy else t + h * 0.1, v)     # the PHY just after the rise before this bit's
        mdc.set(t + h, 1)
        mdc.set(t + 2 * h, 0)
        t += 2 * h

    def field(v, n, by_phy=False):
        for i in range(n - 1, -1, -1):
            bit((v >> i) & 1, by_phy)

    for f in frames:
        c45 = f.get("clause", 22) == 45
        read = f["op"] in ((3, 2) if c45 else (2,))
        field((1 << preamble) - 1, preamble)
        field(0 if c45 else 1, 2)
        field(f["op"], 2)
        field(f["phy"], 5)
        field(f["dev"] if c45 else f["reg"], 5)
        bit(1)                                     # released on a read: the pull-up's 1
        bit(0, read)
        field(f["data"], 16, read)
        md.set(t + h * 0.1, 1)
        t += gap
    return t


def jtag(tck: Wave, tms: Wave, tdi: Wave, tdo: Wave, t: float, scans: list[dict], freq: float,
         idle_clocks: int = 2, gap: float = 5e-6) -> float:
    """JTAG scans (IEEE 1149.1) from Test-Logic-Reset: five TMS 1s, then each {"type": "IR" | "DR", "bits", "tdi",
    "tdo"} (values shifted LSB first) from Run-Test/Idle to Update and back, `idle_clocks` in Run-Test/Idle after.
    TMS, TDI and TDO change after TCK's falling edge and are read on the rising; TDO holds where nothing shifts."""
    h = 0.5 / freq

    def clock(m, i=None, o=None):
        nonlocal t
        tms.set(t + h * 0.1, m)
        if i is not None:
            tdi.set(t + h * 0.1, i)
        if o is not None:
            tdo.set(t + h * 0.15, o)
        tck.set(t + h, 1)
        tck.set(t + 2 * h, 0)
        t += 2 * h

    for _ in range(5):
        clock(1)
    clock(0)
    for sc in scans:
        clock(1)                                   # Select-DR-Scan
        if sc["type"] == "IR":
            clock(1)                               # Select-IR-Scan
        clock(0)                                   # Capture
        clock(0)                                   # Shift
        n = sc["bits"]
        for k in range(n):
            clock(1 if k == n - 1 else 0, (sc["tdi"] >> k) & 1, (sc["tdo"] >> k) & 1)
        clock(1)                                   # Update
        for _ in range(idle_clocks):
            clock(0)                               # Run-Test/Idle
        t += gap
    return t


def dht(w: Wave, t: float, readings: list[bytes], start: float = 1e-3, period: float = 0.5) -> float:
    """DHT11 / DHT22 readings (4 bytes each; the checksum, their sum, added): the host's low of `start`, its release
    (30 us), the sensor's answer (80 us low, 80 us high), 40 bits (50 us low, then 27 us high for 0, 70 us for 1),
    the line released after; `period` apart. The line idles high."""
    for r in readings:
        b = bytes(r) + bytes([sum(r) & 0xFF])
        w.set(t, 0)
        t += start
        w.set(t, 1)
        t += 30e-6
        w.set(t, 0)
        t += 80e-6
        w.set(t, 1)
        t += 80e-6
        for x in b:
            for k in range(7, -1, -1):
                w.set(t, 0)
                t += 50e-6
                w.set(t, 1)
                t += 70e-6 if (x >> k) & 1 else 27e-6
        w.set(t, 0)
        t += 50e-6
        w.set(t, 1)
        t += period
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


def rgb_led(w: Wave, t: float, data: bytes, t0h: float = 0.4e-6, t1h: float = 0.8e-6, period: float = 1.25e-6,
            reset: float = 80e-6) -> float:
    """WS2812-like: each bit a high of t0h / t1h in a period, MSB first, then a low of `reset`."""
    for b in data:
        for k in range(7, -1, -1):
            w.set(t, 1)
            w.set(t + (t1h if (b >> k) & 1 else t0h), 0)
            t += period
    return t + reset


def onewire(w: Wave, t: float, data: bytes, presence: bool = True, reset: float = 500e-6, low_0: float = 65e-6,
            low_1: float = 6e-6, slot: float = 70e-6) -> float:
    """A reset (and the slave's presence pulse), then `data` written LSB first. The line idles high."""
    w.set(t, 0)
    w.set(t + reset, 1)
    if presence:
        w.set(t + reset + 30e-6, 0)
        w.set(t + reset + 150e-6, 1)
    t += 2 * reset
    for b in data:
        for k in range(8):
            w.set(t, 0)
            w.set(t + (low_1 if (b >> k) & 1 else low_0), 1)
            t += slot
    return t + 100e-6


def nec(w: Wave, t: float, frames: list, carrier_hz: float | None = 38e3, duty: float = 1 / 3) -> float:
    """NEC frames ({"addr", "cmd"} or {"repeat": True}) on an IR line: with a carrier, the LED's drive (idle low,
    marks of carrier pulses); without, a receiver's output (idle high, marks low)."""
    idle = w.initial

    def mark(t, d):
        if carrier_hz is None:
            w.set(t, 1 - idle)
            w.set(t + d, idle)
            return
        p = 1 / carrier_hz
        k = 0
        while (k + duty) * p <= d:
            w.set(t + k * p, 1 - idle)
            w.set(t + (k + duty) * p, idle)
            k += 1

    for f in frames:
        mark(t, 9e-3)
        if f.get("repeat"):
            t += 9e-3 + 2.25e-3
            mark(t, 562.5e-6)
            t += 562.5e-6 + 40e-3
            continue
        t += 9e-3 + 4.5e-3
        a = f["addr"]
        data = [a & 0xFF, (a >> 8) if a > 0xFF else a ^ 0xFF, f["cmd"], f["cmd"] ^ 0xFF]
        for byte in data:
            for k in range(8):
                mark(t, 562.5e-6)
                t += 562.5e-6 + (1687.5e-6 if (byte >> k) & 1 else 562.5e-6)
        mark(t, 562.5e-6)
        t += 562.5e-6 + 40e-3
    return t


def i2s(bclk: Wave, ws: Wave, sd: Wave, t: float, frames: list[tuple[int, int]], fs: float, slot: int = 32,
        bits: int = 16, fmt: str = "i2s") -> float:
    """I2S-like audio: frames of (left, right), `slot` BCLKs a channel. SD and WS change on BCLK's falling edge;
    "i2s" puts WS one BCLK before the word's MSB and left at WS low, "left" / "right" align the word to the start /
    end of the slot with left at WS high."""
    h = 0.5 / (fs * 2 * slot)
    left = 0 if fmt == "i2s" else 1
    total = len(frames) * 2 * slot
    lvl, dat = [], []
    for f in frames:
        for c in (0, 1):
            v = f[c] & ((1 << bits) - 1)
            word = [(v >> (bits - 1 - j)) & 1 for j in range(bits)]
            pad = [0] * (slot - bits)
            dat += (pad + word) if fmt == "right" else (word + pad)
            lvl += [left if c == 0 else 1 - left] * slot
    if fmt == "i2s":
        lvl = lvl[1:] + [lvl[-1]]
    for g in range(total):
        bclk.set(t, 0)
        ws.set(t, lvl[g])
        sd.set(t, dat[g])
        bclk.set(t + h, 1)
        t += 2 * h
    bclk.set(t, 0)
    return t


def quadrature(a: Wave, b: Wave, t: float, steps: list[int], step_s: float) -> float:
    """Each entry of steps moves the encoder that many states (positive: A leading B), one state per step_s."""
    seq = [(0, 0), (1, 0), (1, 1), (0, 1)]
    k = seq.index((a.level, b.level))
    for n in steps:
        for _ in range(abs(n)):
            k = (k + (1 if n > 0 else -1)) % 4
            a.set(t, seq[k][0])
            b.set(t, seq[k][1])
            t += step_s
    return t


# --- the protocols added in 2026-11 (each encoder written apart from its reader: CRCs as registers of bits) ---

def _msb(v: int, n: int) -> list[int]:
    return [(v >> (n - 1 - i)) & 1 for i in range(n)]


def _lsb(v: int, n: int) -> list[int]:
    return [(v >> i) & 1 for i in range(n)]


def cec(w: Wave, t: float, messages: list[list[int]], gap: float = 7e-3) -> float:
    """HDMI CEC messages (blocks: the header byte initiator << 4 | destination, then opcode and operands): the start
    bit (3.7 ms low of 4.5), each block's 8 bits, EOM and ACK (data 1: 0.6 ms low, 0: 1.5 ms, 2.4 ms a bit); a
    directed message acknowledged (ACK 0), a broadcast (destination 15) not refused (1). The line idles high."""
    def bit(t, b):
        w.set(t, 0)
        w.set(t + (0.6e-3 if b else 1.5e-3), 1)
        return t + 2.4e-3
    for m in messages:
        w.set(t, 0)
        w.set(t + 3.7e-3, 1)
        t += 4.5e-3
        bc = (m[0] & 0xF) == 0xF
        for k, x in enumerate(m):
            for b in _msb(x, 8):
                t = bit(t, b)
            t = bit(t, int(k == len(m) - 1))
            t = bit(t, 1 if bc else 0)
        t += gap
    return t


SWIM_CLK = 1 / 8e6


def swim(w: Wave, t: float, ops: list[dict], clk: float = SWIM_CLK, gap: float = 50e-6) -> float:
    """SWIM (STM8) at low speed (22 clocks a bit: a 1 low for 2, a 0 for 20), each frame a start bit (0 host, 1
    target), its payload, even parity and the receiver's ACK (1). ops: {"cmd": "WOTF" | "ROTF", "addr", "data"}:
    the command (3 bits), the count, the 24-bit address, then the bytes (written by the host, or the target's
    answers). The line idles high."""
    def put(t, bits):
        for b in bits:
            w.set(t, 0)
            w.set(t + (2 if b else 20) * clk, 1)
            t += 22 * clk
        return t

    def frame(t, start, payload):
        return put(t, [start] + payload + [sum(payload) & 1, 1]) + 30 * clk
    for op in ops:
        data = bytes(op["data"])
        t = frame(t, 0, [0, 1, 0] if op["cmd"] == "WOTF" else [0, 0, 1])
        for v in (len(data), (op["addr"] >> 16) & 0xFF, (op["addr"] >> 8) & 0xFF, op["addr"] & 0xFF):
            t = frame(t, 0, _msb(v, 8))
        for v in data:
            t = frame(t, 0 if op["cmd"] == "WOTF" else 1, _msb(v, 8))
        t += gap
    return t


def dali(w: Wave, t: float, frames: list[tuple[int, int, float]]) -> float:
    """DALI (1200 b/s, Manchester: a 1 low then high), frames (value, bits: 16 forward or 8 backward, the gap after
    in seconds), each led by a start bit 1. The line idles high."""
    half = 1 / 2400
    for value, nbits, after in frames:
        for b in [1] + _msb(value, nbits):
            w.set(t, 0 if b else 1)
            w.set(t + half, 1 if b else 0)
            t += 2 * half
        w.set(t, 1)
        t += 6 * half + after
    return t


def dcc(w: Wave, t: float, packets: list[list[int]], preamble: int = 14) -> float:
    """DCC (NMRA S-9.2) packets: a preamble of 1s, each byte led by a 0, the XOR of the bytes, a closing 1; a 1 two
    halves of 58 us, a 0 of 100 us. The signal never stops: a few preamble 1s after the last."""
    def put(t, bits):
        for b in bits:
            h = 58e-6 if b else 100e-6
            w.set(t, 1)
            w.set(t + h, 0)
            t += 2 * h
        return t
    for data in packets:
        x = 0
        for v in data:
            x ^= v
        bits = [1] * preamble
        for v in list(data) + [x]:
            bits += [0] + _msb(v, 8)
        t = put(t, bits + [1])
    return put(t, [1] * 4)


def sent_crc4(nibbles: list[int]) -> int:
    """SENT's CRC4 (the 2010 recommendation): seed 5, x^4 + x^3 + x^2 + 1, a zero nibble after the data."""
    def steps(reg):
        for _ in range(4):
            top = (reg >> 3) & 1
            reg = (reg << 1) & 0xF
            if top:
                reg ^= 0b1101
        return reg
    reg = 5
    for x in nibbles:
        reg = steps(reg) ^ x
    return steps(reg)


def sent(w: Wave, t: float, messages: list[list[int]], tick: float = 3e-6, pause: int = 80) -> float:
    """SENT (SAE J2716) messages: the sync (56 ticks), the status nibble (0 here), the data nibbles and the CRC4, each
    nibble 12 + its value ticks, a pause pulse after; every pulse a low of 5 ticks. The line idles high."""
    for data in messages:
        for k in [56, 12] + [12 + x for x in data] + [12 + sent_crc4(data), pause]:
            w.set(t, 0)
            w.set(t + 5 * tick, 1)
            t += k * tick
    w.set(t, 0)
    w.set(t + 5 * tick, 1)
    return t + 5 * tick


def j1850_crc8(data: bytes) -> int:
    reg = 0xFF
    for x in data:
        for k in range(7, -1, -1):
            top = ((reg >> 7) & 1) ^ ((x >> k) & 1)
            reg = (reg << 1) & 0xFF
            if top:
                reg ^= 0x1D
    return reg ^ 0xFF


def j1850(w: Wave, t: float, frames: list[bytes], offset: float = 0.0, gap: float = 2e-3) -> float:
    """SAE J1850 VPW frames (the CRC8 added): SOF (200 us active), then symbols taking turns passive / active, a 1
    long (128 us) when passive, short (64 us) when active; `offset` lengthens the active level (a comparator's
    threshold off the middle). The line idles passive (low)."""
    for body in frames:
        data = bytes(body) + bytes([j1850_crc8(bytes(body))])
        w.set(t, 1)
        t += 200e-6 + offset
        active = False
        for x in data:
            for b in _msb(x, 8):
                long_ = (b == 1) if not active else (b == 0)
                w.set(t, int(active))
                t += (128e-6 if long_ else 64e-6) + (offset if active else -offset)
                active = not active
        w.set(t, 0)
        t += gap
    return t


def wiegand26(facility: int, card: int) -> list[int]:
    body = _msb(facility, 8) + _msb(card, 16)
    return [sum(body[:12]) % 2] + body + [1 - sum(body[12:]) % 2]


def wiegand(d0: Wave, d1: Wave, t: float, frames: list[list[int]], width: float = 50e-6, period: float = 1e-3,
            gap: float = 50e-3) -> float:
    """Wiegand frames (lists of bits): a low pulse of `width` on D1 for a 1, on D0 for a 0, `period` apart. Both
    lines idle high."""
    for bits in frames:
        for b in bits:
            w = d1 if b else d0
            w.set(t, 0)
            w.set(t + width, 1)
            t += period
        t += gap
    return t


def ps2(clk: Wave, dat: Wave, t: float, data: bytes, freq: float = 12.5e3, gaps: list[float] | None = None) -> float:
    """PS/2 device to host: per byte a start 0, 8 bits LSB first, odd parity, a stop 1; data set while the clock is
    high, read on its fall. Both lines idle high; gaps[i] after byte i (1 ms if none given)."""
    h = 0.5 / freq
    for i, byte in enumerate(data):
        bits = [0] + _lsb(byte, 8) + [1 - (bin(byte).count("1") & 1), 1]
        for b in bits:
            dat.set(t, b)
            clk.set(t + 0.5 * h, 0)
            clk.set(t + 1.5 * h, 1)
            t += 2 * h
        dat.set(t, 1)
        t += gaps[i] if gaps else 1e-3
    return t


USB_PID = {"OUT": 0x1, "IN": 0x9, "SOF": 0x5, "SETUP": 0xD, "DATA0": 0x3, "DATA1": 0xB, "ACK": 0x2, "NAK": 0xA,
           "STALL": 0xE}


def _crc_sent(bits: list[int], width: int, poly: int, init: int) -> list[int]:
    reg, top, mask = init, 1 << (width - 1), (1 << width) - 1
    for b in bits:
        fb = ((reg & top) >> (width - 1)) ^ b
        reg = (reg << 1) & mask
        if fb:
            reg ^= poly
    reg ^= mask
    return _msb(reg, width)


def usb_packet_bits(pkt: dict) -> list[int]:
    """A USB packet's bits before NRZI and stuffing: SYNC, PID and its complement, the token's address and endpoint
    or the SOF's frame number with CRC5, or the data with CRC16."""
    pid = USB_PID[pkt["pid"]]
    bits = [0] * 7 + [1] + _lsb(pid, 4) + _lsb(pid ^ 0xF, 4)
    if "addr" in pkt:
        body = _lsb(pkt["addr"], 7) + _lsb(pkt["endp"], 4)
        bits += body + _crc_sent(body, 5, 0b00101, 0x1F)
    elif "frame" in pkt:
        body = _lsb(pkt["frame"], 11)
        bits += body + _crc_sent(body, 5, 0b00101, 0x1F)
    elif "data" in pkt:
        body = [b for x in bytes(pkt["data"]) for b in _lsb(x, 8)]
        bits += body + _crc_sent(body, 16, 0x8005, 0xFFFF)
    return bits


def usb(dp: Wave, dm: Wave, t: float, packets: list[dict], speed: str = "full", gap_bits: float = 40) -> float:
    """USB low / full speed on D+ / D-: each packet NRZI coded (a 0 a change) with bit stuffing (a 0 after six 1s),
    ended by an SE0 of two bits and J; idle J (full speed D+ high, low speed D- high) `gap_bits` between."""
    fs = speed == "full"
    bt = 1 / (12e6 if fs else 1.5e6)
    j = (1, 0) if fs else (0, 1)
    dp.set(t, j[0])
    dm.set(t, j[1])
    for p in packets:
        lvl, raw, ones = j, [], 0
        for b in usb_packet_bits(p):
            raw.append(b)
            ones = ones + 1 if b else 0
            if ones == 6:
                raw.append(0)
                ones = 0
        for b in raw:
            if b == 0:
                lvl = (1 - lvl[0], 1 - lvl[1])
            dp.set(t, lvl[0])
            dm.set(t, lvl[1])
            t += bt
        dp.set(t, 0)
        dm.set(t, 0)
        t += 2 * bt
        dp.set(t, j[0])
        dm.set(t, j[1])
        t += gap_bits * bt
    return t


PD_4B5B = [0b11110, 0b01001, 0b10100, 0b10101, 0b01010, 0b01011, 0b01110, 0b01111, 0b10010, 0b10011, 0b10110,
           0b10111, 0b11010, 0b11011, 0b11100, 0b11101]


def usb_pd(w: Wave, t: float, messages: list[tuple[int, list[int]]], ui: float = 1 / 300e3, skew: float = 0.0,
           gap: float = 300e-6) -> float:
    """USB PD messages (header, data objects) on CC: the preamble (64 bits), SOP (Sync-1 x3, Sync-2), header and
    objects little endian and their CRC32 (zlib's: the standard one), 4b5b, EOP; biphase mark coding (a change at
    each bit's start, another mid-bit for a 1), `skew` added to each fall (a threshold off the middle)."""
    import zlib
    for header, objects in messages:
        body = header.to_bytes(2, "little") + b"".join(o.to_bytes(4, "little") for o in objects)
        body += zlib.crc32(body).to_bytes(4, "little")
        bits = [i & 1 for i in range(64)]
        for k in (0b11000, 0b11000, 0b11000, 0b10001):
            bits += _lsb(k, 5)
        for x in body:
            bits += _lsb(PD_4B5B[x & 0xF], 5) + _lsb(PD_4B5B[x >> 4], 5)
        bits += _lsb(0b01101, 5)
        lvl = w.level
        for b in bits:
            lvl ^= 1
            w.set(t + (skew if lvl == 0 else 0), lvl)
            if b:
                lvl ^= 1
                w.set(t + ui / 2 + (skew if lvl == 0 else 0), lvl)
            t += ui
        lvl ^= 1
        w.set(t + (skew if lvl == 0 else 0), lvl)
        if lvl:
            w.set(t + ui, 0)
        t += 2 * ui + gap
    return t


SPDIF_PRE = {"B": [1, 1, 1, 0, 1, 0, 0, 0], "M": [1, 1, 1, 0, 0, 0, 1, 0], "W": [1, 1, 1, 0, 0, 1, 0, 0]}


def spdif(w: Wave, t: float, samples: list[tuple[int, int]], fs: float = 48000, frame0: int = 0) -> float:
    """S/PDIF frames (left, right: 24-bit) at `fs`: subframes of 64 half cells, the preamble (B every 192 frames on
    channel A, M otherwise, W on B; inverted after a cell ending low), 24 audio bits LSB first, V U C 0 and even
    parity, biphase mark coded."""
    half = 1 / (fs * 128)
    lvl = w.level
    for f, pair in enumerate(samples):
        for ch, value in enumerate(pair):
            pre = ("B" if (frame0 + f) % 192 == 0 else "M") if ch == 0 else "W"
            halves = list(SPDIF_PRE[pre] if lvl == 0 else [1 - x for x in SPDIF_PRE[pre]])
            bits = _lsb(value & 0xFFFFFF, 24) + [0, 0, 0]
            bits.append(sum(bits) & 1)
            x = halves[-1]
            for b in bits:
                x ^= 1
                halves.append(x)
                if b:
                    x ^= 1
                halves.append(x)
            for x in halves:
                w.set(t, x)
                t += half
            lvl = x
    return t


def sd_crc7(bits: list[int]) -> int:
    reg = 0
    for b in bits:
        fb = ((reg >> 6) & 1) ^ b
        reg = (reg << 1) & 0x7F
        if fb:
            reg ^= 0x09
    return reg


def sd_frame(direction: int, index: int, arg: int) -> list[int]:
    """A 48-bit command (direction 1) or R1-type response (0) with its CRC7."""
    body = [0, direction] + _msb(index, 6) + _msb(arg, 32)
    return body + _msb(sd_crc7(body), 7) + [1]


def sd_r3(ocr: int) -> list[int]:
    return [0, 0] + [1] * 6 + _msb(ocr, 32) + [1] * 8


def sd_r2(reg: int) -> list[int]:
    body = _msb(reg >> 8, 120)
    return [0, 0] + [1] * 6 + body + _msb(sd_crc7(body), 7) + [1]


def sdmmc(clk: Wave, cmd: Wave, t: float, frames: list[list[int]], freq: float = 400e3, idle: int = 8) -> float:
    """SD mode's CMD line: the frames' bits (sd_frame, sd_r3, sd_r2), `idle` 1s between, CMD changed after CLK's
    fall and read on its rise; the clock runs throughout. CMD idles high."""
    h = 0.5 / freq
    bits = []
    for fr in frames:
        bits += [1] * idle + fr
    bits += [1] * idle
    for b in bits:
        cmd.set(t + 0.2 * h, b)
        clk.set(t + h, 1)
        clk.set(t + 2 * h, 0)
        t += 2 * h
    return t


def cjtag(tckc: Wave, tmsc: Wave, t: float, scans: list[dict], tck: float = 1e-6, idle_cycles: int = 3) -> float:
    """cJTAG (IEEE 1149.7) OScan1: a reset escape (8 toggles), TLR to Run-Test/Idle, a selection escape (6) and the
    online activation (OAC 1100, EC 1000, CP 0110, LSB first), then each scan ({"type", "bits", "tdi", "tdo"}, LSB
    first) as JTAG cycles of three TCKC cycles (nTDI, TMS, TDO). TMSC set while TCKC is low, read on its rise; both
    idle high."""
    def cycle(msc):
        nonlocal t
        tckc.set(t, 0)
        tmsc.set(t + 0.1 * tck, msc)
        tckc.set(t + 0.5 * tck, 1)
        t += tck

    def escape(n):
        nonlocal t
        tckc.set(t, 0)
        tckc.set(t + 0.5 * tck, 1)
        u, lvl = t + 0.6 * tck, tmsc.level
        for _ in range(n):
            lvl ^= 1
            tmsc.set(u, lvl)
            u += 0.2 * tck
        t = u + 0.5 * tck
    escape(8)
    for b in [1, 1, 1, 1, 1, 0]:
        cycle(b)
    escape(6)
    for b in [0, 0, 1, 1] + [0, 0, 0, 1] + [0, 1, 1, 0]:
        cycle(b)
    cyc = [(0, 0, 0)] * idle_cycles
    for sc in scans:
        cyc += [(1, 0, 0)] + ([(1, 0, 0)] if sc["type"] == "IR" else []) + [(0, 0, 0)] * 2
        n = sc["bits"]
        for j in range(n):
            cyc.append((int(j == n - 1), (sc["tdi"] >> j) & 1, (sc["tdo"] >> j) & 1))
        cyc += [(1, 0, 0), (0, 0, 0)]
    for tms, tdi, tdo in cyc:
        cycle(1 - tdi)
        cycle(tms)
        cycle(tdo)
    return t


def tdm(bclk: Wave, fsync: Wave, sd: Wave, t: float, frames: list[list[int]], fs: float = 48000, slot: int = 32,
        bits: int = 24, offset: int = 1, fs_width: int = 1) -> float:
    """TDM: each frame len(frame) slots of `slot` bits, the words MSB first at a slot's start (zeros after); the frame
    sync high for `fs_width` bits from `offset` bits before the frame's first bit (1, 1: DSP mode A; 0, slot: mode
    B). FS and SD change on BCLK's fall."""
    n = len(frames[0])
    h = 0.5 / (fs * n * slot)
    dat = []
    for f in frames:
        for v in f:
            dat += _msb(v & ((1 << bits) - 1), bits) + [0] * (slot - bits)
    total = len(dat)
    sync = [0] * total
    for k in range(len(frames)):
        for j in range(fs_width):
            i = k * n * slot - offset + j
            if 0 <= i < total:
                sync[i] = 1
    for g in range(total):
        bclk.set(t, 0)
        fsync.set(t, sync[g])
        sd.set(t, dat[g])
        bclk.set(t + h, 1)
        t += 2 * h
    bclk.set(t, 0)
    return t


CAN_FD_LENGTHS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64]
_CRC17 = [16, 14, 13, 11, 6, 4, 3, 1, 0]
_CRC21 = [20, 13, 11, 7, 4, 3, 0]


def _crc_reg(bits, width, taps, first=1):
    reg = [first] + [0] * (width - 1)
    for b in bits:
        fb = b ^ reg[0]
        reg = reg[1:] + [0]
        if fb:
            for tp in taps:
                reg[width - 1 - tp] ^= 1
    return reg


def _stuff(body):
    out, run, last = [], 0, None
    for b in body:
        out.append(b)
        run = run + 1 if b == last else 1
        last = b
        if run == 5:
            out.append(1 - b)
            last, run = 1 - b, 1
    return out, len(out) - len(body)


def can_fd_frame(ident: int, data: bytes, brs: bool = True, ext: bool | None = None):
    """A CAN FD frame's bits (ISO 11898-1:2015): (before the rate switch, at the data rate, after it)."""
    ext = ident > 0x7FF if ext is None else ext
    dlc = CAN_FD_LENGTHS.index(len(data))
    if ext:
        head = [0] + _msb(ident >> 18, 11) + [1, 1] + _msb(ident & 0x3FFFF, 18) + [0, 1, 0, int(brs)]
    else:
        head = [0] + _msb(ident, 11) + [0, 0, 1, 0, int(brs)]
    tail = [0] + _msb(dlc, 4) + [b for x in data for b in _msb(x, 8)]
    sent, n_stuff = _stuff(head + tail)
    hs, _ = _stuff(head)
    split = len(hs) - (1 if len(hs) >= 6 and len(set(hs[-6:-1])) == 1 and hs[-1] != hs[-2] else 0)
    gray = [(0, 0, 0), (0, 0, 1), (0, 1, 1), (0, 1, 0), (1, 1, 0), (1, 1, 1), (1, 0, 1), (1, 0, 0)][n_stuff % 8]
    sc = list(gray) + [sum(gray) & 1]
    width, taps = (17, _CRC17) if len(data) <= 16 else (21, _CRC21)
    seq = sc + _crc_reg(sent + sc, width, taps)
    field, prev = [], sent[-1]
    for i, b in enumerate(seq):
        if i == 0 or (i >= 4 and (i - 4) % 4 == 0):
            field.append(1 - prev)
            prev = 1 - prev
        field.append(b)
        prev = b
    return sent[:split], sent[split:] + field + [1], [0, 1] + [1] * 7


def can_fd(w: Wave, t: float, frames: list[dict], nominal: float, data_rate: float, sp: float = 0.75,
           ifs_bits: float = 10) -> float:
    """CAN FD frames ({"id", "data", "brs"}; "fd": False a classic data frame instead, its CRC15 from 0), the data
    phase at `data_rate` where BRS is set (the rate changing at BRS's and the CRC delimiter's sample points)."""
    tn, td = 1 / nominal, 1 / data_rate
    segs = []
    for f in frames:
        if not f.get("fd", True):
            body = [0] + _msb(f["id"], 11) + [0, 0, 0] + _msb(len(f["data"]), 4) + \
                [b for x in f["data"] for b in _msb(x, 8)]
            s, _ = _stuff(body + _crc_reg(body, 15, [14, 10, 8, 7, 4, 3, 0], first=0))
            segs += [(b, tn) for b in s + [1, 0, 1] + [1] * 7]
        else:
            slow, fast, after = can_fd_frame(f["id"], bytes(f["data"]), f.get("brs", True))
            if not f.get("brs", True):
                segs += [(b, tn) for b in slow + fast + after]
            else:
                segs += [(b, tn) for b in slow[:-1]] + [(slow[-1], sp * tn + (1 - sp) * td)]
                segs += [(b, td) for b in fast[:-1]] + [(fast[-1], sp * td + (1 - sp) * tn)]
                segs += [(b, tn) for b in after]
        segs.append((1, ifs_bits * tn))
    for level, d in segs:
        w.set(t, level)
        t += d
    return t
