"""JTAG (wireskein._engine.jtag): TMS walking the TAP, the scans' TDI and TDO; TDO told from TDI by each IR scan's
first two bits out (1, 0). Waveforms made here (gen.jtag)."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen
from wireskein._engine.jtag import NEXT, RTI, TLR, walk


def test_five_ones_lead_any_state_to_reset():
    for s in range(16):
        for _ in range(5):
            s = int(NEXT[s, 1])
        assert s == TLR
    st = walk(np.array([1, 1, 1, 1, 1, 0, 0]))
    assert st[-1] == RTI and (st[:5] == -1).sum() >= 1


def test_scans_are_read():
    rng = np.random.default_rng(3)
    scans = []
    for _ in range(10):
        scans.append({"type": "IR", "bits": 9, "tdi": int(rng.integers(0, 512)), "tdo": (int(rng.integers(0, 128)) << 2) | 1})
        scans.append({"type": "DR", "bits": 36, "tdi": int(rng.integers(0, 2 ** 36)), "tdo": int(rng.integers(0, 2 ** 36))})
    w = {k: gen.Wave(0) for k in ("TCK", "TMS", "TDI", "TDO")}
    end = gen.jtag(w["TCK"], w["TMS"], w["TDI"], w["TDO"], 10e-6, scans, 1e6)
    cap = gen.quantize(w, 10e6, end + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "jtag" and c["verdict"] == "confirmed"
    assert c["roles"] == {"tck": "TCK", "tms": "TMS", "tdi": "TDI", "tdo": "TDO"} and c["params"]["ir_bits"] == 9
    assert [(s["type"], s["bits"], s["tdi"], s["tdo"]) for s in c["result"]["scans"]] == \
        [(s["type"], s["bits"], format(s["tdi"], "x"), format(s["tdo"], "x")) for s in scans]


def test_last_scan_kept_where_the_clock_stops_entering_update():
    """No clock after the last scan's Exit1 -> Update edge (idle_clocks 0, the capture ending there): the scan ends on
    the edge entering Update and is kept (2026-11-21, synth-protocols-30 lost it)."""
    rng = np.random.default_rng(30)
    scans = []
    for _ in range(8):
        scans.append({"type": "IR", "bits": 5, "tdi": int(rng.integers(0, 32)), "tdo": (int(rng.integers(0, 8)) << 2) | 1})
        scans.append({"type": "DR", "bits": 32, "tdi": int(rng.integers(0, 2 ** 32)), "tdo": int(rng.integers(0, 2 ** 32))})
    w = {k: gen.Wave(0) for k in ("TCK", "TMS", "TDI", "TDO")}
    end = gen.jtag(w["TCK"], w["TMS"], w["TDI"], w["TDO"], 10e-6, scans, 1e6, idle_clocks=0, gap=0.0)
    cap = gen.quantize(w, 10e6, end, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "jtag"]
    assert [(s["type"], s["tdi"]) for s in c["result"]["scans"]] == [(s["type"], format(s["tdi"], "x")) for s in scans]
