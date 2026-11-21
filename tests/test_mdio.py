"""MDIO (an Ethernet PHY's management, IEEE 802.3 clauses 22 and 45), declared in decl/mdio.toml. A clause 22 frame
is 64 clocks, eight whole SPI words: the SPI reading of the same bits is no rival to it. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

RATE = 20e6


def _read(frames):
    mdc, md = gen.Wave(0), gen.Wave(1)
    end = gen.mdio(mdc, md, 10e-6, frames, 1e6)
    cap = gen.quantize({"MDC": mdc, "MDIO": md}, RATE, end + 10e-6, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    return [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]


def test_clause_22_reads_and_writes():
    rng = np.random.default_rng(1)
    frames = [{"op": int(rng.choice([1, 2])), "phy": 1, "reg": int(rng.integers(0, 32)), "data": int(rng.integers(0, 2 ** 16))}
              for _ in range(12)]
    (c,) = _read(frames)
    assert c["protocol"] == "mdio" and c["verdict"] == "confirmed"
    assert [(f["op"], f["phy"], f["reg"], f["data"]) for f in c["result"]["frames"]] == \
        [("W" if f["op"] == 1 else "R", f["phy"], f["reg"], f["data"]) for f in frames]


def test_clause_45():
    frames = []
    for k in range(6):
        frames += [{"clause": 45, "op": 0, "phy": 0, "dev": 1, "data": 0xA016 + k},
                   {"clause": 45, "op": 3, "phy": 0, "dev": 1, "data": 2 * k}]
    (c,) = _read(frames)
    assert c["protocol"] == "mdio"
    assert [(f["op"], f["dev"], f["data"]) for f in c["result"]["frames"]] == \
        [({0: "ADDR", 3: "R"}[f["op"]], f["dev"], f["data"]) for f in frames]


def test_clause_45_addresses_followed():
    """A read and its address: ADDR sets a device's register address, a read-and-increment steps it after reading
    (sigrok's mdio shows 8001, 8002, ... so)."""
    frames = [{"clause": 45, "op": 0, "phy": 0, "dev": 1, "data": 0x8000}]
    frames += [{"clause": 45, "op": 2, "phy": 0, "dev": 1, "data": 0x10 + k} for k in range(5)]
    frames += [{"clause": 45, "op": 0, "phy": 0, "dev": 3, "data": 0x0020},
               {"clause": 45, "op": 1, "phy": 0, "dev": 3, "data": 0x1234},
               {"clause": 45, "op": 3, "phy": 0, "dev": 1, "data": 0x55}]
    (c,) = _read(frames)
    got = [(f["op"], f["dev"], f.get("addr")) for f in c["result"]["frames"] if f["op"] != "ADDR"]
    assert got == [("RINC", 1, 0x8000 + k) for k in range(5)] + [("W", 3, 0x20), ("R", 1, 0x8005)]
