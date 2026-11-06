"""RVSWD's long form and the RISC-V Debug Module above it, on a WCH-LinkE and a CH32V103 (tests/data/rvswd_long_truth,
from wch-protocols experiments/e173). The truth is ch32rv's output on the LinkE (<name>.json): the chip id, the uid,
the option bytes and the registers it printed, read here from the wire as the debugger's memory and register reads.
The V103 has no Access Memory: the LinkE reads memory with programs in the program buffer, which the analysis runs.

The Debug Module's reading is also checked on DMI accesses written here (no capture): abstract commands, a program
buffer, autoexec."""

import json
from pathlib import Path

import pytest

from wireskein._engine.riscv_dm import accesses
from wireskein.decode import analyze_file

D = Path(__file__).parent / "data" / "rvswd_long_truth"
CASES = ["linke_v103_11_info_low", "linke_v103_13_info_high", "linke_v103_14_option_get", "linke_v103_19_dbg_regs"]


@pytest.fixture(scope="module")
def docs():
    return {}


def _claim(docs, name):
    if name not in docs:
        docs[name] = analyze_file(D / f"{name}.wireskein", mode="all")
    (c,) = [c for c in docs[name]["parts"][0]["claims"] if c["protocol"] == "rvswd"]
    return c


@pytest.mark.parametrize("name", CASES)
def test_the_long_form_is_read_on_its_pins(docs, name):
    truth = json.loads((D / f"{name}.truth.json").read_text())
    c = _claim(docs, name)
    assert c["roles"] == truth["roles"] and c["verdict"] == "confirmed"
    dmi = c["layers"]["dmi"]
    assert len(dmi) > 100 and all(x["form"] == "long" for x in dmi)
    assert dmi[0]["reg"] == "dmcontrol"
    # as ch32rv lets the hart go at the end, the V103 answers a frame or two a clock late (address 0x10 echoed as
    # 0x08, the data shifted) or as failed: those fail, and only those
    bad = [i for i, x in enumerate(dmi) if not x["ok"] or x["status"]]
    assert all(i >= len(dmi) - 4 for i in bad), bad


@pytest.mark.parametrize("name", CASES)
def test_what_ch32rv_printed_is_on_the_wire(docs, name):
    truth = json.loads((D / f"{name}.truth.json").read_text())
    dm = _claim(docs, name)["layers"]["dm"]
    mem = {}
    for e in dm:
        if e["op"] == "MEMR" and e["value"] is not None:
            mem.setdefault(e["addr"], e["value"])
    for a, v in truth.get("memory", {}).items():
        assert mem.get(int(a, 16)) == int(v, 16), (a, v)
    for a, v in truth.get("memory_low16", {}).items():
        assert mem.get(int(a, 16), 0) & 0xFFFF == int(v, 16), (a, v)
    regs = {e["addr"]: e["value"] for e in dm if e["op"] == "REGR"}
    for r, v in truth.get("registers", {}).items():
        assert regs.get(int(r, 16)) == int(v, 16), (r, v)


def test_the_stop_is_told_by_the_clock_held_high_not_a_fixed_time(docs):
    # at the LinkE's high speed a STOP's clock high is under 2 us at times: a fixed 2 us joins two frames
    dmi = _claim(docs, "linke_v103_13_info_high")["layers"]["dmi"]
    assert len(dmi) >= 146


# ---- the Debug Module on DMI accesses written here ----

def W(addr, data):
    return {"op": "W", "addr": addr, "data": data}


def R(addr, data):
    return {"op": "R", "addr": addr, "data": data}


def test_access_memory_with_postincrement():
    tx = [W(0x05, 0x20000000), W(0x17, (2 << 24) | (2 << 20) | (1 << 19)), R(0x04, 0x11223344),
          W(0x17, (2 << 24) | (2 << 20) | (1 << 19)), R(0x04, 0x55667788),
          W(0x05, 0x20000100), W(0x04, 0xCAFE), W(0x17, (2 << 24) | (1 << 20) | (1 << 16))]
    assert accesses(tx) == [["MEMR", 0x20000000, 0x11223344, 4], ["MEMR", 0x20000004, 0x55667788, 4],
                            ["MEMW", 0x20000100, 0xCAFE, 2]]


def test_a_program_buffer_load_is_read_when_its_data_register_is():
    # a3 = 0xe0000380 (data0's address, set up without postexec); then a1 = address, run: lw a0,0(a1); sw a0,4(a3)
    lw = (0 << 20) | (11 << 15) | (2 << 12) | (10 << 7) | 0x03
    sw = (0 << 25) | (10 << 20) | (13 << 15) | (2 << 12) | (4 << 7) | 0x23
    tx = [W(0x04, 0xE0000380), W(0x17, 0x0023100D), W(0x20, lw), W(0x21, sw), W(0x22, 0x00100073),
          W(0x04, 0x08000000), W(0x17, 0x0027100B), R(0x05, 0xDEADBEEF)]
    mem = [e for e in accesses(tx) if e[0] == "MEMR"]
    assert mem == [["MEMR", 0x08000000, 0xDEADBEEF, 4]]


def test_hartinfo_gives_the_data_registers_address_and_compressed_code_runs():
    # hartinfo: dataaddr 0x380 (sign-extended 12 bits: 0xfffff380 is not it; 0x380 is), dataaccess 1 -> not memory
    # mapped; then 0 -> memory: c.lw a0,0(a1); c.sw a0,4(a3); c.ebreak
    c_lw = 0x4188          # c.lw a0, 0(a1)
    c_sw = 0xC2C8          # c.sw a0, 4(a3)
    tx = [R(0x12, 0x00002380 & ~(1 << 16)), W(0x04, 0x380), W(0x17, 0x0023100D),
          W(0x20, c_lw | (c_sw << 16)), W(0x21, 0x9002),
          W(0x04, 0x1FFFF7E0), W(0x17, 0x0027100B), R(0x05, 0x00400040)]
    assert [e for e in accesses(tx) if e[0] == "MEMR"] == [["MEMR", 0x1FFFF7E0, 0x00400040, 4]]


def test_autoexec_reruns_the_command_and_a_dm_without_it_reads_back_zero():
    # program: lw t1,0(t0); addi t0,t0,4; ebreak. t0 = base; autoexec on data0: each read of data0 reads the next word
    lw = (5 << 15) | (2 << 12) | (6 << 7) | 0x03
    addi = (4 << 20) | (5 << 15) | (5 << 7) | 0x13
    run = W(0x17, 0x00261006)                  # read t1 into data0, then run the program
    base = [W(0x20, lw), W(0x21, addi), W(0x22, 0x00100073), W(0x04, 0x1FFFF800), W(0x17, 0x00271005)]
    with_auto = accesses(base + [run, W(0x18, 1), R(0x04, 0xA), R(0x04, 0xB)])
    assert [e[1:3] for e in with_auto if e[0] == "MEMR"][:2] == [[0x1FFFF800, 0xA], [0x1FFFF804, 0xB]]
    without = accesses(base + [run, W(0x18, 1), R(0x18, 0), W(0x18, 0), R(0x04, 0xA), R(0x04, 0xB)])
    assert [e[1:3] for e in without if e[0] == "MEMR"] == [[0x1FFFF800, 0xA], [0x1FFFF804, None]]
