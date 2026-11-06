"""The qspi check: SPI on several data lines, per window of CS active and phase by phase. The lines are made by
an encoder written here, apart from the reader: a flash's Fast Read Quad I/O (0xEB, 1-4-4) and Quad Output read
(0x6B, 1-1-4)."""

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import qspi
from wireskein.verify import check_qspi

RATE, F = 100e6, 10e6
HALF = 1 / F / 2


def clocks_of(phases):
    """Each clock's value on IO0..IO3 (None: not driven, held), from (width, bits as a string) or ("dummy", n)."""
    out = []
    for p in phases:
        if p[0] == "dummy":
            out += [None] * p[1]
            continue
        w, bits, *line = p
        for k in range(0, len(bits), w):
            v = [None] * 4
            if w == 1:
                v[line[0] if line else 0] = int(bits[k])
            else:
                for j in range(w):
                    v[w - 1 - j] = int(bits[k + j])
            out.append(v)
    return out


def hexbits(h):
    return "".join(f"{int(c, 16):04b}" for c in h)


def bus(windows, mode=0, skew=0.0):
    """Mode 0: data set up half a period before each rising edge."""
    cpol = mode >> 1
    t, sck, cs, io = 1e-6, [], [], [[] for _ in range(4)]
    lv = [0, 0, 0, 0]
    for clocks in windows:
        cs.append(t)
        t += HALF
        for c in clocks:
            for j in range(4):
                if c is not None and c[j] is not None and c[j] != lv[j]:
                    io[j].append(t + skew * j)
                    lv[j] = c[j]
            t += HALF
            sck.append(t)                                 # the leading edge: sampling in modes 0 and 3
            t += HALF
            sck.append(t)
        t += HALF
        cs.append(t)
        t += 2e-6
    n = int(round((t + 1e-6) * RATE))
    e = lambda xs: np.array(sorted(int(round(x * RATE)) for x in xs), np.int64)
    sck_ch = Channel("SCK", cpol, e(sck))
    return Capture(RATE, n, [sck_ch, Channel("CS", 1, e(cs))] + [Channel(f"IO{j}", 0, e(io[j])) for j in range(4)])


IO = ["IO0", "IO1", "IO2", "IO3"]
DATA = "0123456789abcdeffedcba9876543210"


def eb(addr, data):
    return clocks_of([(1, hexbits("eb")), (4, hexbits(addr)), (4, hexbits("ff")), ("dummy", 4), (4, hexbits(data))])


def test_fast_read_quad_io():
    cap = bus([eb("001000", DATA), eb("002000", "a5")])
    want = [[{"hex": "eb"}, {"width": 4, "hex": "001000"}, {"width": 4, "bits": 8}, {"dummy": 4},
             {"width": 4, "hex": DATA}],
            [{"hex": "eb"}, {"width": 4, "hex": "002000"}, {"width": 4, "hex": "ff"}, {"dummy": 4}, {"width": 4}]]
    ok, got, why = check_qspi(cap, qspi("SCK", "CS", IO, want, freq_hz=F))
    assert ok is True, (why, got)
    assert got["phases"][1][-1] == "a5" and got["clocks"] == [8 + 6 + 2 + 4 + 32, 8 + 6 + 2 + 4 + 2]
    assert abs(got["freq_hz"] / F - 1) < 0.01


def test_quad_output_read_1_1_4():
    w = clocks_of([(1, hexbits("6b")), (1, hexbits("000100")), ("dummy", 8), (4, hexbits(DATA))])
    cap = bus([w])
    ok, got, why = check_qspi(cap, qspi("SCK", "CS", IO, [[{"hex": "6b"}, {"hex": "000100"}, {"dummy": 8},
                                                             {"width": 4, "hex": DATA}]]))
    assert ok is True, (why, got)


def test_wrong_data_short_window_and_leftover_clocks():
    cap = bus([eb("001000", DATA)])
    base = [{"hex": "eb"}, {"width": 4, "hex": "001000"}, {"width": 4, "bits": 8}, {"dummy": 4}]
    ok, _, why = check_qspi(cap, qspi("SCK", "CS", IO, [base + [{"width": 4, "hex": DATA[:-1] + "1"}]]))
    assert ok is False and "phase #4" in why
    ok, _, why = check_qspi(cap, qspi("SCK", "CS", IO, [base + [{"width": 4, "hex": DATA[:16]}]]))
    assert ok is False and "16 clocks after its phases" in why
    ok, _, why = check_qspi(cap, qspi("SCK", "CS", IO, [base + [{"width": 4, "hex": DATA + "00"}]]))
    assert ok is False and "2 clocks short" in why


def test_a_single_line_read_on_io1():
    w = clocks_of([(1, hexbits("03")), (1, hexbits("000000")), (1, hexbits("c3"), 1)])
    cap = bus([w])
    ok, got, why = check_qspi(cap, qspi("SCK", "CS", IO, [[{"hex": "03"}, {"hex": "000000"}, {"lines": [1], "hex": "c3"}]]))
    assert ok is True, (why, got)


def test_without_transfers_each_window_on_all_lines():
    w = clocks_of([(4, hexbits("deadbeef"))])
    ok, got, why = check_qspi(bus([w]), qspi("SCK", "CS", IO))
    assert ok is True and got["data"] == ["deadbeef"], (why, got)


def test_mode_mismatch():
    cap = bus([eb("001000", DATA)])
    ok, _, why = check_qspi(cap, qspi("SCK", "CS", IO, mode=3))
    assert ok is False and "CPOL 1" in why


# ---- the truth from hardware: an OEP probe's ESP-IDF spi_master (SPI3_HOST, half duplex, no DMA) at 1 MHz,
# mode 0, its SCK, CS and IO0-3 captured at 20 MS/s (tests/data/qspi_truth; the phases in each .truth.json) ----

@pytest.mark.parametrize("name", ["qspi_A_EB_1-4-4", "qspi_B_6B_1-1-4", "qspi_C_3B_1-1-2"])
def test_hardware_esp_idf_spi_master(name):
    import json
    from pathlib import Path
    from wireskein._engine import fileio
    d = Path(__file__).parent / "data" / "qspi_truth"
    (cap,) = fileio.load_runs(d / f"{name}.wireskein")
    t = json.loads((d / f"{name}.truth.json").read_text())
    assert t["interface"].startswith("io.github.open-embedded-probe.bench.qspi")
    phases = []
    for p in t["transaction"]["phases"]:
        if p["phase"] == "dummy":
            phases.append({"dummy": p["clocks"]})
        else:
            h = p.get("bytes") or p["value"][2:]
            phases.append({"width": p["lines"], "hex": h})
    ok, got, why = check_qspi(cap, qspi("sck", "cs", ["io0", "io1", "io2", "io3"], [phases], freq_hz=1e6, freq_rel=0.01))
    assert ok is True, (why, got)
    assert got["clocks"] == [t["transaction"]["sck_clocks_seen"]]
