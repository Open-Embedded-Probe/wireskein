"""Random scenarios for the evaluation corpus.

Each scenario combines one to three buses with decoy lines, renders them at a
sample rate chosen relative to the fastest bus, anonymizes the channels and
returns (capture, truth). Scenarios are fully determined by their seed.
"""

from __future__ import annotations

import numpy as np

from . import gen
from .fixture import anonymize
from .model import Capture

BAUDS = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1_000_000, 250_000, 31250, 74880]
TEXT = [b"OK\r\n", b"AT+GMR\r\n", b"temp=23.5 hum=41.2\n", b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n",
        b"CASE_BEGIN Device Detection\n", b"ready> ", b"ERROR 12\r\n"]


def _payload(rng: np.random.Generator, n: int, text: bool) -> bytes:
    if text:
        out = b""
        while len(out) < n:
            out += TEXT[rng.integers(len(TEXT))]
        return out
    return bytes(rng.integers(0, 256, n, dtype=np.uint8))


class Builder:
    def __init__(self, rng: np.random.Generator, stress: str | None = None):
        self.rng = rng
        self.stress = stress
        self.waves: dict[str, gen.Wave] = {}
        self.buses: list[dict] = []
        self.decoys: list[dict] = []
        self.fastest = 0.0
        self.t_end = 0.0

    def wave(self, name: str, initial: int) -> gen.Wave:
        self.waves[name] = gen.Wave(initial)
        return self.waves[name]

    def add_uart(self, idx: int) -> None:
        r = self.rng
        baud = float(r.choice(BAUDS)) * (1 + r.uniform(-0.01, 0.01))
        cfg = {
            "baud": baud,
            "data_bits": int(r.choice([8, 8, 8, 7])),
            "parity": str(r.choice(["none", "none", "even", "odd"])),
            "stop_bits": float(r.choice([1, 1, 2])),
            "idle": int(r.choice([1, 1, 1, 0])),
            "bit_order": "lsb",
        }
        text = bool(r.random() < 0.6)
        name = f"uart{idx}_tx"
        w = self.wave(name, cfg["idle"])
        t = r.uniform(0.2e-3, 2e-3)
        sent = b""
        segments = []
        hop = float(r.choice([2, 4, 8])) if self.stress == "baudhop" else 1.0
        n_chunks = int(r.integers(2, 6))
        for ci in range(n_chunks):
            if self.stress == "baudhop" and ci == n_chunks // 2 and n_chunks > 1:
                cfg = dict(cfg, baud=cfg["baud"] * hop)
                t += 50 / cfg["baud"] * 10
            segments.append({"from": t, "baud": cfg["baud"]})
            data = _payload(r, int(r.integers(4, 40)), text)
            if cfg["data_bits"] == 7:
                data = bytes(b & 0x7F for b in data)
            gaps = [float(r.choice([0, 0, 0, r.uniform(0, 3)])) for _ in data]
            t = gen.uart(w, t, data, cfg["baud"], cfg["data_bits"], cfg["parity"], cfg["stop_bits"], gap_bits=gaps)
            sent += data
            t += r.uniform(10, 200) / cfg["baud"] * 10
        params = dict(cfg, baud=segments[0]["baud"])
        if hop != 1.0:
            params["baud_segments"] = segments
        self.buses.append({"protocol": "uart", "roles": {"data": name}, "params": params,
                           "expect": {"bytes": sent.hex()}})
        self.fastest = max(self.fastest, cfg["baud"])
        self.t_end = max(self.t_end, t)

    def add_i2c(self, idx: int) -> None:
        r = self.rng
        freq = float(r.choice([100e3, 100e3, 400e3, 1e6])) * (1 + r.uniform(-0.05, 0.05))
        scl, sda = self.wave(f"i2c{idx}_scl", 1), self.wave(f"i2c{idx}_sda", 1)
        addr = int(r.integers(0x08, 0x78))
        txs = []
        for _ in range(int(r.integers(3, 12))):
            rw = "read" if r.random() < 0.4 else "write"
            tr = {"addr": addr if r.random() < 0.8 else int(r.integers(0x08, 0x78)), "rw": rw,
                  "bytes": [int(x) for x in r.integers(0, 256, int(r.integers(0 if rw == "write" else 1, 7)))]}
            if r.random() < 0.1:
                tr["nack_at"] = 0
                tr["bytes"] = []
            txs.append(tr)
        stretch = {int(k): float(r.uniform(2, 50)) / freq for k in r.integers(0, 60, 3)} if r.random() < 0.3 else None
        if self.stress == "freqhop":
            t = r.uniform(0.2e-3, 1e-3)
            for tr in txs:
                f_i = freq * float(np.exp(r.uniform(np.log(0.25), np.log(4))))
                t = gen.i2c(scl, sda, t, [tr], f_i, gap=r.uniform(20e-6, 500e-6))
        else:
            t = gen.i2c(scl, sda, r.uniform(0.2e-3, 1e-3), txs, freq, stretch=stretch, gap=r.uniform(20e-6, 500e-6))
        self.buses.append({"protocol": "i2c", "roles": {"scl": scl_name(idx), "sda": sda_name(idx)},
                           "params": {"clock_hz": freq},
                           "expect": {"transactions": [{"addr": x["addr"], "rw": x["rw"], "addr_ack": x.get("nack_at") != 0,
                                                        "bytes": x["bytes"]} for x in txs]}})
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_spi(self, idx: int) -> None:
        r = self.rng
        freq = float(r.choice([100e3, 500e3, 1e6, 4e6, 8e6])) * (1 + r.uniform(-0.02, 0.02))
        mode = int(r.integers(0, 4))
        order = str(r.choice(["msb", "msb", "msb", "lsb"]))
        has_cs = bool(r.random() < 0.85)
        has_miso = bool(r.random() < 0.7)
        clk = self.wave(f"spi{idx}_clk", mode >> 1)
        mosi = self.wave(f"spi{idx}_mosi", 0)
        miso = self.wave(f"spi{idx}_miso", 0) if has_miso else None
        cs = self.wave(f"spi{idx}_cs", 1) if has_cs else None
        frames = []
        for _ in range(int(r.integers(3, 15))):
            n = int(r.integers(1, 9))
            frames.append((bytes(r.integers(0, 256, n, dtype=np.uint8)),
                           bytes(r.integers(0, 256, n, dtype=np.uint8)) if has_miso else b""))
        if self.stress == "freqhop":
            t = r.uniform(0.1e-3, 0.5e-3)
            for fr in frames:
                f_i = freq * float(np.exp(r.uniform(np.log(0.25), np.log(4))))
                t = gen.spi(clk, mosi, miso, cs, t, [fr], f_i, mode, order, gap=r.uniform(5, 200) / freq)
        else:
            t = gen.spi(clk, mosi, miso, cs, r.uniform(0.1e-3, 0.5e-3), frames, freq, mode, order,
                        gap=r.uniform(5, 200) / freq)
        roles = {"clk": clk_name(idx), "mosi": f"spi{idx}_mosi"}
        if has_miso:
            roles["miso"] = f"spi{idx}_miso"
        if has_cs:
            roles["cs"] = f"spi{idx}_cs"
        self.buses.append({"protocol": "spi", "roles": roles,
                           "params": {"clock_hz": freq, "mode": mode, "bit_order": order},
                           "expect": {"mosi": [f[0].hex() for f in frames], "miso": [f[1].hex() for f in frames]}})
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_uart_upper(self, idx: int) -> None:
        """UART carrying a known upper protocol (or none), 8N1."""
        from .analyzers.upper import crc16_modbus
        r = self.rng
        baud = float(r.choice([4800, 9600, 19200, 38400, 115200])) * (1 + r.uniform(-0.01, 0.01))
        kind = str(r.choice(["nmea", "modbus", "text", "binary"]))
        name = f"uart{idx}_tx"
        w = self.wave(name, 1)
        t = r.uniform(0.5e-3, 2e-3)
        sent, frames = b"", []
        for _ in range(int(r.integers(3, 8))):
            if kind == "nmea":
                body = f"GPGGA,{int(r.integers(0, 235959)):06d},{r.uniform(0, 90):08.3f},N,{r.uniform(0, 180):09.3f},E,1,08,0.9,{r.uniform(0, 999):.1f},M,,,"
                cs = 0
                for ch in body.encode():
                    cs ^= ch
                data = f"${body}*{cs:02X}\r\n".encode()
                gaps = 0.0
            elif kind == "modbus":
                pdu = bytes([int(r.integers(1, 248)), int(r.choice([3, 4, 6, 16]))]) + bytes(r.integers(0, 256, int(r.integers(4, 12)), dtype=np.uint8))
                crc = crc16_modbus(pdu)
                data = pdu + bytes([crc & 0xFF, crc >> 8])
                frames.append(data.hex())
                gaps = [0.0] * (len(data) - 1) + [45.0]  # >= 3.5 characters of silence after each frame
            elif kind == "text":
                data = _payload(r, int(r.integers(10, 40)), True)
                gaps = 0.0
            else:
                data = _payload(r, int(r.integers(10, 40)), False)
                gaps = 0.0
            t = gen.uart(w, t, data, baud, gap_bits=gaps)
            sent += data
            t += r.uniform(20, 200) / baud * 10
        p = {"baud": baud, "data_bits": 8, "parity": "none", "stop_bits": 1, "idle": 1, "bit_order": "lsb", "payload": kind}
        self.buses.append({"protocol": "uart", "roles": {"data": name}, "params": p,
                           "expect": {"bytes": sent.hex(), **({"modbus": frames} if frames else {})}})
        self.fastest = max(self.fastest, baud)
        self.t_end = max(self.t_end, t)

    def add_scpi(self, idx: int) -> None:
        """Host <-> instrument over two UART lines: queries get a response after a delay."""
        r = self.rng
        baud = float(r.choice([9600, 19200, 38400, 57600, 115200])) * (1 + r.uniform(-0.01, 0.01))
        tx, rx = self.wave(f"scpi{idx}_tx", 1), self.wave(f"scpi{idx}_rx", 1)
        dialog = [("*IDN?", "ACME,MODEL-1,SN0042,1.0.3"), ("*RST", None), ("CONF:VOLT:DC 10", None),
                  ("MEAS:VOLT:DC?", "+1.23456E+00"), ("SYST:ERR?", '+0,"No error"'), ("TRIG:SOUR BUS", None),
                  ("READ?", "-4.20000E-03"), ("MEAS:CURR:DC?", "+2.50000E-01"), ("*OPC?", "1")]
        t = r.uniform(0.5e-3, 2e-3)
        exchanges = []
        for _ in range(int(r.integers(3, 9))):
            cmd, resp = dialog[int(r.integers(len(dialog)))]
            t = gen.uart(tx, t, (cmd + "\n").encode(), baud)
            if resp is not None:
                t += r.uniform(0.3e-3, 5e-3)
                t = gen.uart(rx, t, (resp + "\n").encode(), baud)
            exchanges.append([cmd, resp])
            t += r.uniform(1e-3, 10e-3)
        p = {"baud": baud, "data_bits": 8, "parity": "none", "stop_bits": 1, "idle": 1, "bit_order": "lsb"}
        sent_tx = "".join(c + "\n" for c, _ in exchanges).encode()
        sent_rx = "".join(x + "\n" for _, x in exchanges if x is not None).encode()
        self.buses.append({"protocol": "uart", "roles": {"data": f"scpi{idx}_tx"}, "params": p, "expect": {"bytes": sent_tx.hex()}})
        self.buses.append({"protocol": "uart", "roles": {"data": f"scpi{idx}_rx"}, "params": p, "expect": {"bytes": sent_rx.hex()}})
        self.buses.append({"protocol": "scpi", "roles": {"tx": f"scpi{idx}_tx", "rx": f"scpi{idx}_rx"}, "params": {"baud": baud},
                           "expect": {"exchanges": exchanges}})
        self.fastest = max(self.fastest, baud)
        self.t_end = max(self.t_end, t)

    def add_spinor(self, idx: int) -> None:
        """An MCU talking to a JEDEC SPI NOR flash: RDID, RDSR, WREN, READ, PP, SE, SFDP."""
        r = self.rng
        freq = float(r.choice([1e6, 4e6, 8e6])) * (1 + r.uniform(-0.02, 0.02))
        mode = int(r.choice([0, 3]))
        clk, mosi = self.wave(f"nor{idx}_clk", mode >> 1), self.wave(f"nor{idx}_mosi", 0)
        miso, cs = self.wave(f"nor{idx}_miso", 0), self.wave(f"nor{idx}_cs", 1)
        maker, dev = [(0xEF, 0x4018), (0xC2, 0x2017), (0xC8, 0x4016)][int(r.integers(3))]
        seq = [("RDID", bytes([0x9F, 0, 0, 0]), bytes([0xFF, maker, dev >> 8, dev & 0xFF]))]
        for _ in range(int(r.integers(3, 8))):
            op = str(r.choice(["READ", "RDSR", "WREN", "PP", "SE", "SFDP"]))
            addr = int(r.integers(0, 1 << 24)) & 0xFFFF00
            ab = addr.to_bytes(3, "big")
            if op == "READ":
                n = int(r.integers(4, 32))
                seq.append((op, bytes([0x03]) + ab + bytes(n), bytes(4) + bytes(r.integers(0, 256, n, dtype=np.uint8))))
            elif op == "RDSR":
                seq.append((op, bytes([0x05, 0]), bytes([0xFF, int(r.integers(0, 4))])))
            elif op == "WREN":
                seq.append((op, bytes([0x06]), bytes([0xFF])))
            elif op == "PP":
                n = int(r.integers(4, 32))
                seq.append((op, bytes([0x02]) + ab + bytes(r.integers(0, 256, n, dtype=np.uint8)), bytes(4 + n)))
            elif op == "SE":
                seq.append((op, bytes([0x20]) + ab, bytes(4)))
            else:
                seq.append((op, bytes([0x5A, 0, 0, 0, 0]) + bytes(8), bytes(5) + b"SFDP" + bytes([6, 1, 1, 0xFF])))
        frames = [(m, s_) for _, m, s_ in seq]
        t = gen.spi(clk, mosi, miso, cs, r.uniform(0.1e-3, 0.5e-3), frames, freq, mode, "msb", gap=r.uniform(5, 50) / freq)
        self.buses.append({"protocol": "spi", "roles": {"clk": f"nor{idx}_clk", "mosi": f"nor{idx}_mosi", "miso": f"nor{idx}_miso",
                                                        "cs": f"nor{idx}_cs"},
                           "params": {"clock_hz": freq, "mode": mode, "bit_order": "msb"},
                           "expect": {"mosi": [f[0].hex() for f in frames], "miso": [f[1].hex() for f in frames]},
                           "device": {"name": "SPI NOR flash", "jedec": f"{maker:02x}{dev:04x}", "commands": [x[0] for x in seq]}})
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_lin(self, idx: int) -> None:
        r = self.rng
        baud = float(r.choice([9600, 19200, 10417])) * (1 + r.uniform(-0.01, 0.01))
        name = f"lin{idx}"
        w = self.wave(name, 1)
        frames = [(int(r.integers(0, 60)), bytes(r.integers(0, 256, int(r.integers(1, 9)), dtype=np.uint8)))
                  for _ in range(int(r.integers(3, 10)))]
        t = gen.lin(w, r.uniform(0.5e-3, 2e-3), frames, baud)
        self.buses.append({"protocol": "lin", "roles": {"data": name}, "params": {"baud": baud},
                           "expect": {"frames": [[i, d.hex()] for i, d in frames]}})
        self.fastest = max(self.fastest, baud)
        self.t_end = max(self.t_end, t)

    def add_dmx(self, idx: int) -> None:
        r = self.rng
        name = f"dmx{idx}"
        w = self.wave(name, 1)
        packets = [bytes(r.integers(0, 256, int(r.integers(8, 64)), dtype=np.uint8)) for _ in range(int(r.integers(2, 6)))]
        t = gen.dmx(w, r.uniform(0.2e-3, 1e-3), packets)
        self.buses.append({"protocol": "dmx512", "roles": {"data": name}, "params": {"baud": 250000.0},
                           "expect": {"packets": [p.hex() for p in packets]}})
        self.fastest = max(self.fastest, 250000.0)
        self.t_end = max(self.t_end, t)

    def add_swd(self, idx: int) -> None:
        r = self.rng
        freq = float(r.choice([100e3, 1e6, 4e6, 10e6])) * (1 + r.uniform(-0.02, 0.02))
        clk, dio = self.wave(f"swd{idx}_clk", 0), self.wave(f"swd{idx}_dio", 0)
        packets = []
        for _ in range(int(r.integers(4, 20))):
            ack = int(r.choice([1, 1, 1, 1, 1, 2, 4]))
            packets.append({"apndp": int(r.integers(0, 2)), "rnw": int(r.integers(0, 2)), "a": int(r.integers(0, 4)),
                            "ack": ack, "data": int(r.integers(0, 2**32)) if ack == 1 else None})
        idle = int(r.choice([0, 0, 2, 8]))
        t = gen.swd(clk, dio, r.uniform(0.1e-3, 0.5e-3), packets, freq, idle_clocks=idle, gap=r.uniform(5, 100) / freq)
        self.buses.append({"protocol": "swd", "roles": {"swclk": f"swd{idx}_clk", "swdio": f"swd{idx}_dio"},
                           "params": {"clock_hz": freq, "idle_clocks": idle},
                           "expect": {"packets": [[p["apndp"], p["rnw"], p["a"], p["ack"], p["data"]] for p in packets]}})
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_can(self, idx: int) -> None:
        r = self.rng
        rate = float(r.choice([125e3, 250e3, 500e3, 1e6])) * (1 + r.uniform(-0.005, 0.005))
        name = f"can{idx}"
        w = self.wave(name, 1)
        frames = []
        for _ in range(int(r.integers(3, 15))):
            ext = bool(r.random() < 0.3)
            frames.append({"id": int(r.integers(0, 2**29 if ext else 2**11)), "ext": ext,
                           "data": [int(x) for x in r.integers(0, 256, int(r.integers(0, 9)))], "ack": bool(r.random() < 0.9)})
        t = gen.can(w, r.uniform(0.2e-3, 1e-3), frames, rate, ifs_bits=float(r.choice([3, 3, 10, 50])))
        self.buses.append({"protocol": "can", "roles": {"data": name}, "params": {"bitrate": rate},
                           "expect": {"frames": [[f["id"], f["ext"], bytes(f["data"]).hex()] for f in frames]}})
        self.fastest = max(self.fastest, rate)
        self.t_end = max(self.t_end, t)

    def add_decoy(self, idx: int) -> None:
        r = self.rng
        kind = str(r.choice(["static", "static", "pwm", "clock", "random", "burst_clock"]))
        name = f"decoy{idx}_{kind}"
        w = self.wave(name, int(r.integers(0, 2)) if kind == "static" else 0)
        info = {"kind": kind, "channel": name}
        if kind == "pwm":
            f, d = float(r.uniform(500, 50e3)), float(r.uniform(0.05, 0.95))
            info.update(freq=f, duty=d)
            self._deferred.append(lambda t1, w=w, f=f, d=d: gen.pwm(w, 0, t1, f, d))
        elif kind == "clock":
            f = float(r.choice([32768, 1e6, 12e6, 100e3, 115200]))
            info.update(freq=f)
            self._deferred.append(lambda t1, w=w, f=f: gen.pwm(w, 0, t1, f, 0.5))
        elif kind == "burst_clock":
            f = float(r.uniform(10e3, 1e6))
            info.update(freq=f)

            def burst(t1, w=w, f=f):
                t = 0.0
                while t < t1:
                    n = int(r.integers(8, 64))
                    gen.pwm(w, t, min(t1, t + n / f), f, 0.5)
                    w.set(min(t1, t + n / f), 0)
                    t += n / f + r.uniform(20, 200) / f
            self._deferred.append(burst)
        elif kind == "random":
            rt = float(r.uniform(100, 50e3))
            info.update(toggle_rate=rt)
            self._deferred.append(lambda t1, w=w, rt=rt: gen.random_toggles(w, 0, t1, rt, r))
        self.decoys.append(info)

    _deferred: list


def scl_name(i): return f"i2c{i}_scl"
def sda_name(i): return f"i2c{i}_sda"
def clk_name(i): return f"spi{i}_clk"


STRESS = {
    "glitch": {"glitches": 0.002},     # 1-sample spikes, per edge of the channel
    "midstart": {"cut": 0.3},          # capture starts 0-30 % into the traffic
    "lowrate": {"oversample": [3, 4]}, # barely resolvable
    "jitter": {"jitter": 0.3},         # timing jitter in samples (std)
    "freqhop": {},                     # SPI / I2C clock changes per frame (x0.25..x4)
    "baudhop": {},                     # UART baud rate switches mid-stream (x2/x4/x8)
}


def scenario(seed: int, profile: str = "mixed", stress: str | None = None) -> tuple[Capture, dict]:
    rng = np.random.default_rng(seed)
    b = Builder(rng, stress)
    b._deferred = []
    kinds = {"mixed": ["uart", "i2c", "spi"], "uart": ["uart"], "i2c": ["i2c"], "spi": ["spi"],
             "uartlike": ["uart", "lin", "dmx"], "duplex": ["scpi", "scpi", "uart"],
             "upper": ["uart_upper"], "spinor": ["spinor", "spi"],
             "swd": ["swd"], "can": ["can"], "swdcan": ["swd", "can", "spi", "uart"]}[profile]
    n_bus = int(rng.integers(1, 4)) if profile in ("mixed", "swdcan") else 1
    for i in range(n_bus):
        getattr(b, "add_" + str(rng.choice(kinds)))(i)
    for i in range(int(rng.integers(0, 4))):
        b.add_decoy(i)
    t_end = b.t_end + rng.uniform(0.2e-3, 2e-3)
    for f in b._deferred:
        f(t_end)
    st = STRESS.get(stress or "", {})
    oversample = float(rng.choice(st.get("oversample", [4, 6, 8, 10, 16, 25, 50, 100])))
    rate = min(max(b.fastest * oversample, 1e6), 200e6)
    rate = float(min([1e6, 2e6, 4e6, 8e6, 10e6, 12e6, 16e6, 20e6, 24e6, 25e6, 50e6, 100e6, 200e6],
                     key=lambda x: (x < rate, abs(x - rate))))
    jitter = float(st.get("jitter", rng.choice([0, 0, 0.02, 0.1]))) / rate
    glitches = None
    if "glitches" in st:
        glitches = {k: int(rng.poisson(max(1, len(w.times)) * st["glitches"])) + 1 for k, w in b.waves.items()
                    if w.times}
    t_start = float(rng.uniform(0, st["cut"]) * b.t_end) if "cut" in st else 0.0
    cap = gen.quantize(b.waves, rate, t_end, rng, jitter_s=jitter, glitches=glitches, t_start=t_start)
    anon, mapping = anonymize(cap, seed)
    back = {v: k for k, v in mapping.items()}

    def rename(bus):
        bus = dict(bus)
        bus["roles"] = {k: back[v] for k, v in bus["roles"].items()}
        return bus

    truth = {
        "id": f"synth-{profile}{'-' + stress if stress else ''}-{seed}",
        "generator": {"seed": seed, "profile": profile, "stress": stress, "t_start": t_start, "rate": rate, "jitter_samples": jitter * rate,
                      "oversample_vs_fastest": rate / b.fastest if b.fastest else None},
        "channels": {k: {"original": v, "static": len(anon.channel(k).edges) == 0} for k, v in mapping.items()},
        "buses": [rename(x) for x in b.buses],
        "decoys": [{**d, "channel": back[d["channel"]]} for d in b.decoys],
    }
    return anon, truth
