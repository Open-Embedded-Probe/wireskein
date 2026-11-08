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
    assert [e[:4] for e in accesses(tx)] == [["MEMR", 0x20000000, 0x11223344, 4], ["MEMR", 0x20000004, 0x55667788, 4],
                                             ["MEMW", 0x20000100, 0xCAFE, 2]]


def test_a_program_buffer_load_is_read_when_its_data_register_is():
    # a3 = 0xe0000380 (data0's address, set up without postexec); then a1 = address, run: lw a0,0(a1); sw a0,4(a3)
    lw = (0 << 20) | (11 << 15) | (2 << 12) | (10 << 7) | 0x03
    sw = (0 << 25) | (10 << 20) | (13 << 15) | (2 << 12) | (4 << 7) | 0x23
    tx = [W(0x04, 0xE0000380), W(0x17, 0x0023100D), W(0x20, lw), W(0x21, sw), W(0x22, 0x00100073),
          W(0x04, 0x08000000), W(0x17, 0x0027100B), R(0x05, 0xDEADBEEF)]
    mem = [e for e in accesses(tx) if e[0] == "MEMR"]
    assert [e[:4] for e in mem] == [["MEMR", 0x08000000, 0xDEADBEEF, 4]]


def test_hartinfo_gives_the_data_registers_address_and_compressed_code_runs():
    # hartinfo: dataaddr 0x380 (sign-extended 12 bits: 0xfffff380 is not it; 0x380 is), dataaccess 1 -> not memory
    # mapped; then 0 -> memory: c.lw a0,0(a1); c.sw a0,4(a3); c.ebreak
    c_lw = 0x4188          # c.lw a0, 0(a1)
    c_sw = 0xC2C8          # c.sw a0, 4(a3)
    tx = [R(0x12, 0x00002380 & ~(1 << 16)), W(0x04, 0x380), W(0x17, 0x0023100D),
          W(0x20, c_lw | (c_sw << 16)), W(0x21, 0x9002),
          W(0x04, 0x1FFFF7E0), W(0x17, 0x0027100B), R(0x05, 0x00400040)]
    assert [e[:4] for e in accesses(tx) if e[0] == "MEMR"] == [["MEMR", 0x1FFFF7E0, 0x00400040, 4]]


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


# ---- a short-form session on a CH32L103 (from wch-protocols), read by decode from its .sr ----

@pytest.mark.parametrize("name", ["linke_l103_target_info", "linke_v203_100mhz_target_info"])
def test_short_frames_read_by_decode_from_a_sr(name):
    # linke_l103: a fast LinkE holds SWCLK high 3 to 4 times its usual high at a short frame's turnaround (0.5 us at
    # 3 MHz): taken for a STOP, the frame splits and is lost (the L103's DetachChip and its ESIG reads were).
    # linke_v203_100mhz: SWCLK's high is 2-3 samples: the views with short pulses filtered out lose bits, and must not
    # win over the lines as they are for the clocks outside START and STOP (an idle clock's glitches)
    from wireskein.decode import decode_file
    truth = json.loads((D / f"{name}.truth.json").read_text())
    doc = decode_file(D / f"{name}.sr", {"buses": [{"protocol": "rvswd", "pins": truth["roles"]}]})
    (bus,) = doc["parts"][0]["buses"]
    dmi = bus["result"]["dmi"]
    forms = {f: sum(1 for x in dmi if x["form"] == f) for f in ("short", "long")}
    assert forms == truth["frames"]
    mem = {e["addr"]: e["value"] for e in bus["result"]["dm"] if e["op"] == "MEMR"}
    for a, v in truth["memory"].items():
        assert mem.get(int(a, 16)) == int(v, 16)
    for a, v in truth.get("memory_low16", {}).items():
        assert mem.get(int(a, 16), 0) & 0xFFFF == int(v, 16)
    assert all("e" in x and x["e"] > x["s"] for x in dmi)


def test_a_frame_no_layout_reads_is_uninterpreted():
    # frames left out of the framing (as a framing that missed them would) are said as unread, not left out
    from wireskein._engine import rvswd
    from wireskein.decode import decode_file
    truth = json.loads((D / "linke_l103_target_info.truth.json").read_text())
    orig = rvswd.reframe_startstop
    rate = 50e6

    def missing(fr, cap, **kw):
        sb = fr.source
        return [f for f in orig(fr, cap, **kw) if not 126.1e-3 <= sb.t[f[0]] / rate <= 128.6e-3]
    try:
        rvswd.reframe_startstop = missing
        doc = decode_file(D / "linke_l103_target_info.sr", {"buses": [{"protocol": "rvswd", "pins": truth["roles"]}]},
                          split="off")
    finally:
        rvswd.reframe_startstop = orig
    (bus,) = doc["parts"][0]["buses"]
    lost = [u for u in bus["uninterpreted"] if 126.1e-3 <= u["s"] / rate <= 128.6e-3 and u["edges"] > 50]
    assert lost, bus["uninterpreted"]

def test_an_answer_a_clock_late_is_marked(docs):
    # as ch32rv lets the V103 go, its answers come 1.05 to 1.1 clocks after the bit's rising edge (0x10 echoed as
    # 0x08); in time they come 0.5 to 0.8 after it
    dmi = _claim(docs, "linke_v103_14_option_get")["layers"]["dmi"]
    late = [i for i, x in enumerate(dmi) if x.get("late")]
    assert late == [len(dmi) - 3, len(dmi) - 2]
    for i, x in enumerate(dmi):
        d = x["answer_delay"]["median"]
        assert (1.0 < d < 1.3) if i in late else (0.4 < d < 0.9), (i, x)


def test_a_line_with_no_start_has_no_frames():
    import numpy as np
    from wireskein._engine import typed
    from wireskein._engine.model import Capture, Channel
    from wireskein._engine.rvswd import reframe_startstop
    clk = Channel("C", 0, np.arange(10, 2000, 10, dtype=np.int64))
    dio = Channel("D", 0, np.array([15, 1015], np.int64))      # rises and falls only while the clock is low
    cap = Capture(1e6, 3000, [clk, dio])
    sb = typed.sync_bits(cap, None, "C", ("D",), None, "rise")
    assert reframe_startstop(typed.frames_gap(sb), cap) == []


# ---- SWIO: a CH32V003's ram read by a LinkE (wch-protocols' fixture), the bytes ch32rv wrote as the truth ----

def test_swio_bulk_read_with_continuations_is_the_bytes_ch32rv_read():
    # the LinkE reads 16 words a time: one read of data0, 14 continuations (33 pulses) with autoexec on, the last
    # after turning autoexec off; the
    # program in the program buffer builds data0's address (0xe00000f4) itself, which the Debug Module learns
    import struct
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_v003_read_ram_256.sr", {"buses": [{"protocol": "swio", "pins": {"dio": "SWIO"}}]})
    (bus,) = doc["parts"][0]["buses"]
    r = bus["result"]
    assert sum(1 for x in r["dmi"] if x.get("fast")) == 4 * 14
    words = struct.unpack("<64I", (D / "linke_v003_read_ram_256.bin").read_bytes())
    mem = {e["addr"]: e["value"] for e in r["dm"] if e["op"] == "MEMR"}
    assert [mem.get(0x20000000 + 4 * i) for i in range(64)] == list(words)
    # the frame's pulses against the split: every short low under it, every long one over it
    split = bus["params"]["split_ns"]
    assert all(x["short_max_ns"] < split < x["long_min_ns"] for x in r["dmi"] if x["short_max_ns"] and x["long_min_ns"])


def test_swio_activity_that_is_no_frame_is_uninterpreted():
    # before it connects, a LinkE sends 32 pulses (and one more: 33, the shape of a continuation, but after no read
    # of data0) and the two-wire long form's queries on the one wire: neither is SWIO, both are said as unread
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_v003_read_ram_256.sr", {"buses": [{"protocol": "swio", "pins": {"dio": "SWIO"}}]})
    (bus,) = doc["parts"][0]["buses"]
    first = bus["result"]["dmi"][0]
    assert not first.get("fast")
    un = bus["uninterpreted"]
    assert un and un[0]["edges"] == 66 and un[0]["s"] < first["s"]
    assert any(u["edges"] > 1000 for u in un)          # the long form's queries, one after another


def test_rvswd_frames_carry_their_bits_and_clock_period():
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_l103_target_info.sr",
                      {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]})
    dmi = doc["parts"][0]["buses"][0]["result"]["dmi"]
    assert all(len(x["bits"]) == (53 if x["form"] == "short" else 85) for x in dmi)
    assert all(x["period_ns"] > 0 for x in dmi)


def test_burst_reads_are_the_flash_ch32rv_read():
    # a CH32V203's 4 KiB read by a LinkE: 64 bursts of 15 reads of data0 (autoexec, Access Memory with
    # postincrement) after one Access Memory each; a burst's high now and then held twice its usual (1.1 us) is no
    # STOP. The truth: the bytes ch32rv wrote (read --range 0x08000000+4096)
    import struct
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_v203_read_flash_4k.sr",
                      {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]})
    r = doc["parts"][0]["buses"][0]["result"]
    assert sum(1 for x in r["dmi"] if x["op"] == "BURST") == 64
    words = struct.unpack("<1024I", (D / "linke_v203_read_flash_4k.bin").read_bytes())
    mem = {e["addr"]: e["value"] for e in r["dm"] if e["op"] == "MEMR"}
    assert [mem.get(0x08000000 + 4 * i) for i in range(1024)] == list(words)


def test_data_ringing_at_the_clock_edge_is_read_as_it_settles():
    # a tap's reflection rings SWDIO for a few samples right at SWCLK's rising edge (a change launched by that edge):
    # read at the edge itself, the first query's host data changed run to run (0x11, 0x10, 0x01, 0x18); read as it
    # settles it is 0x19 in every run, as wch-protocols' rvswd.py (with its hold filter) reads it
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_l103_repeat1_target_info.sr",
                      {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]})
    dmi = doc["parts"][0]["buses"][0]["result"]["dmi"]
    first = next(x for x in dmi if x["form"] == "long")["bits"]
    assert (int(first[0:7], 2), int(first[7:39], 2), first[39:41]) == (0x11, 0x19, "00")


def test_ringing_data_does_not_let_a_view_without_a_short_clock_win():
    # wch-protocols e163 preff (LinkE speed setting 0xff): SWDIO rings for 1-3 samples while SWCLK is high and one
    # clock's low is only 6 samples; read raw the bits rang, and a view with short pulses filtered out (that clock
    # gone) won with 52-bit frames that passed by chance (dmstatus 0x704). Read as the data settles: 53 bits, 0x382
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_l103_e163_preff.sr",
                      {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]})
    dmi = doc["parts"][0]["buses"][0]["result"]["dmi"]
    short = [x for x in dmi if x["form"] == "short"]
    assert len(short) == 52 and all(len(x["bits"]) == 53 and x["ok"] for x in short)
    assert {x["data"] for x in short if x["reg"] == "dmstatus"} <= {0x382, 0xc82, 0x30c82}


def test_a_glitch_clock_is_dropped_when_that_gives_a_known_length():
    import numpy as np
    from wireskein._engine.rvswd import _repaired, known_length
    highs = np.full(200, 25.0)
    highs[30] = 1                                     # a reflection's glitch read as a clock, inside a 54-clock frame
    assert _repaired(10, 64, highs) == (10, 64, (30,))
    assert _repaired(10, 63, highs) == (10, 63)       # 53: a known length, nothing dropped
    highs[30] = 25
    assert _repaired(10, 64, highs) == (10, 64)       # no glitch clock to drop: left as it is (not read)
    assert known_length(585) and not known_length(586)


def test_named_fields_layers_and_times():
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_v203_read_flash_4k.sr",
                      {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}, "layers": ["bits"]}]})
    r = doc["parts"][0]["buses"][0]["result"]
    # every sampling edge, to time the clock inside a frame
    t = r["bits"]["t"]
    assert len(t) >= sum(len(x["bits"]) for x in r["dmi"])
    short = [x for x in r["dmi"] if x["form"] == "short" and x["op"] != "BURST"]
    f = short[0]["fields"]
    assert set(f) >= {"addr", "rw", "header_parity", "park", "ctl", "data", "data_parity", "status", "pad", "stop"}
    assert all(x["fields"]["header_parity_ok"] and x["fields"]["data_parity_ok"] for x in short if x["ok"])
    burst = [x for x in r["dmi"] if x["op"] == "BURST"][0]["fields"]
    assert len(burst["header"]) == 14 and len(burst["aux"]) == 14 and len(burst["trailer"]) == 7
    # the Debug Module's accesses with the ticks of the command (s) and of the read that showed the value (e)
    mem = [e for e in r["dm"] if e["op"] == "MEMR"]
    assert all(e["s"] <= e["e"] for e in mem)


def test_swio_pulses_and_turnaround():
    import collections
    from wireskein.decode import decode_file
    doc = decode_file(D / "linke_v003_read_ram_256.sr",
                      {"buses": [{"protocol": "swio", "pins": {"dio": "SWIO"}, "layers": ["pulses"]}]})
    r = doc["parts"][0]["buses"][0]["result"]
    assert len(r["pulses"]["s"]) == len(r["pulses"]["width"]) > 41 * 60
    gaps = collections.defaultdict(list)
    for x in r["dmi"]:
        if "rw_gap_ns" in x:
            gaps[x["op"]].append(x["rw_gap_ns"])
    # from the R/W bit's end to the first data pulse: a read waits for the target (740-780 ns), a write does not
    assert 700 < min(gaps["R"]) and max(gaps["R"]) < 820 and 340 < min(gaps["W"]) and max(gaps["W"]) < 400


def test_a_stop_far_over_the_usual_high_though_short():
    # at its fastest a LinkE holds SWCLK high only 30 ns, and a STOP may hold it only 1.4 us: under the 1.5 us that
    # tells a STOP from a burst's twice-usual high (1.1 us), but over 4 times the usual: still a STOP
    import numpy as np
    from wireskein._engine import typed
    from wireskein._engine.model import Capture, Channel
    from wireskein._engine.rvswd import reframe_startstop
    rate, per, hi = 100e6, 10, 3                      # 10 MHz clock, highs of 3 samples
    clk, dio, t = [], [], 1000

    def frame(t):
        dio.append(t - 2)                             # START: DIO falls while CLK is high (an idle high before)
        clk.append(t)                                 # CLK falls: the frame's clocks
        for _ in range(53):
            clk.extend([t + per - hi, t + per])
            t += per
        clk.append(t + per - hi)                      # CLK rises and stays high
        dio.append(t + per - hi + 2)                  # STOP: DIO rises while CLK is high
        return t + per - hi + 140                     # held 1.4 us, then the next frame
    t = frame(t)
    t = frame(t)
    cap = Capture(rate, t + 1000, [Channel("C", 1, np.asarray(clk, np.int64)), Channel("D", 1, np.asarray(sorted(dio), np.int64))])
    sb = typed.sync_bits(cap, None, "C", ("D",), None, "rise")
    got = reframe_startstop(typed.frames_gap(sb), cap)
    assert [f[1] - f[0] for f in got] == [54, 54]


SPEC = {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]}


@pytest.mark.parametrize("name,base,size,long_", [("linke_v203_speed_high_read_ram_256", 0x20000000, 256, 202),
                                                  ("linke_v203_speed_high_read_flash_4k", 0x08000000, 4096, 202)])
def test_a_clock_slow_then_fast_keeps_the_fast_bits(name, base, size, long_):
    # a LinkE at its high speed on a CH32V203 at 50 MS/s: it connects slowly (a period of about 106 samples), then
    # reads fast (about 5): DIO's pulses of a sample or two are ringing only where the clock is slow; the fast clock's
    # highs of a sample or two are clocks. The truth: the bytes ch32rv wrote (read --range)
    import struct
    from wireskein.decode import decode_file
    r = decode_file(D / f"{name}.sr", SPEC)["parts"][0]["buses"][0]["result"]
    assert sum(1 for x in r["dmi"] if x.get("form") == "long") == long_
    words = struct.unpack(f"<{size // 4}I", (D / f"{name}.bin").read_bytes())
    mem = {e["addr"]: e["value"] for e in r["dm"] if e["op"] == "MEMR"}
    assert [mem.get(base + 4 * i) for i in range(size // 4)] == list(words)


def test_the_first_frames_after_a_slow_connect_are_read():
    # where wireskein 0.0.26 lost them: the first long query after the connect, and a short read of dmstatus
    from wireskein.decode import decode_file
    for name, tick, op, bits in (("linke_v203_speed_high_read_ram_256", 3150780, "NOP", 85),
                                 ("linke_v203_speed_high_read_flash_4k", 6183619, "R", 53)):
        r = decode_file(D / f"{name}.sr", SPEC)["parts"][0]["buses"][0]["result"]
        (x,) = [x for x in r["dmi"] if x["s"] == tick]
        assert (x["op"], len(x["bits"]), x["addr"]) == (op, bits, 0x11)
    r = decode_file(D / "linke_v203_speed_high_read_ram_256.sr", SPEC)["parts"][0]["buses"][0]["result"]
    assert sum(1 for x in r["dmi"] if x["op"] == "BURST") == 4        # no burst read out of the connect's frames


def test_pieces_read_on_the_edge_the_first_piece_reads():
    # a LinkE at its fastest at 160 MS/s: a piece on its own may read on the falling edge, every frame a clock early
    # (an extra 0 first, the last bit lost); every piece reads on the edge the first one decoded on, as read whole
    from wireskein.decode import decode_file
    name = D / "linke_v203_160mhz_flash_pattern4k.sr"
    whole = decode_file(name, SPEC, split="off")["parts"][0]["buses"][0]
    doc = decode_file(name, SPEC)
    part = doc["parts"][0]
    assert len(part["split"]["pieces"]) > 2
    b = part["buses"][0]
    assert b["params"]["sample_edge"] == whole["params"]["sample_edge"] == "rise"
    assert b["result"]["dmi"] == whole["result"]["dmi"]
    assert sum(1 for x in b["result"]["dmi"] if x["op"] == "W" and x["addr"] == 0x17) > 1000


def test_a_clock_too_fast_for_the_rate_is_told_with_its_speeds():
    # a LinkE connects at 0.33 MHz, then reads at 10 MHz with highs of 20 ns: at 50 MS/s a high may fall between two
    # samples. The speeds come from the frames read, each frame carrying its own clock
    from wireskein.decode import decode_file
    b = decode_file(D / "linke_v203_speed_high_read_ram_256.sr", SPEC)["parts"][0]["buses"][0]
    sp = {round(g["freq_hz"] / 1e4) / 100: g for g in b["clock_speeds"]}
    assert sp[0.33]["frames"] == 202 and sp[0.33]["rate_ok"] and not sp[10.0]["rate_ok"]
    us = b["undersampled"]
    assert us["clock"] == "SWCLK" and us["high_ns"] == 20.0 and (us["least_rate_hz"], us["best_rate_hz"]) == (150e6, 250e6)
    x = next(x for x in b["result"]["dmi"] if x.get("form") == "long")
    assert x["period_ns"] > 2000 and 900 < x["high_ns"] < 1100
    # at 160 MS/s the same clock's highs last 4 samples: not told
    b = decode_file(D / "linke_v203_160mhz_flash_pattern4k.sr", SPEC)["parts"][0]["buses"][0]
    assert "undersampled" not in b and all(g["rate_ok"] for g in b["clock_speeds"])


def test_a_frame_a_clock_short_is_read_only_where_one_reading_is_left():
    # the same 4 KiB flashed, taken at 50 MS/s (a 10 MHz clock's highs of 20 ns: clocks lost between samples) and at
    # 160 MS/s (none lost). A frame a clock short is read with the lost clock restored only where every place it may
    # have been lost gives the same bits; each such frame is one the 160 MS/s capture reads too
    from wireskein.decode import decode_file
    slow = decode_file(D / "linke_v203_flash_pattern4k.sr", SPEC)["parts"][0]["buses"][0]
    fast = decode_file(D / "linke_v203_160mhz_flash_pattern4k.sr", SPEC)["parts"][0]["buses"][0]
    seen = {(x["op"], x["addr"], str(x["data"])) for x in fast["result"]["dmi"] if x["ok"] and x.get("form") == "short"}
    restored = [x for x in slow["result"]["dmi"] if "restored" in x]
    assert len(restored) > 100 and all(x["ok"] and len(x["bits"]) == 53 for x in restored)
    assert all((x["op"], x["addr"], str(x["data"])) in seen for x in restored)
    assert all(x["s"] < x["restored"][0] < x["e"] for x in restored)
    assert slow["undersampled"]["restored"] == len(restored)


def test_how_a_bus_read_is_told_in_counts():
    # a LinkE at its fastest at 50 MS/s: the frames read and how, the clock too fast, and the noise seen
    from wireskein.decode import decode_file
    b = decode_file(D / "linke_v203_flash_pattern4k.sr", SPEC)["parts"][0]["buses"][0]
    r = b["reading"]
    dmi = b["result"]["dmi"]
    assert r["frames"] == len(dmi) and r["ok"] == sum(1 for x in dmi if x["ok"])
    assert r["restored"] == sum(1 for x in dmi if "restored" in x) > 100 and r["rate_ok"] is False
    assert r["unread"] == len(b["uninterpreted"]) and r["glitches"] > 0 and r["ringing"]["SWDIO"] > 0
    assert r["glitch_share"] == round(2 * r["glitches"] / r["clock_edges"], 4) and r["noisy"]
    # at 160 MS/s: every speed sampled well, nothing restored
    r = decode_file(D / "linke_v203_160mhz_flash_pattern4k.sr", SPEC)["parts"][0]["buses"][0]["reading"]
    assert r["rate_ok"] and r["restored"] == 0 and r["frames"] == 9414
