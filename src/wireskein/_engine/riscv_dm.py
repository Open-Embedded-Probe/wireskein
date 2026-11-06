"""The RISC-V Debug Module (RISC-V Debug Specification 0.13) on DMI accesses, whatever carries them.

A DMI access is {"op": "R"/"W", "addr", "data"}: WCH's RVSWD and SWIO carry them (rvswd.py), a JTAG DTM would
too. Above them this reads what the debugger did to the hart: registers and memory read and written, by abstract
commands (Access Register, Access Memory) and by programs it puts in the program buffer and runs (a debugger does
so when the Debug Module has no Access Memory, as on a CH32V103). The program buffer is run here on what is known:
registers the debugger wrote, data registers it wrote or read. A load from memory gives a value known only once the
debugger reads the data register it was stored to; that read is the memory read.

Each access read is (op, addr, value, size): op MEMR / MEMW / REGR / REGW, size in bytes (memory) or None.
"""

from __future__ import annotations

import numpy as np

from .stack import Node, Stream

DM_NAMES = {0x04: "data0", 0x05: "data1", 0x06: "data2", 0x07: "data3", 0x10: "dmcontrol", 0x11: "dmstatus",
            0x12: "hartinfo", 0x13: "haltsum1", 0x14: "hawindowsel", 0x15: "hawindow", 0x16: "abstractcs",
            0x17: "command", 0x18: "abstractauto", 0x19: "confstrptr0", 0x1D: "nextdm", 0x30: "authdata",
            0x38: "sbcs", 0x39: "sbaddress0", 0x3A: "sbaddress1", 0x3C: "sbdata0", 0x3D: "sbdata1", 0x40: "haltsum0",
            **{0x20 + i: f"progbuf{i}" for i in range(16)}}
# registers outside the specification a vendor puts in the DM's address space
VENDOR_NAMES = {0x7C: "wch_7c", 0x7D: "wch_7d", 0x7E: "wch_7e", 0x7F: "wch_7f"}
NAMES = {**DM_NAMES, **VENDOR_NAMES}
MASK = 0xFFFFFFFF


class Load:
    """A value not yet seen (loaded from memory, or a register read): known when the debugger reads the data
    register it went to. It fills the accesses it is the value of."""
    __slots__ = ("entries",)

    def __init__(self, *entries: list):
        self.entries = [e for e in entries if e[2] is None]

    def resolve(self, value: int) -> None:
        for e in self.entries:
            size = e[3] or 4
            e[2] = value if size >= 4 else value & ((1 << (8 * size)) - 1)
        self.entries = []


def _sx(v: int, bits: int) -> int:
    v &= (1 << bits) - 1
    return v - (1 << bits) if v >> (bits - 1) else v


def _decode(pb: dict[int, int], pc: int):
    """The instruction at byte offset pc of the program buffer: (length, name, fields), or None."""
    w = pb.get(pc // 4)
    if w is None:
        return None
    h = (w >> 16) & 0xFFFF if pc % 4 else w & 0xFFFF
    if h & 3 != 3:
        return 2, *_decode_c(h)
    if pc % 4:
        nxt = pb.get(pc // 4 + 1)
        if nxt is None:
            return None
        w = ((nxt & 0xFFFF) << 16) | h
    return 4, *_decode_32(w)


def _decode_32(w: int):
    op, rd, f3, rs1, rs2, f7 = w & 0x7F, (w >> 7) & 31, (w >> 12) & 7, (w >> 15) & 31, (w >> 20) & 31, w >> 25
    i_imm = _sx(w >> 20, 12)
    if w == 0x00100073:
        return "ebreak", {}
    if op == 0x03:
        return {0: "lb", 1: "lh", 2: "lw", 4: "lbu", 5: "lhu"}.get(f3, "?"), {"rd": rd, "rs1": rs1, "imm": i_imm}
    if op == 0x23:
        imm = _sx(((w >> 25) << 5) | ((w >> 7) & 31), 12)
        return {0: "sb", 1: "sh", 2: "sw"}.get(f3, "?"), {"rs1": rs1, "rs2": rs2, "imm": imm}
    if op == 0x13:
        name = {0: "addi", 2: "slti", 3: "sltiu", 4: "xori", 6: "ori", 7: "andi", 1: "slli",
                5: "srai" if f7 & 0x20 else "srli"}[f3]
        return name, {"rd": rd, "rs1": rs1, "imm": i_imm if f3 not in (1, 5) else rs2}
    if op == 0x33 and f7 in (0, 0x20):
        name = {(0, 0): "add", (0, 0x20): "sub", (4, 0): "xor", (6, 0): "or", (7, 0): "and", (1, 0): "sll",
                (5, 0): "srl"}.get((f3, f7), "?")
        return name, {"rd": rd, "rs1": rs1, "rs2": rs2}
    if op == 0x37:
        return "lui", {"rd": rd, "imm": w & 0xFFFFF000}
    if op == 0x63:
        imm = _sx(((w >> 31) << 12) | (((w >> 7) & 1) << 11) | (((w >> 25) & 0x3F) << 5) | (((w >> 8) & 15) << 1), 13)
        return {0: "beq", 1: "bne", 4: "blt", 5: "bge", 6: "bltu", 7: "bgeu"}.get(f3, "?"), \
            {"rs1": rs1, "rs2": rs2, "imm": imm}
    if op == 0x6F:
        imm = _sx(((w >> 31) << 20) | (((w >> 12) & 0xFF) << 12) | (((w >> 20) & 1) << 11)
                  | (((w >> 21) & 0x3FF) << 1), 21)
        return "jal", {"rd": rd, "imm": imm}
    return "?", {}


def _decode_c(h: int):
    q, f3 = h & 3, h >> 13
    r3a, r3b = 8 + ((h >> 7) & 7), 8 + ((h >> 2) & 7)
    rd, rs2 = (h >> 7) & 31, (h >> 2) & 31
    imm6 = _sx((((h >> 12) & 1) << 5) | ((h >> 2) & 31), 6)
    if h == 0x9002:
        return "ebreak", {}
    if q == 0:
        off = (((h >> 10) & 7) << 3) | (((h >> 6) & 1) << 2) | (((h >> 5) & 1) << 6)
        if f3 == 2:
            return "lw", {"rd": r3b, "rs1": r3a, "imm": off}
        if f3 == 6:
            return "sw", {"rs1": r3a, "rs2": r3b, "imm": off}
        return "?", {}
    if q == 1:
        if f3 == 0:
            return "addi", {"rd": rd, "rs1": rd, "imm": imm6}
        if f3 == 2:
            return "addi", {"rd": rd, "rs1": 0, "imm": imm6}                   # c.li
        if f3 == 3 and rd not in (0, 2):
            return "lui", {"rd": rd, "imm": (imm6 << 12) & MASK}
        if f3 == 4:
            sub = (h >> 10) & 3
            if sub == 0:
                return "srli", {"rd": r3a, "rs1": r3a, "imm": imm6 & 31}
            if sub == 1:
                return "srai", {"rd": r3a, "rs1": r3a, "imm": imm6 & 31}
            if sub == 2:
                return "andi", {"rd": r3a, "rs1": r3a, "imm": imm6}
            name = {0: "sub", 1: "xor", 2: "or", 3: "and"}[(h >> 5) & 3]
            return name, {"rd": r3a, "rs1": r3a, "rs2": r3b}
        if f3 == 5:
            b = h >> 2
            imm = _sx((((h >> 12) & 1) << 11) | (((h >> 8) & 1) << 10) | (((h >> 9) & 3) << 8) | (((h >> 6) & 1) << 7)
                      | (((h >> 7) & 1) << 6) | (((h >> 2) & 1) << 5) | (((h >> 11) & 1) << 4) | (((b >> 1) & 7) << 1), 12)
            return "jal", {"rd": 0, "imm": imm}                                   # c.j
        if f3 in (6, 7):
            imm = _sx((((h >> 12) & 1) << 8) | (((h >> 5) & 3) << 6) | (((h >> 2) & 1) << 5) | (((h >> 10) & 3) << 3)
                      | (((h >> 3) & 3) << 1), 9)
            return "beq" if f3 == 6 else "bne", {"rs1": r3a, "rs2": 0, "imm": imm}  # c.beqz / c.bnez
        return "?", {}
    if f3 == 0:
        return "slli", {"rd": rd, "rs1": rd, "imm": imm6 & 31}
    if f3 == 4:
        if not (h >> 12) & 1:
            return ("addi", {"rd": rd, "rs1": rs2, "imm": 0}) if rs2 else ("?", {})   # c.mv
        if rd and rs2:
            return "add", {"rd": rd, "rs1": rd, "rs2": rs2}
        return "?", {}
    return "?", {}


_ALU = {"addi": lambda a, b: a + b, "add": lambda a, b: a + b, "sub": lambda a, b: a - b,
        "xori": lambda a, b: a ^ b, "xor": lambda a, b: a ^ b, "ori": lambda a, b: a | b, "or": lambda a, b: a | b,
        "andi": lambda a, b: a & b, "and": lambda a, b: a & b, "slli": lambda a, b: a << (b & 31),
        "sll": lambda a, b: a << (b & 31), "srli": lambda a, b: a >> (b & 31), "srl": lambda a, b: a >> (b & 31),
        "srai": lambda a, b: _sx(a, 32) >> (b & 31), "slti": lambda a, b: int(_sx(a, 32) < b),
        "sltiu": lambda a, b: int(a < (b & MASK))}
_BR = {"beq": lambda a, b: a == b, "bne": lambda a, b: a != b, "blt": lambda a, b: _sx(a, 32) < _sx(b, 32),
       "bge": lambda a, b: _sx(a, 32) >= _sx(b, 32), "bltu": lambda a, b: a < b, "bgeu": lambda a, b: a >= b}
_SIZE = {"lb": 1, "lbu": 1, "lh": 2, "lhu": 2, "lw": 4, "sb": 1, "sh": 2, "sw": 4}


class DebugModule:
    """The Debug Module as the debugger drives it, read access by access (feed)."""

    def __init__(self):
        self.data: dict[int, object] = {}       # data registers 0..11: int, a Load, or None (unknown)
        self.progbuf: dict[int, int] = {}
        self.gpr: dict[int, object] = {0: 0}
        self.dataaddr: int | None = None        # where the data registers are in the hart's memory
        self.command: int | None = None
        self.autoexec = 0
        self.out: list[list] = []

    # ---- the hart, running the program buffer ----
    def _data_index(self, addr: int):
        if self.dataaddr is None:
            return None
        k = (addr - self.dataaddr) // 4
        return k if 0 <= addr - self.dataaddr < 48 else None

    def _run(self) -> None:
        pc, steps = 0, 0
        while steps < 64:
            steps += 1
            ins = _decode(self.progbuf, pc)
            if ins is None:
                return
            n, name, f = ins
            g = self.gpr
            if name == "ebreak":
                return
            if name in ("lb", "lbu", "lh", "lhu", "lw"):
                base = g.get(f["rs1"])
                if not isinstance(base, int):
                    return
                a = (base + f["imm"]) & MASK
                k = self._data_index(a)
                size = _SIZE[name]
                if k is not None:
                    v = self.data.get(k)
                    if isinstance(v, int) and size < 4:
                        v = (v >> (8 * (a % 4))) & ((1 << (8 * size)) - 1)
                        if name in ("lb", "lh"):
                            v = _sx(v, 8 * size) & MASK
                else:
                    entry = ["MEMR", a, None, size]
                    self.out.append(entry)
                    v = Load(entry)
                if f["rd"]:
                    g[f["rd"]] = v
            elif name in ("sb", "sh", "sw"):
                base, v = g.get(f["rs1"]), g.get(f["rs2"])
                if not isinstance(base, int):
                    return
                a = (base + f["imm"]) & MASK
                k = self._data_index(a)
                if k is not None:
                    self.data[k] = v
                else:
                    size = _SIZE[name]
                    val = v if isinstance(v, int) else None
                    if val is not None and size < 4:
                        val &= (1 << (8 * size)) - 1
                    self.out.append(["MEMW", a, val, size])
            elif name in _ALU:
                a = g.get(f["rs1"])
                b = g.get(f["rs2"]) if "rs2" in f else f["imm"]
                if f["rd"]:
                    g[f["rd"]] = (_ALU[name](a, b) & MASK) if isinstance(a, int) and isinstance(b, int) else None
            elif name == "lui":
                if f["rd"]:
                    g[f["rd"]] = f["imm"] & MASK
            elif name in _BR:
                a, b = g.get(f["rs1"]), g.get(f["rs2"])
                if not (isinstance(a, int) and isinstance(b, int)):
                    return                       # a branch on what is not known: stop here
                if _BR[name](a, b):
                    pc += f["imm"]
                    continue
            elif name == "jal":
                if f["rd"]:
                    g[f["rd"]] = None
                pc += f["imm"]
                continue
            else:
                return                           # an instruction not followed: stop
            pc += n

    # ---- abstract commands ----
    def _execute(self, cmd: int) -> None:
        kind = cmd >> 24
        write, transfer, postexec = (cmd >> 16) & 1, (cmd >> 17) & 1, (cmd >> 18) & 1
        if kind == 0:
            regno = cmd & 0xFFFF
            if transfer:
                if write:
                    v = self.data.get(0)
                    self.out.append(["REGW", regno, v if isinstance(v, int) else None, None])
                    if 0x1000 <= regno < 0x1020 and regno != 0x1000:
                        self.gpr[regno - 0x1000] = v
                else:                        # the value is the one the debugger then reads from data0
                    v = self.gpr.get(regno - 0x1000) if 0x1000 <= regno < 0x1020 else None
                    entry = ["REGR", regno, None, None]
                    self.out.append(entry)
                    self.data[0] = Load(entry, *(v.entries if isinstance(v, Load) else ()))
            if postexec:
                self._run()
        elif kind == 2:
            a = self.data.get(1)
            size = 1 << ((cmd >> 20) & 7)
            if write:
                v = self.data.get(0)
                self.out.append(["MEMW", a if isinstance(a, int) else None, v if isinstance(v, int) else None, size])
            else:
                entry = ["MEMR", a if isinstance(a, int) else None, None, size]
                self.out.append(entry)
                self.data[0] = Load(entry)
            if (cmd >> 19) & 1 and isinstance(a, int):        # aampostincrement
                self.data[1] = (a + size) & MASK

    # ---- DMI accesses ----
    def feed(self, op: str, addr: int, data) -> None:
        if op == "BURST":
            self.out.append(["BURST", self.data.get(1) if isinstance(self.data.get(1), int) else None, data, None])
            return
        if addr is None:
            return
        if 0x04 <= addr <= 0x0F:
            k = addr - 0x04
            if op == "W":
                self.data[k] = data
            else:
                v = self.data.get(k)
                if isinstance(v, Load):
                    v.resolve(data)
                self.data[k] = data
            if self.command is not None and (self.autoexec >> k) & 1:
                self._execute(self.command)
        elif 0x20 <= addr <= 0x2F:
            if op == "W":
                self.progbuf[addr - 0x20] = data
            if self.command is not None and (self.autoexec >> (16 + addr - 0x20)) & 1:
                self._execute(self.command)
        elif addr == 0x17 and op == "W":
            self.command = data
            self._execute(data)
        elif addr == 0x18:
            self.autoexec = data             # written, or read back (a DM without autoexec reads 0)
        elif addr == 0x12 and op == "R" and data is not None:
            if (data >> 16) & 1 == 0:                        # dataaccess 0: the data registers are in memory
                self.dataaddr = _sx(data & 0xFFF, 12) & MASK


def accesses(tx: list[dict], dataaddr: int | None = None) -> list[list]:
    """The register and memory accesses the DMI accesses `tx` carry ((op, addr, value, size), in order).
    dataaddr: where the data registers are in the hart's memory, when the capture never reads hartinfo; else it is
    learnt from the program buffer (an address a program stores to and the debugger then reads from data0..)."""
    dm = DebugModule()
    dm.dataaddr = dataaddr
    if dataaddr is None:
        dm.dataaddr = _learn_dataaddr(tx)
    for x in tx:
        if _skip(x):
            continue
        dm.feed(x["op"], x["addr"], x["data"])
    return dm.out


def _skip(x: dict) -> bool:
    """An access that did not happen: one not read whole, or one the DMI answered failed or busy."""
    if x["op"] == "BURST":
        return False
    return x.get("ok", True) is False or (x.get("form") == "long" and bool(x.get("status")))


def _learn_dataaddr(tx: list[dict]) -> int | None:
    """The data registers' address from the transfers: hartinfo when read; else the address a register is set to
    by a transfer and then used as a store's base by the program buffer, whose result the debugger reads (a debugger
    puts data0's address in a register). Tried on the values written to registers: the one the program's stores
    land in data0..data3 from."""
    for x in tx:
        if x["op"] == "R" and x["addr"] == 0x12 and x.get("ok", True) and not (x["data"] >> 16) & 1:
            return _sx(x["data"] & 0xFFF, 12) & MASK
    # candidates: values put in registers by Access Register writes (data0 then command with transfer+write)
    cands, data0 = [], None
    for x in tx:
        if x["op"] == "W" and x["addr"] == 0x04:
            data0 = x["data"]
        elif x["op"] == "W" and x["addr"] == 0x17 and data0 is not None:
            c = x["data"]
            # a pointer set up (transfer, write, no postexec), not an argument a program runs on
            if c >> 24 == 0 and (c >> 16) & 7 == 3 and data0 % 4 == 0 and data0 not in cands:
                cands.append(data0)
                if len(cands) >= 8:
                    break
    # and the addresses the programs touch (a program may build data0's address itself: lui / addi), each as data0..3
    dry = DebugModule()
    for x in tx:
        if not _skip(x):
            dry.feed(x["op"], x["addr"], x["data"])
    seen: dict[int, int] = {}
    for op, a, _, _ in dry.out:
        if op in ("MEMR", "MEMW") and isinstance(a, int):
            for k in range(4):
                c = (a - 4 * k) & MASK
                if c % 4 == 0:
                    seen[c] = seen.get(c, 0) + 1
    for c, _ in sorted(seen.items(), key=lambda kv: -kv[1])[:16]:
        if c not in cands:
            cands.append(c)
    # a candidate must read more of memory than none does (else any address the program touched would tie)
    best, best_n = None, sum(1 for e in dry.out if e[0] == "MEMR" and e[2] is not None)
    for a in cands:
        dm = DebugModule()
        dm.dataaddr = a
        for x in tx:
            if _skip(x):
                continue
            dm.feed(x["op"], x["addr"], x["data"])
        n = sum(1 for e in dm.out if e[0] == "MEMR" and e[2] is not None)
        if n > best_n:
            best, best_n = a, n
    return best


def dm_node(parent: Node) -> Node | None:
    tx = parent.output.items
    if not tx:
        return None
    known = float(np.mean([x["addr"] in NAMES for x in tx if x["op"] != "BURST"] or [0]))
    log = accesses(tx)
    has_ctrl = any(x["addr"] == 0x10 for x in tx) and any(x["addr"] == 0x11 for x in tx)
    score = known * (0.5 + 0.5 * has_ctrl)
    return Node("riscv_dm", {}, parent.roles, Stream("dm.accesses", log),
                {"dm_addr": known, "accesses": len(log), "dmcontrol_dmstatus": has_ctrl}, parent,
                layer_score=score, total=score)
