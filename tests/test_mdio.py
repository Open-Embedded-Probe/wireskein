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
