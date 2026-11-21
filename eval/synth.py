"""Random scenarios for the evaluation corpus.

Each scenario combines one to three buses with decoy lines, renders them at a
sample rate chosen relative to the fastest bus, anonymizes the channels and
returns (capture, truth). Scenarios are fully determined by their seed. The waveforms come from the generator
wireskein's own tests use (wireskein._engine.gen); the capture is what the decoders read (wireskein.decode.analyze).
"""

from __future__ import annotations

import numpy as np

from wireskein._engine import gen
from wireskein._engine.model import Capture, Channel


def anonymize(cap: Capture, seed: int) -> tuple[Capture, dict[str, str]]:
    """Rename channels to D0..Dn in a seeded random order (analysis may rely neither on names nor on their order).
    Returns the new capture and a map anonymous name -> original name."""
    rng = np.random.default_rng(seed)
    order = rng.permutation(len(cap.channels))
    channels, mapping = [], {}
    for new_idx, old_idx in enumerate(order):
        old = cap.channels[old_idx]
        name = f"D{new_idx}"
        channels.append(Channel(name, old.initial, old.edges, old.step, old.phase))
        mapping[name] = old.name
    return Capture(cap.rate, cap.n_samples, channels), mapping

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
            # stress "nack": most addresses answer no one (a bus scan, a device not there)
            if r.random() < (0.7 if self.stress == "nack" else 0.1):
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

    def add_spi_multi(self, idx: int) -> None:
        """One SPI bus (CLK, MOSI, MISO shared) with 2 or 3 targets, each on its own CS, their transfers interleaved:
        one bus per target in the truth, the shared lines in each, told together by `bus`."""
        r = self.rng
        freq = float(r.choice([100e3, 500e3, 1e6, 4e6, 8e6])) * (1 + r.uniform(-0.02, 0.02))
        mode = int(r.integers(0, 4))
        order = str(r.choice(["msb", "msb", "msb", "lsb"]))
        n_cs = int(r.integers(2, 4))
        clk = self.wave(f"spi{idx}_clk", mode >> 1)
        mosi = self.wave(f"spi{idx}_mosi", 0)
        miso = self.wave(f"spi{idx}_miso", 0)
        css = [self.wave(f"spi{idx}_cs{k}", 1) for k in range(n_cs)]
        sent: list[list] = [[] for _ in range(n_cs)]
        t = r.uniform(0.1e-3, 0.5e-3)
        for _ in range(int(r.integers(6, 24))):
            k = int(r.integers(0, n_cs))
            n = int(r.integers(1, 9))
            fr = (bytes(r.integers(0, 256, n, dtype=np.uint8)), bytes(r.integers(0, 256, n, dtype=np.uint8)))
            sent[k].append(fr)
            t = gen.spi(clk, mosi, miso, css[k], t, [fr], freq, mode, order, gap=r.uniform(5, 200) / freq)
        for k in range(n_cs):
            if not sent[k]:
                continue
            self.buses.append({"protocol": "spi", "bus": f"spi{idx}",
                               "roles": {"clk": clk_name(idx), "mosi": f"spi{idx}_mosi", "miso": f"spi{idx}_miso",
                                         "cs": f"spi{idx}_cs{k}"},
                               "params": {"clock_hz": freq, "mode": mode, "bit_order": order},
                               "expect": {"mosi": [f[0].hex() for f in sent[k]], "miso": [f[1].hex() for f in sent[k]]}})
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_spi_variant(self, idx: int, variant: str) -> None:
        """One SPI bus in one of the shapes the spec allows (SPI_VARIANTS), its truth as the evidence can tell it: a
        line that never moves is no CS (nothing shows it selects), and a target whose CS is not captured is a
        truth bus with no CS (the bus's transfers outside the captured selections); `variant` names the shape."""
        r = self.rng
        freq = float(r.choice([100e3, 500e3, 1e6, 4e6, 8e6])) * (1 + r.uniform(-0.02, 0.02))
        mode = int(r.integers(0, 4))
        order = str(r.choice(["msb", "msb", "msb", "lsb"]))
        has_miso = bool(r.random() < 0.7)
        clk = self.wave(f"spi{idx}_clk", mode >> 1)
        mosi = self.wave(f"spi{idx}_mosi", 0)
        miso = self.wave(f"spi{idx}_miso", 0) if has_miso else None
        n_t = {"multi": int(r.integers(2, 4)), "uncaptured": 2}.get(variant, 1)
        active = 1 if variant == "cs_high" else 0
        cs = [None] * n_t
        if variant in ("cs", "cs_high", "sessions", "session1", "empty", "multi", "uncaptured"):
            cs = [self.wave(f"spi{idx}_cs{k}", 1 - active) for k in range(n_t)]
            if variant == "uncaptured":
                cs[1] = None                                   # the other target's CS is not on the probe
                del self.waves[f"spi{idx}_cs1"]
        for k in range({"cs_tied": 1, "cs_tied2": 2}.get(variant, 0)):
            self.wave(f"spi{idx}_tied{k}", 0)                  # captured, held low (to GND): never moves
        sent: list[list] = [[] for _ in range(n_t)]
        t = r.uniform(0.1e-3, 0.5e-3)
        n_frames = int(r.integers(4, 16))
        k_of = [int(r.integers(0, n_t)) for _ in range(n_frames)]
        # sessions: one selection holding several transfers with gaps between them (2 to 4 selections; session1: one)
        session = variant in ("sessions", "session1")
        cuts = set(r.choice(np.arange(1, n_frames), int(r.integers(1, 4)), replace=False).tolist()) \
            if variant == "sessions" else set()
        held: list[list] = []
        for i in range(n_frames):
            k = k_of[i]
            if session and (i == 0 or i in cuts):
                if i:
                    cs[0].set(t, 1 - active)
                    t += r.uniform(20, 200) / freq
                cs[0].set(t, active)
                t += r.uniform(1, 4) / freq
                held.append([])
            n = int(r.integers(1, 9))
            fr = (bytes(r.integers(0, 256, n, dtype=np.uint8)), bytes(r.integers(0, 256, n, dtype=np.uint8)) if has_miso else b"")
            sent[k].append(fr)
            if session:
                held[-1].append(fr)
            gap = r.uniform(5, 200) / freq
            if variant == "empty" and r.random() < 0.3:            # a selection with no clock in it
                cs[k].set(t, active)
                cs[k].set(t + r.uniform(2, 20) / freq, 1 - active)
                t += r.uniform(25, 60) / freq
            own = cs[k] if not session else None
            if own is not None:
                own.set(t, active)
            t = gen.spi(clk, mosi, miso, None, t, [fr], freq, mode, order, gap=gap)
            if own is not None:
                own.set(t - gap, 1 - active)
        if session:
            cs[0].set(t, 1 - active)
            t += r.uniform(5, 50) / freq
        for k in range(n_t):
            if not sent[k]:
                continue
            roles = {"clk": clk_name(idx), "mosi": f"spi{idx}_mosi"}
            if has_miso:
                roles["miso"] = f"spi{idx}_miso"
            if cs[k] is not None:
                roles["cs"] = f"spi{idx}_cs{k}"
            params = {"clock_hz": freq, "mode": mode, "bit_order": order}
            if active:
                params["cs_active"] = 1
            bus = {"protocol": "spi", "roles": roles, "params": params, "variant": variant,
                   "expect": {"mosi": [f[0].hex() for f in sent[k]], "miso": [f[1].hex() for f in sent[k]]}}
            if session:                                        # a selection's transfers are one frame
                bus["expect"] = {"mosi": ["".join(f[0].hex() for f in h) for h in held],
                                 "miso": ["".join(f[1].hex() for f in h) for h in held]}
            if variant == "session1":
                # one selection around all the traffic: nothing tells it from any line that changed once before and
                # once after; the CS may be left out (the bytes then read as the clock's bursts frame them)
                bus["cs_optional"] = True
            if n_t > 1:
                bus["bus"] = f"spi{idx}"
            self.buses.append(bus)
        self.fastest = max(self.fastest, freq)
        self.t_end = max(self.t_end, t)

    def add_uart_upper(self, idx: int) -> None:
        """UART carrying a known upper protocol (or none), 8N1."""
        from wireskein._engine.analyzers.upper import crc16_modbus
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

    def add_onewire(self, idx: int) -> None:
        """A 1-Wire master at standard speed: resets each with a presence pulse (now and then none: no slave
        answered), then bytes written LSB first; slot, low times varied within the spec."""
        r = self.rng
        name = f"onewire{idx}"
        w = self.wave(name, 1)
        slot, low_0, low_1 = float(r.uniform(65e-6, 110e-6)), float(r.uniform(60e-6, 64e-6)), float(r.uniform(3e-6, 12e-6))
        t = float(r.uniform(0.2e-3, 2e-3))
        txs = []
        for _ in range(int(r.integers(2, 7))):
            data = bytes(r.integers(0, 256, int(r.integers(1, 10)), dtype=np.uint8))
            presence = bool(r.random() < 0.9)
            t = gen.onewire(w, t, data, presence=presence, reset=float(r.uniform(480e-6, 600e-6)), low_0=low_0,
                            low_1=low_1, slot=slot)
            t += float(r.uniform(0.1e-3, 2e-3))
            txs.append({"presence": presence, "data": data.hex()})
        # shortest_s: the shortest pulse the line has (what a noise must stay clear of: bench/wsbench/data.hardness)
        self.buses.append({"protocol": "onewire", "roles": {"dq": name}, "params": {"speed": "standard", "shortest_s": low_1},
                           "expect": {"resets": txs}})
        self.fastest = max(self.fastest, 1 / (2 * low_1))
        self.t_end = max(self.t_end, t)

    def add_rgb(self, idx: int) -> None:
        """A WS2812-like LED string: frames of 3-byte pixels, each followed by a reset low."""
        r = self.rng
        name = f"rgb{idx}"
        w = self.wave(name, 0)
        period = float(r.choice([1.25e-6, 2.5e-6]))
        t = float(r.uniform(0.1e-3, 1e-3))
        frames = []
        for _ in range(int(r.integers(2, 6))):
            data = bytes(r.integers(0, 256, 3 * int(r.integers(1, 9)), dtype=np.uint8))
            t = gen.rgb_led(w, t, data, t0h=0.32 * period, t1h=0.64 * period, period=period,
                            reset=float(r.uniform(60e-6, 300e-6)))
            frames.append(data.hex())
        self.buses.append({"protocol": "rgb_led", "roles": {"data": name}, "params": {"period": period, "shortest_s": 0.32 * period},
                           "expect": {"frames": frames}})
        self.fastest = max(self.fastest, 1 / (0.32 * period))
        self.t_end = max(self.t_end, t)

    def add_nec(self, idx: int) -> None:
        """An NEC remote code: the LED's drive with a 38 kHz carrier (idle low), or a receiver's output (idle
        high); commands with their complements, now and then a repeat code."""
        r = self.rng
        carrier = 38e3 if r.random() < 0.5 else None
        name = f"ir{idx}"
        w = self.wave(name, 0 if carrier else 1)
        frames = []
        for _ in range(int(r.integers(2, 6))):
            frames.append({"addr": int(r.integers(0, 256)), "cmd": int(r.integers(0, 256))})
            if r.random() < 0.3:
                frames.append({"repeat": True})
        t = gen.nec(w, float(r.uniform(1e-3, 5e-3)), frames, carrier_hz=carrier)
        self.buses.append({"protocol": "ir_nec", "roles": {"data": name}, "params": {"carrier_hz": carrier,
                                                                    "shortest_s": 0.5 / carrier if carrier else 562.5e-6},
                           "expect": {"frames": [[f["addr"], f["cmd"]] for f in frames if not f.get("repeat")]}})
        self.fastest = max(self.fastest, 2 * (carrier or 1 / 562.5e-6))
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

    # --- the protocols added in 2026-11: their truth compared field by field (eval/evaluate.py's generic match) ---

    def _generic(self, protocol, roles, layer, fields, items, check=None, fastest=0.0, t=0.0, params=None):
        self.buses.append({"protocol": protocol, "roles": roles, "params": {**(params or {}), "check": check or {}},
                           "expect": {"layer": layer, "fields": fields, "items": items}})
        self.fastest = max(self.fastest, fastest)
        self.t_end = max(self.t_end, t)

    def _t0(self):
        return float(self.rng.uniform(0.05e-3, 0.5e-3))

    def add_cec(self, idx: int) -> None:
        r = self.rng
        name = f"cec{idx}"
        w = self.wave(name, 1)
        msgs = []
        for _ in range(int(r.integers(3, 7))):
            ini, dst = int(r.integers(0, 15)), int(r.choice([0, 4, 5, 15]))
            n = int(r.choice([0, 1, 1, 2, 3]))
            msgs.append([ini << 4 | dst] + [int(x) for x in r.integers(0, 256, n)])
        t = gen.cec(w, self._t0(), msgs, gap=float(r.uniform(5e-3, 20e-3)))
        items = [[m[0] >> 4, m[0] & 0xF, m[1] if len(m) > 1 else None, bytes(m[2:]).hex() or None]
                 for m in msgs]
        self._generic("cec", {"cec": name}, "messages", ["initiator", "destination", "opcode", "data"], items,
                      fastest=1 / 0.6e-3, t=t)

    def add_swim(self, idx: int) -> None:
        r = self.rng
        name = f"swim{idx}"
        w = self.wave(name, 1)
        ops = []
        for _ in range(int(r.integers(2, 6))):
            cmd = "ROTF" if r.random() < 0.6 else "WOTF"
            ops.append({"cmd": cmd, "addr": int(r.choice([0x7F80, 0x4800, 0x8000, 0x5000])) + int(r.integers(0, 64)),
                        "data": [int(x) for x in r.integers(0, 256, int(r.integers(1, 9)))]})
        t = gen.swim(w, self._t0(), ops, gap=float(r.uniform(20e-6, 200e-6)))
        items = [[o["cmd"], len(o["data"]), o["addr"], bytes(o["data"]).hex()] for o in ops]
        self._generic("swim", {"swim": name}, "transactions", ["cmd", "count", "addr", "data"], items,
                      fastest=1 / (2 * gen.SWIM_CLK), t=t)

    def add_dali(self, idx: int) -> None:
        r = self.rng
        name = f"dali{idx}"
        w = self.wave(name, 1)
        frames, items = [], []
        for _ in range(int(r.integers(3, 8))):
            a = int(r.integers(0, 64)) << 1 | 1
            cmd = int(r.integers(144, 200))
            frames.append((a << 8 | cmd, 16, float(r.uniform(2.5e-3, 8e-3))))
            items.append(["forward", a, cmd])
            if r.random() < 0.7:
                ans = int(r.integers(0, 256))
                frames.append((ans, 8, float(r.uniform(10e-3, 30e-3))))
                items.append(["backward", None, ans])
        t = gen.dali(w, self._t0(), frames)
        self._generic("dali", {"dali": name}, "frames", ["kind", "addr_byte", "data"], items, fastest=2400, t=t)

    def add_dcc(self, idx: int) -> None:
        r = self.rng
        name = f"dcc{idx}"
        w = self.wave(name, 0)
        pk = [[int(r.integers(1, 128)), int(r.integers(0x60, 0x80))] + ([int(r.integers(0, 256))] if r.random() < 0.3 else [])
              for _ in range(int(r.integers(4, 12)))]
        t = gen.dcc(w, self._t0(), pk, preamble=int(r.integers(14, 20)))
        items = []
        for d in pk:
            x = 0
            for v in d:
                x ^= v
            items.append([d[0], bytes(d + [x]).hex()])
        self._generic("dcc", {"track": name}, "packets", ["address", "bytes"], items, fastest=1 / 58e-6, t=t,
                      params={"ok_only": True})

    def add_sent(self, idx: int) -> None:
        r = self.rng
        name = f"sent{idx}"
        w = self.wave(name, 1)
        tick = float(r.choice([3e-6, 3e-6, 6e-6, 12e-6])) * (1 + r.uniform(-0.05, 0.05))
        n = int(r.choice([3, 4, 6, 6]))
        msgs = [[int(x) for x in r.integers(0, 16, n)] for _ in range(int(r.integers(8, 20)))]
        t = gen.sent(w, self._t0(), msgs, tick=tick)
        self._generic("sent", {"data": name}, "messages", ["data"], [[m] for m in msgs], fastest=1 / tick, t=t,
                      params={"ok_only": True})

    def add_j1850(self, idx: int) -> None:
        r = self.rng
        name = f"j1850_{idx}"
        w = self.wave(name, 0)
        frames = [bytes(r.integers(0, 256, int(r.integers(3, 9))).tolist()) for _ in range(int(r.integers(3, 8)))]
        t = gen.j1850(w, self._t0(), frames, offset=float(r.choice([0, 0, 10e-6, 20e-6])),
                      gap=float(r.uniform(0.5e-3, 3e-3)))
        items = [[(f + bytes([gen.j1850_crc8(f)])).hex()] for f in frames]
        self._generic("j1850", {"data": name}, "frames", ["bytes"], items, fastest=1 / 34e-6, t=t,
                      params={"ok_only": True})

    def add_wiegand(self, idx: int) -> None:
        r = self.rng
        d0, d1 = f"wg{idx}_d0", f"wg{idx}_d1"
        w0, w1 = self.wave(d0, 1), self.wave(d1, 1)
        cards = [(int(r.integers(0, 256)), int(r.integers(0, 65536))) for _ in range(int(r.integers(2, 6)))]
        width = float(r.choice([20e-6, 50e-6, 100e-6]))
        t = gen.wiegand(w0, w1, self._t0(), [gen.wiegand26(*c) for c in cards], width=width,
                        period=float(r.choice([0.5e-3, 1e-3, 2e-3])), gap=float(r.uniform(20e-3, 100e-3)))
        self._generic("wiegand", {"d0": d0, "d1": d1}, "frames", ["facility", "card"], [list(c) for c in cards],
                      fastest=1 / width, t=t)

    def add_ps2(self, idx: int) -> None:
        r = self.rng
        c, d = f"ps2_{idx}_clk", f"ps2_{idx}_data"
        wc, wd = self.wave(c, 1), self.wave(d, 1)
        codes = []
        for k in r.integers(1, 0x84, int(r.integers(3, 10))).tolist():
            codes += [k, 0xF0, k]
        f = float(r.uniform(10e3, 16.7e3))
        t = gen.ps2(wc, wd, self._t0(), bytes(codes), freq=f, gaps=[float(x) for x in r.uniform(0.3e-3, 5e-3, len(codes))])
        self._generic("ps2", {"clk": c, "data": d}, "frames", ["data"], [[x] for x in codes], fastest=2 * f, t=t)

    def add_usb(self, idx: int) -> None:
        r = self.rng
        speed = "full" if r.random() < 0.6 else "low"
        dp, dm = f"usb{idx}_dp", f"usb{idx}_dm"
        wp, wm = self.wave(dp, int(speed == "full")), self.wave(dm, int(speed == "low"))     # idle J
        pk = []
        frame = int(r.integers(0, 2000))
        for k in range(int(r.integers(3, 8))):
            if speed == "full" and r.random() < 0.5:
                pk.append({"pid": "SOF", "frame": (frame + k) & 0x7FF})
            addr, ep = int(r.integers(0, 128)), int(r.integers(0, 16))
            if r.random() < 0.5:
                pk.append({"pid": "IN", "addr": addr, "endp": ep})
                if r.random() < 0.3:
                    pk.append({"pid": "NAK"})
                    continue
            else:
                pk.append({"pid": "OUT", "addr": addr, "endp": ep})
            pk.append({"pid": "DATA1" if k % 2 else "DATA0", "data": bytes(r.integers(0, 256, int(r.integers(0, 17))).tolist())})
            pk.append({"pid": "ACK"})
        t = gen.usb(wp, wm, self._t0(), pk, speed, gap_bits=float(r.uniform(10, 200)))
        items = [[p["pid"], p.get("addr"), p.get("endp"), p.get("frame"),
                  bytes(p["data"]).hex() if "data" in p else None] for p in pk]
        self._generic("usb", {"dp": dp, "dm": dm}, "packets", ["pid", "addr", "endp", "frame", "data"], items,
                      check={"speed": speed}, fastest=12e6 if speed == "full" else 1.5e6, t=t)

    def add_usb_pd(self, idx: int) -> None:
        r = self.rng
        name = f"cc{idx}"
        w = self.wave(name, 0)
        msgs = []
        for k in range(int(r.integers(3, 8))):
            nobj = int(r.integers(0, 5))
            hdr = (nobj << 12) | ((k & 7) << 9) | 0x40 | (int(r.integers(1, 16)) if nobj == 0 else int(r.integers(1, 5)))
            msgs.append((hdr, [int(x) for x in r.integers(0, 2 ** 32, nobj, dtype=np.uint64)]))
        ui = 1 / (300e3 * (1 + r.uniform(-0.1, 0.1)))
        t = gen.usb_pd(w, self._t0(), msgs, ui=ui, skew=float(r.choice([0, 0, -0.2, -0.4])) * 1e-6,
                       gap=float(r.uniform(100e-6, 1e-3)))
        self._generic("usb_pd", {"cc": name}, "packets", ["header", "objects"], [[h, o] for h, o in msgs],
                      fastest=2 / ui, t=t, params={"ok_only": True})

    def add_spdif(self, idx: int) -> None:
        r = self.rng
        name = f"spdif{idx}"
        w = self.wave(name, 0)
        fs = float(r.choice([32000, 44100, 48000]))
        n = int(r.integers(24, 60))
        samples = [(int(a), int(b)) for a, b in r.integers(0, 1 << 24, (n, 2))]
        f0 = int(r.integers(0, 192))
        t = gen.spdif(w, self._t0(), samples, fs=fs, frame0=f0)
        items = []
        for f, (a, b) in enumerate(samples):
            items += [["B" if (f0 + f) % 192 == 0 else "M", a], ["W", b]]
        self._generic("spdif", {"data": name}, "subframes", ["preamble", "audio"], items, fastest=fs * 128, t=t,
                      params={"ok_only": True})

    def add_sdmmc(self, idx: int) -> None:
        r = self.rng
        c, d = f"sd{idx}_clk", f"sd{idx}_cmd"
        wc, wd = self.wave(c, 0), self.wave(d, 1)
        rca = int(r.integers(1, 0x10000))
        cid = int.from_bytes(r.bytes(16), "big")
        ocr = 0xC0FF8000
        seq = [(1, 0, 0, None), (1, 8, 0x1AA, (8, 0x1AA)), (1, 55, 0, (55, 0x120)), (1, 41, 0x40FF8000, "R3"),
               (1, 2, 0, "R2"), (1, 3, 0, (3, rca << 16 | 0x0500))]
        for _ in range(int(r.integers(2, 8))):
            k = int(r.choice([13, 16, 17, 7, 24]))
            seq.append((1, k, int(r.integers(0, 2 ** 32)) if k in (17, 24) else rca << 16, (k, int(r.integers(0, 2 ** 32)))))
        start = int(r.integers(0, 3))
        frames, items = [], []
        for dirn, k, arg, resp in seq[start:]:
            frames.append(gen.sd_frame(dirn, k, arg))
            items.append(["CMD", k, arg])
            if resp == "R3":
                frames.append(gen.sd_r3(ocr))
                items.append(["R3", None, ocr])
            elif resp == "R2":
                frames.append(gen.sd_r2(cid))
                items.append(["R2", None, None])
            elif resp is not None:
                frames.append(gen.sd_frame(0, *resp))
                items.append(["RSP", *resp])
        f = float(r.choice([400e3, 400e3, 1e6, 25e6]))
        t = gen.sdmmc(wc, wd, self._t0(), frames, freq=f, idle=int(r.integers(2, 40)))
        self._generic("sdmmc", {"clk": c, "cmd": d}, "frames", ["dir", "index", "arg"],
                      [[a, b, c_] for a, b, c_ in items], fastest=2 * f, t=t)
        # an R3's value is its OCR (the field "ocr"): the arg compared is that
        self.buses[-1]["expect"]["alias"] = {"arg": "ocr"}

    def _jtag_scans(self, ir_bits):
        r = self.rng
        scans = []
        for _ in range(int(r.integers(4, 17))):    # a debugger's session: scans on and on
            dr = int(r.choice([1, 8, 32, 32, 41]))
            scans.append({"type": "IR", "bits": ir_bits, "tdi": int(r.integers(0, 1 << ir_bits)),
                          "tdo": (int(r.integers(0, 1 << max(0, ir_bits - 2))) << 2) | 1})
            scans.append({"type": "DR", "bits": dr, "tdi": int(r.integers(0, 2 ** dr)), "tdo": int(r.integers(0, 2 ** dr))})
        return scans, [[s["type"], s["bits"], format(s["tdi"], "x"), format(s["tdo"], "x")] for s in scans]

    def add_jtag(self, idx: int) -> None:
        r = self.rng
        names = {k: f"jtag{idx}_{k}" for k in ("tck", "tms", "tdi", "tdo")}
        w = {k: self.wave(v, 0) for k, v in names.items()}
        freq = float(r.choice([100e3, 1e6, 4e6, 10e6]))
        scans, items = self._jtag_scans(int(r.choice([4, 5, 5, 9])))
        t = gen.jtag(w["tck"], w["tms"], w["tdi"], w["tdo"], self._t0(), scans, freq,
                     idle_clocks=int(r.integers(0, 8)), gap=float(r.uniform(1, 50)) / freq)
        self._generic("jtag", names, "scans", ["type", "bits", "tdi", "tdo"], items, fastest=freq, t=t)

    def add_cjtag(self, idx: int) -> None:
        r = self.rng
        k, m = f"cjtag{idx}_tckc", f"cjtag{idx}_tmsc"
        wk, wm = self.wave(k, 1), self.wave(m, 1)
        tck = 1 / float(r.choice([100e3, 1e6, 4e6]))
        scans, items = self._jtag_scans(int(r.choice([4, 5, 5, 9])))
        t = gen.cjtag(wk, wm, self._t0(), scans, tck=tck)
        self._generic("jtag", {"tckc": k, "tmsc": m}, "scans", ["type", "bits", "tdi", "tdo"], items,
                      check={"cjtag": "oscan1"}, fastest=5 / tck, t=t)

    def add_mdio(self, idx: int) -> None:
        r = self.rng
        c, d = f"mdio{idx}_mdc", f"mdio{idx}_md"
        wc, wd = self.wave(c, 0), self.wave(d, 1)
        freq = float(r.choice([1e6, 2.5e6, 400e3]))
        phy = int(r.integers(0, 32))
        frames = [{"op": int(r.choice([1, 2])), "phy": phy, "reg": int(r.integers(0, 32)), "data": int(r.integers(0, 2 ** 16))}
                  for _ in range(int(r.integers(4, 12)))]
        t = gen.mdio(wc, wd, self._t0(), frames, freq,
                     gap=float(r.uniform(2, 50)) / freq)
        items = [["W" if f["op"] == 1 else "R", f["phy"], f["reg"], f["data"]] for f in frames]
        # the PHY's data a tenth of a clock after the rise before the one that reads it: the rate resolves that
        self._generic("mdio", {"mdc": c, "mdio": d}, "frames", ["op", "phy", "reg", "data"], items, fastest=10 * freq, t=t)

    def add_i2s(self, idx: int) -> None:
        r = self.rng
        names = {"sck": f"i2s{idx}_bclk", "ws": f"i2s{idx}_ws", "sd": f"i2s{idx}_sd"}
        w = {k: self.wave(v, 0) for k, v in names.items()}
        fs = float(r.choice([8000, 16000, 44100, 48000]))
        if r.random() < 0.5:
            fmt, bits = str(r.choice(["i2s", "left", "right"])), int(r.choice([16, 24]))
            lim = 1 << (bits - 1)
            frames = [(int(a), int(b)) for a, b in r.integers(-lim, lim, (int(r.integers(30, 120)), 2))]
            t = gen.i2s(w["sck"], w["ws"], w["sd"], self._t0(), frames, fs, slot=32, bits=bits, fmt=fmt)
            self._generic("i2s", names, "frames", ["left", "right"], [list(f) for f in frames],
                          check={"format": fmt, "word_bits": bits}, fastest=fs * 128, t=t)
        else:
            n = int(r.choice([4, 6, 8]))
            slot, bits = [(32, 24), (32, 16), (16, 12)][int(r.integers(0, 3))]
            offset, width = [(1, 1), (0, slot)][int(r.integers(0, 2))]
            lim = 1 << (bits - 1)
            frames = [[int(x) for x in row] for row in r.integers(-lim, lim, (int(r.integers(20, 60)), n))]
            t = gen.tdm(w["sck"], w["ws"], w["sd"], self._t0(), frames, fs=fs, slot=slot, bits=bits, offset=offset,
                        fs_width=width)
            self._generic("i2s", names, "frames", ["slots"], [[f] for f in frames],
                          check={"format": "tdm", "slots_per_frame": n, "word_bits": bits}, fastest=fs * n * slot * 2, t=t)

    def add_dht(self, idx: int) -> None:
        r = self.rng
        name = f"dht{idx}"
        w = self.wave(name, 1)
        rd = []
        for _ in range(int(r.integers(2, 5))):
            if r.random() < 0.5:
                rd.append(bytes([int(r.integers(20, 90)), 0, int(r.integers(0, 40)), int(r.integers(0, 10))]))
            else:
                h, tc = int(r.integers(200, 900)), int(r.integers(-100, 400))
                rd.append(bytes([h >> 8, h & 0xFF, (abs(tc) >> 8) | (0x80 if tc < 0 else 0), abs(tc) & 0xFF]))
        t = gen.dht(w, self._t0(), rd, period=float(r.uniform(0.02, 0.1)))
        self._generic("dht", {"data": name}, "readings", ["bytes"], [[(b + bytes([sum(b) & 0xFF])).hex()] for b in rd],
                      fastest=1 / 27e-6, t=t)

    def add_can_fd(self, idx: int) -> None:
        r = self.rng
        nominal = float(r.choice([250e3, 500e3, 1e6]))
        ratio = int(r.choice([2, 4, 5, 8]))
        name = f"canfd{idx}"
        w = self.wave(name, 1)
        frames = []
        for _ in range(int(r.integers(3, 10))):
            fd = r.random() < 0.75
            ext = bool(r.random() < 0.3) and fd
            n = int(r.choice(gen.CAN_FD_LENGTHS[:13])) if fd else int(r.integers(0, 9))
            frames.append({"id": int(r.integers(0x800 if ext else 0, 2 ** 29 if ext else 2 ** 11)), "fd": fd,
                           "brs": bool(r.random() < 0.8), "data": [int(x) for x in r.integers(0, 256, n)]})
        t = gen.can_fd(w, self._t0(), frames, nominal, nominal * ratio, ifs_bits=float(r.choice([10, 10, 30])))
        self.buses.append({"protocol": "can", "roles": {"data": name}, "params": {"bitrate": nominal},
                           "expect": {"frames": [[f["id"], f["id"] > 0x7FF, bytes(f["data"]).hex()] for f in frames]}})
        self.fastest = max(self.fastest, nominal * ratio)
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


DECOY_MAX = 20000      # a decoy's changes at most (see scenario)

# the protocols each with a case of their own (profile "protocols"), and among other buses ("protomix")
PROTOCOLS = ("cec", "swim", "dali", "dcc", "sent", "j1850", "wiegand", "ps2", "usb", "usb_pd", "spdif", "sdmmc", "jtag",
             "cjtag", "mdio", "i2s", "dht", "can_fd")

SPI_VARIANTS = ("cs", "nocs", "cs_high", "cs_tied", "cs_tied2", "sessions", "session1", "multi", "uncaptured", "empty")


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
    "nack": {},                        # I2C: 7 in 10 transactions an address no one answers (a bus scan)
}


def scenario(seed: int, profile: str = "mixed", stress: str | None = None,
             rate: float | None = None) -> tuple[Capture, dict]:
    """rate: sample at this rate instead of the seed's (the random draws are the same either way, so the traffic is
    too): the same scenario taken at several rates."""
    rate_given = rate
    rng = np.random.default_rng(seed)
    b = Builder(rng, stress)
    b._deferred = []
    kinds = {"mixed": ["uart", "i2c", "spi"], "uart": ["uart"], "i2c": ["i2c"], "spi": ["spi"],
             "uartlike": ["uart", "lin", "dmx"], "duplex": ["scpi", "scpi", "uart"],
             "upper": ["uart_upper"], "spinor": ["spinor", "spi"],
             "swd": ["swd"], "can": ["can"], "swdcan": ["swd", "can", "spi", "uart"],
             "pulse": ["onewire", "rgb", "uart"], "ir": ["nec"],
             "spimulti": ["spi_multi"], "spimultimix": ["spi_multi", "uart", "i2c"], "spivar": [],
             "protocols": [], "protomix": list(PROTOCOLS) + ["uart", "i2c", "spi", "can", "swd", "onewire", "nec"]}[profile]
    n_bus = (int(rng.integers(1, 4)) if profile in ("mixed", "swdcan", "spimultimix", "pulse")
             else int(rng.integers(2, 4)) if profile == "protomix" else 0 if profile in ("spivar", "protocols") else 1)
    for i in range(n_bus):
        getattr(b, "add_" + str(rng.choice(kinds)))(i)
    if profile == "spivar":                     # the shapes in turn, so that any run of seeds covers them all
        b.add_spi_variant(1, SPI_VARIANTS[seed % len(SPI_VARIANTS)])
    if profile == "protocols":                  # one bus, the protocols in turn (any run of seeds covers them all)
        getattr(b, "add_" + PROTOCOLS[seed % len(PROTOCOLS)])(0)
    for i in range(int(rng.integers(0, 4))):
        b.add_decoy(i)
    t_end = b.t_end + rng.uniform(0.2e-3, 2e-3)
    for f in b._deferred:
        f(t_end)
    # a decoy's question (a free-running clock, a PWM, noise: no bus) is answered by DECOY_MAX changes; a fast clock
    # over the whole capture (12 MHz for 150 ms: 3.6 M changes) is length by chance, all of the case's time spent on
    # it. Past that, the decoy runs in one stretch of DECOY_MAX changes and holds its level elsewhere (its own
    # generator, so that the rest of the case is the same)
    wr = np.random.default_rng(seed + 7_000_000)
    for info in b.decoys:
        w = b.waves[info["channel"]]
        if len(w.times) > DECOY_MAX:
            k = int(wr.integers(0, len(w.times) - DECOY_MAX + 1))
            w.initial = w.initial ^ (k & 1)
            w.times = w.times[k:k + DECOY_MAX]
            w.level = w.initial ^ (len(w.times) & 1)
            info["changes_kept"] = DECOY_MAX
    st = STRESS.get(stress or "", {})
    oversample = float(rng.choice(st.get("oversample", [4, 6, 8, 10, 16, 25, 50, 100])))
    rate = min(max(b.fastest * oversample, 1e6), 200e6)
    rate = float(min([1e6, 2e6, 4e6, 8e6, 10e6, 12e6, 16e6, 20e6, 24e6, 25e6, 50e6, 100e6, 200e6],
                     key=lambda x: (x < rate, abs(x - rate))))
    jitter = float(st.get("jitter", rng.choice([0, 0, 0.02, 0.1]))) / rate     # seconds: the same at any rate
    if rate_given:
        rate = float(rate_given)
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
