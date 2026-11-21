"""cJTAG (IEEE 1149.7) OScan1 on TCKC and TMSC, read as `jtag` with `params.cjtag: "oscan1"`: escapes (TMSC
toggling while TCKC is high), the online activation after a selection, then JTAG cycles of three TCKC cycles (nTDI,
TMS, TDO). The scans are compared with the ones written. Waveforms made here, by an encoder apart from the reader."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

TCK = 1e-6          # a TCKC cycle


class Pair:
    def __init__(self):
        self.k, self.m, self.t = gen.Wave(1), gen.Wave(1), 20e-6

    def cycle(self, msc):
        """One TCKC cycle: TMSC set while TCKC is low, sampled at its rise."""
        self.k.set(self.t, 0)
        self.m.set(self.t + 0.1 * TCK, msc)
        self.k.set(self.t + 0.5 * TCK, 1)
        self.t += TCK

    def escape(self, toggles):
        """TCKC held high while TMSC toggles."""
        self.k.set(self.t, 0)
        self.k.set(self.t + 0.5 * TCK, 1)
        t = self.t + 0.6 * TCK
        lvl = self.m.level
        for _ in range(toggles):
            lvl ^= 1
            self.m.set(t, lvl)
            t += 0.2 * TCK
        self.t = t + 0.5 * TCK

    def oscan1(self, tms, tdi, tdo):
        self.cycle(1 - tdi)
        self.cycle(tms)
        self.cycle(tdo)


def jtag_cycles(scans):
    """(tms, tdi, tdo) per JTAG cycle from Run-Test/Idle: each scan ("IR" / "DR", tdi bits, tdo bits) through
    Capture, Shift (its bits, the last leaving with TMS 1), Exit1 and Update back to Run-Test/Idle."""
    out = []
    for kind, tdi, tdo in scans:
        out += [(1, 0, 0)] + ([(1, 0, 0)] if kind == "IR" else []) + [(0, 0, 0)] * 2
        for j, (a, b) in enumerate(zip(tdi, tdo)):
            out.append((int(j == len(tdi) - 1), a, b))
        out += [(1, 0, 0), (0, 0, 0)]
    return out


def bits(v, n):
    return [(v >> j) & 1 for j in range(n)]


def test_oscan1_scans():
    rng = np.random.default_rng(11)
    p = Pair()
    p.escape(8)                                         # reset: four-wire, TMSC as TMS
    for b in [1, 1, 1, 1, 1, 0]:                        # Test-Logic-Reset, then Run-Test/Idle
        p.cycle(b)
    p.escape(6)                                         # selection, then OAC (OScan1), EC, CP
    for b in [0, 0, 1, 1] + [0, 0, 0, 1] + [0, 1, 1, 0]:
        p.cycle(b)
    want, scans = [], []
    for _ in range(12):
        ir_in = int(rng.integers(0, 32))
        ir_out = 0b00001 | (int(rng.integers(0, 8)) << 2)   # the IR's capture: 1 then 0 first out
        dr_in, dr_out = int(rng.integers(0, 1 << 32)), int(rng.integers(0, 1 << 32))
        scans += [("IR", bits(ir_in, 5), bits(ir_out, 5)), ("DR", bits(dr_in, 32), bits(dr_out, 32))]
        want += [("IR", 5, format(ir_in, "x"), format(ir_out, "x")), ("DR", 32, format(dr_in, "x"), format(dr_out, "x"))]
    for tms, tdi, tdo in [(0, 0, 0)] * 3 + jtag_cycles(scans):
        p.oscan1(tms, tdi, tdo)
    cap = gen.quantize({"TCKC": p.k, "TMSC": p.m}, 20e6, p.t + 20e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "jtag" and c["verdict"] == "confirmed"
    assert c["roles"] == {"tckc": "TCKC", "tmsc": "TMSC"} and c["params"]["cjtag"] == "oscan1"
    assert [(s["type"], s["bits"], s["tdi"], s["tdo"]) for s in c["result"]["scans"]] == want
