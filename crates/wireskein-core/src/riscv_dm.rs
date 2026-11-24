//! The RISC-V Debug Module (Debug Specification 0.13) on DMI accesses, whatever carries them (Python's
//! `_engine/riscv_dm.py`): what the debugger did to the hart, registers and memory read and written by abstract
//! commands (Access Register, Access Memory) and by programs it puts in the program buffer and runs. The program
//! buffer is run on what is known (registers the debugger wrote, data registers written or read); a load from memory
//! gives a value known only when the debugger reads the data register it went to.
//!
//! Each access read: (op, addr, value, size, s, e): op MEMR / MEMW / REGR / REGW (or BURST), size in bytes (memory),
//! s the tick of the DMI access that made it, e that of the one that showed its value.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const MASK: i64 = 0xFFFF_FFFF;

/// A DMI access as the dm reads it.
#[derive(Debug, Clone)]
pub struct Dmi {
    pub op: String,
    pub addr: Option<i64>,
    pub data: Data,
    pub ok: bool,
    pub long_failed: bool, // a long frame whose DMI status said failed or busy
    pub s: Option<i64>,
    pub e: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Num(i64),
    List(Vec<i64>),
    Null,
}

/// An access read.
#[derive(Debug, Clone)]
pub struct Entry {
    pub op: &'static str,
    pub addr: Option<i64>,
    pub value: Data,
    pub size: Option<i64>,
    pub s: Option<i64>,
    pub e: Option<i64>,
}

/// A value not yet seen (loaded from memory, a register read): the accesses it is the value of.
type Load = Rc<RefCell<Vec<usize>>>;

#[derive(Debug, Clone)]
enum Val {
    Int(i64),
    Load(Load),
    Unknown,
}

impl Val {
    fn int(&self) -> Option<i64> {
        match self {
            Val::Int(v) => Some(*v),
            _ => None,
        }
    }
}

fn sx(v: i64, bits: u32) -> i64 {
    let v = v & ((1i64 << bits) - 1);
    if (v >> (bits - 1)) & 1 == 1 {
        v - (1i64 << bits)
    } else {
        v
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct F {
    rd: i64,
    rs1: i64,
    rs2: Option<i64>,
    imm: i64,
}

/// The instruction at byte offset pc of the program buffer: (length, name, fields).
fn decode(pb: &BTreeMap<i64, i64>, pc: i64) -> Option<(i64, &'static str, F)> {
    let w = *pb.get(&pc.div_euclid(4))?;
    let h = if pc % 4 != 0 { (w >> 16) & 0xFFFF } else { w & 0xFFFF };
    if h & 3 != 3 {
        let (n, f) = decode_c(h);
        return Some((2, n, f));
    }
    let w = if pc % 4 != 0 {
        let nxt = *pb.get(&(pc.div_euclid(4) + 1))?;
        ((nxt & 0xFFFF) << 16) | h
    } else {
        w
    };
    let (n, f) = decode_32(w);
    Some((4, n, f))
}

fn decode_32(w: i64) -> (&'static str, F) {
    let (op, rd, f3, rs1, rs2, f7) = (
        w & 0x7F,
        (w >> 7) & 31,
        (w >> 12) & 7,
        (w >> 15) & 31,
        (w >> 20) & 31,
        w >> 25,
    );
    let i_imm = sx(w >> 20, 12);
    if w == 0x0010_0073 {
        return ("ebreak", F::default());
    }
    let fr = |imm| F {
        rd,
        rs1,
        rs2: None,
        imm,
    };
    match op {
        0x03 => (
            match f3 {
                0 => "lb",
                1 => "lh",
                2 => "lw",
                4 => "lbu",
                5 => "lhu",
                _ => "?",
            },
            fr(i_imm),
        ),
        0x23 => {
            let imm = sx(((w >> 25) << 5) | ((w >> 7) & 31), 12);
            (
                match f3 {
                    0 => "sb",
                    1 => "sh",
                    2 => "sw",
                    _ => "?",
                },
                F {
                    rd: 0,
                    rs1,
                    rs2: Some(rs2),
                    imm,
                },
            )
        }
        0x13 => {
            let name = match f3 {
                0 => "addi",
                2 => "slti",
                3 => "sltiu",
                4 => "xori",
                6 => "ori",
                7 => "andi",
                1 => "slli",
                _ => {
                    if f7 & 0x20 != 0 {
                        "srai"
                    } else {
                        "srli"
                    }
                }
            };
            (name, fr(if f3 == 1 || f3 == 5 { rs2 } else { i_imm }))
        }
        0x33 if f7 == 0 || f7 == 0x20 => {
            let name = match (f3, f7) {
                (0, 0) => "add",
                (0, 0x20) => "sub",
                (4, 0) => "xor",
                (6, 0) => "or",
                (7, 0) => "and",
                (1, 0) => "sll",
                (5, 0) => "srl",
                _ => "?",
            };
            (
                name,
                F {
                    rd,
                    rs1,
                    rs2: Some(rs2),
                    imm: 0,
                },
            )
        }
        0x37 => ("lui", fr(w & 0xFFFF_F000)),
        0x63 => {
            let imm = sx(
                ((w >> 31) << 12) | (((w >> 7) & 1) << 11) | (((w >> 25) & 0x3F) << 5) | (((w >> 8) & 15) << 1),
                13,
            );
            (
                match f3 {
                    0 => "beq",
                    1 => "bne",
                    4 => "blt",
                    5 => "bge",
                    6 => "bltu",
                    7 => "bgeu",
                    _ => "?",
                },
                F {
                    rd: 0,
                    rs1,
                    rs2: Some(rs2),
                    imm,
                },
            )
        }
        0x6F => {
            let imm = sx(
                ((w >> 31) << 20) | (((w >> 12) & 0xFF) << 12) | (((w >> 20) & 1) << 11) | (((w >> 21) & 0x3FF) << 1),
                21,
            );
            ("jal", fr(imm))
        }
        _ => ("?", F::default()),
    }
}

fn decode_c(h: i64) -> (&'static str, F) {
    let (q, f3) = (h & 3, h >> 13);
    let (r3a, r3b) = (8 + ((h >> 7) & 7), 8 + ((h >> 2) & 7));
    let (rd, rs2) = ((h >> 7) & 31, (h >> 2) & 31);
    let imm6 = sx((((h >> 12) & 1) << 5) | ((h >> 2) & 31), 6);
    let f = |rd, rs1, rs2: Option<i64>, imm| F { rd, rs1, rs2, imm };
    if h == 0x9002 {
        return ("ebreak", F::default());
    }
    if q == 0 {
        let off = (((h >> 10) & 7) << 3) | (((h >> 6) & 1) << 2) | (((h >> 5) & 1) << 6);
        return match f3 {
            2 => ("lw", f(r3b, r3a, None, off)),
            6 => ("sw", f(0, r3a, Some(r3b), off)),
            _ => ("?", F::default()),
        };
    }
    if q == 1 {
        return match f3 {
            0 => ("addi", f(rd, rd, None, imm6)),
            2 => ("addi", f(rd, 0, None, imm6)), // c.li
            3 if rd != 0 && rd != 2 => ("lui", f(rd, 0, None, (imm6 << 12) & MASK)),
            4 => match (h >> 10) & 3 {
                0 => ("srli", f(r3a, r3a, None, imm6 & 31)),
                1 => ("srai", f(r3a, r3a, None, imm6 & 31)),
                2 => ("andi", f(r3a, r3a, None, imm6)),
                _ => {
                    let name = ["sub", "xor", "or", "and"][((h >> 5) & 3) as usize];
                    (name, f(r3a, r3a, Some(r3b), 0))
                }
            },
            5 => {
                let b = h >> 2;
                let imm = sx(
                    (((h >> 12) & 1) << 11)
                        | (((h >> 8) & 1) << 10)
                        | (((h >> 9) & 3) << 8)
                        | (((h >> 6) & 1) << 7)
                        | (((h >> 7) & 1) << 6)
                        | (((h >> 2) & 1) << 5)
                        | (((h >> 11) & 1) << 4)
                        | (((b >> 1) & 7) << 1),
                    12,
                );
                ("jal", f(0, 0, None, imm)) // c.j
            }
            6 | 7 => {
                let imm = sx(
                    (((h >> 12) & 1) << 8)
                        | (((h >> 5) & 3) << 6)
                        | (((h >> 2) & 1) << 5)
                        | (((h >> 10) & 3) << 3)
                        | (((h >> 3) & 3) << 1),
                    9,
                );
                (if f3 == 6 { "beq" } else { "bne" }, f(0, r3a, Some(0), imm))
            }
            _ => ("?", F::default()),
        };
    }
    match f3 {
        0 => ("slli", f(rd, rd, None, imm6 & 31)),
        4 => {
            if (h >> 12) & 1 == 0 {
                if rs2 != 0 {
                    ("addi", f(rd, rs2, None, 0)) // c.mv
                } else {
                    ("?", F::default())
                }
            } else if rd != 0 && rs2 != 0 {
                ("add", f(rd, rd, Some(rs2), 0))
            } else {
                ("?", F::default())
            }
        }
        _ => ("?", F::default()),
    }
}

fn alu(name: &str, a: i64, b: i64) -> Option<i64> {
    Some(match name {
        "addi" | "add" => a + b,
        "sub" => a - b,
        "xori" | "xor" => a ^ b,
        "ori" | "or" => a | b,
        "andi" | "and" => a & b,
        "slli" | "sll" => a << (b & 31),
        "srli" | "srl" => a >> (b & 31),
        "srai" => sx(a, 32) >> (b & 31),
        "slti" => (sx(a, 32) < b) as i64,
        "sltiu" => (a < (b & MASK)) as i64,
        _ => return None,
    })
}

fn branch(name: &str, a: i64, b: i64) -> Option<bool> {
    Some(match name {
        "beq" => a == b,
        "bne" => a != b,
        "blt" => sx(a, 32) < sx(b, 32),
        "bge" => sx(a, 32) >= sx(b, 32),
        "bltu" => a < b,
        "bgeu" => a >= b,
        _ => return None,
    })
}

fn size_of(name: &str) -> i64 {
    match name {
        "lb" | "lbu" | "sb" => 1,
        "lh" | "lhu" | "sh" => 2,
        _ => 4,
    }
}

/// The Debug Module as the debugger drives it, read access by access.
#[derive(Default)]
struct Dm {
    data: BTreeMap<i64, Val>,
    progbuf: BTreeMap<i64, i64>,
    gpr: BTreeMap<i64, Val>,
    dataaddr: Option<i64>,
    command: Option<i64>,
    autoexec: i64,
    out: Vec<Entry>,
    now: (Option<i64>, Option<i64>),
}

impl Dm {
    fn new(dataaddr: Option<i64>) -> Self {
        let mut d = Dm {
            dataaddr,
            ..Default::default()
        };
        d.gpr.insert(0, Val::Int(0));
        d
    }

    fn put(&mut self, op: &'static str, addr: Option<i64>, value: Data, size: Option<i64>) -> usize {
        self.out.push(Entry {
            op,
            addr,
            value,
            size,
            s: self.now.0,
            e: self.now.1,
        });
        self.out.len() - 1
    }

    /// A Load of the entries not yet known.
    fn load(&self, entries: Vec<usize>) -> Val {
        Val::Load(Rc::new(RefCell::new(
            entries
                .into_iter()
                .filter(|&e| self.out[e].value == Data::Null)
                .collect(),
        )))
    }

    fn resolve(&mut self, l: &Load, value: i64, at: Option<i64>) {
        for e in l.borrow_mut().drain(..) {
            let size = self.out[e].size.unwrap_or(4);
            self.out[e].value = Data::Num(if size >= 4 {
                value
            } else {
                value & ((1 << (8 * size)) - 1)
            });
            if at.is_some() {
                self.out[e].e = at;
            }
        }
    }

    fn data_index(&self, addr: i64) -> Option<i64> {
        let d = self.dataaddr?;
        (0..48).contains(&(addr - d)).then(|| (addr - d).div_euclid(4))
    }

    fn gget(&self, r: i64) -> Val {
        self.gpr.get(&r).cloned().unwrap_or(Val::Unknown)
    }

    /// The program buffer run on what is known.
    fn run(&mut self) {
        let (mut pc, mut steps) = (0i64, 0);
        while steps < 64 {
            steps += 1;
            let Some((n, name, f)) = decode(&self.progbuf, pc) else {
                return;
            };
            match name {
                "ebreak" => return,
                "lb" | "lbu" | "lh" | "lhu" | "lw" => {
                    let Some(base) = self.gget(f.rs1).int() else {
                        return;
                    };
                    let a = (base + f.imm) & MASK;
                    let size = size_of(name);
                    let v = match self.data_index(a) {
                        Some(k) => {
                            let v = self.data.get(&k).cloned().unwrap_or(Val::Unknown);
                            match v {
                                Val::Int(x) if size < 4 => {
                                    let mut y = (x >> (8 * (a % 4))) & ((1 << (8 * size)) - 1);
                                    if name == "lb" || name == "lh" {
                                        y = sx(y, 8 * size as u32) & MASK;
                                    }
                                    Val::Int(y)
                                }
                                other => other,
                            }
                        }
                        None => {
                            let e = self.put("MEMR", Some(a), Data::Null, Some(size));
                            self.load(vec![e])
                        }
                    };
                    if f.rd != 0 {
                        self.gpr.insert(f.rd, v);
                    }
                }
                "sb" | "sh" | "sw" => {
                    let Some(base) = self.gget(f.rs1).int() else {
                        return;
                    };
                    let v = self.gget(f.rs2.unwrap_or(0));
                    let a = (base + f.imm) & MASK;
                    match self.data_index(a) {
                        Some(k) => {
                            self.data.insert(k, v);
                        }
                        None => {
                            let size = size_of(name);
                            let val = v.int().map(|x| if size < 4 { x & ((1 << (8 * size)) - 1) } else { x });
                            self.put("MEMW", Some(a), val.map_or(Data::Null, Data::Num), Some(size));
                        }
                    }
                }
                "lui" => {
                    if f.rd != 0 {
                        self.gpr.insert(f.rd, Val::Int(f.imm & MASK));
                    }
                }
                "jal" => {
                    if f.rd != 0 {
                        self.gpr.insert(f.rd, Val::Unknown);
                    }
                    pc += f.imm;
                    continue;
                }
                _ if branch(name, 0, 0).is_some() => {
                    let (Some(a), Some(b)) = (self.gget(f.rs1).int(), self.gget(f.rs2.unwrap_or(0)).int()) else {
                        return; // a branch on what is not known: stop here
                    };
                    if branch(name, a, b) == Some(true) {
                        pc += f.imm;
                        continue;
                    }
                }
                _ if alu(name, 0, 0).is_some() => {
                    let a = self.gget(f.rs1).int();
                    let b = match f.rs2 {
                        Some(r) => self.gget(r).int(),
                        None => Some(f.imm),
                    };
                    if f.rd != 0 {
                        let v = match (a, b) {
                            (Some(a), Some(b)) => Val::Int(alu(name, a, b).unwrap_or(0) & MASK),
                            _ => Val::Unknown,
                        };
                        self.gpr.insert(f.rd, v);
                    }
                }
                _ => return, // an instruction not followed: stop
            }
            pc += n;
        }
    }

    fn execute(&mut self, cmd: i64) {
        let kind = cmd >> 24;
        let (write, transfer, postexec) = ((cmd >> 16) & 1, (cmd >> 17) & 1, (cmd >> 18) & 1);
        if kind == 0 {
            let regno = cmd & 0xFFFF;
            if transfer == 1 {
                if write == 1 {
                    let v = self.data.get(&0).cloned().unwrap_or(Val::Unknown);
                    self.put("REGW", Some(regno), v.int().map_or(Data::Null, Data::Num), None);
                    if (0x1001..0x1020).contains(&regno) {
                        self.gpr.insert(regno - 0x1000, v);
                    }
                } else {
                    let v = if (0x1000..0x1020).contains(&regno) {
                        self.gget(regno - 0x1000)
                    } else {
                        Val::Unknown
                    };
                    let e = self.put("REGR", Some(regno), Data::Null, None);
                    let mut entries = vec![e];
                    if let Val::Load(l) = &v {
                        entries.extend(l.borrow().iter().copied());
                    }
                    let l = self.load(entries);
                    self.data.insert(0, l);
                }
            }
            if postexec == 1 {
                self.run();
            }
        } else if kind == 2 {
            let a = self.data.get(&1).and_then(|v| v.int());
            let size = 1i64 << ((cmd >> 20) & 7);
            if write == 1 {
                let v = self.data.get(&0).and_then(|v| v.int());
                self.put("MEMW", a, v.map_or(Data::Null, Data::Num), Some(size));
            } else {
                let e = self.put("MEMR", a, Data::Null, Some(size));
                let l = self.load(vec![e]);
                self.data.insert(0, l);
            }
            if (cmd >> 19) & 1 == 1
                && let Some(a) = a
            {
                self.data.insert(1, Val::Int((a + size) & MASK));
            }
        }
    }

    fn feed(&mut self, op: &str, addr: Option<i64>, data: &Data, s: Option<i64>, e: Option<i64>) {
        self.now = (s, e.or(s));
        if op == "BURST" {
            match (addr, data) {
                (Some(a), Data::List(vs)) if (0x04..=0x0F).contains(&a) => {
                    for &v in vs {
                        self.feed("R", Some(a), &Data::Num(v), s, e);
                    }
                }
                _ => {
                    let d1 = self.data.get(&1).and_then(|v| v.int());
                    self.put("BURST", d1, data.clone(), None);
                }
            }
            return;
        }
        let Some(addr) = addr else { return };
        let num = match data {
            Data::Num(v) => Some(*v),
            _ => None,
        };
        if (0x04..=0x0F).contains(&addr) {
            let k = addr - 0x04;
            if op != "W"
                && let Some(Val::Load(l)) = self.data.get(&k).cloned()
                && let Some(v) = num
            {
                let at = self.now.1;
                self.resolve(&l, v, at);
            }
            self.data.insert(k, num.map_or(Val::Unknown, Val::Int));
            if let Some(c) = self.command
                && (self.autoexec >> k) & 1 == 1
            {
                self.execute(c);
            }
        } else if (0x20..=0x2F).contains(&addr) {
            if op == "W"
                && let Some(v) = num
            {
                self.progbuf.insert(addr - 0x20, v);
            }
            if let Some(c) = self.command
                && (self.autoexec >> (16 + addr - 0x20)) & 1 == 1
            {
                self.execute(c);
            }
        } else if addr == 0x17 && op == "W" {
            if let Some(v) = num {
                self.command = Some(v);
                self.execute(v);
            }
        } else if addr == 0x18 {
            self.autoexec = num.unwrap_or(0);
        } else if addr == 0x12
            && op == "R"
            && let Some(v) = num
            && (v >> 16) & 1 == 0
        {
            self.dataaddr = Some(sx(v & 0xFFF, 12) & MASK);
        }
    }
}

/// An access that did not happen: one not read whole, or one the DMI answered failed or busy.
fn skip(x: &Dmi) -> bool {
    x.op != "BURST" && (!x.ok || x.long_failed)
}

/// The data registers' address: hartinfo when read; else the address (of those a register is set to, or the
/// programs touch) with which the programs read the most of memory.
fn learnt(tx: &[Dmi]) -> Option<i64> {
    for x in tx {
        if x.op == "R"
            && x.addr == Some(0x12)
            && x.ok
            && let Data::Num(d) = x.data
            && (d >> 16) & 1 == 0
        {
            return Some(sx(d & 0xFFF, 12) & MASK);
        }
    }
    let mut cands: Vec<i64> = vec![];
    let mut data0: Option<i64> = None;
    for x in tx {
        let Data::Num(d) = x.data else { continue };
        if x.op == "W" && x.addr == Some(0x04) {
            data0 = Some(d);
        } else if x.op == "W"
            && x.addr == Some(0x17)
            && let Some(d0) = data0
            && d >> 24 == 0
            && (d >> 16) & 7 == 3
            && d0 % 4 == 0
            && !cands.contains(&d0)
        {
            cands.push(d0);
            if cands.len() >= 8 {
                break;
            }
        }
    }
    let mut dry = Dm::new(None);
    for x in tx.iter().filter(|x| !skip(x)) {
        dry.feed(&x.op, x.addr, &x.data, None, None);
    }
    let mut seen: Vec<(i64, usize)> = vec![];
    for en in &dry.out {
        if (en.op == "MEMR" || en.op == "MEMW")
            && let Some(a) = en.addr
        {
            for k in 0..4 {
                let c = (a - 4 * k) & MASK;
                if c % 4 == 0 {
                    match seen.iter_mut().find(|s| s.0 == c) {
                        Some(s) => s.1 += 1,
                        None => seen.push((c, 1)),
                    }
                }
            }
        }
    }
    seen.sort_by_key(|s| std::cmp::Reverse(s.1));
    for &(c, _) in seen.iter().take(16) {
        if !cands.contains(&c) {
            cands.push(c);
        }
    }
    let read = |out: &[Entry]| out.iter().filter(|e| e.op == "MEMR" && e.value != Data::Null).count();
    let (mut best, mut best_n) = (None, read(&dry.out));
    for a in cands {
        let mut dm = Dm::new(Some(a));
        for x in tx.iter().filter(|x| !skip(x)) {
            dm.feed(&x.op, x.addr, &x.data, x.s, x.e);
        }
        let n = read(&dm.out);
        if n > best_n {
            (best, best_n) = (Some(a), n);
        }
    }
    best
}

/// The register and memory accesses the DMI accesses carry, in order.
pub fn accesses(tx: &[Dmi]) -> Vec<Entry> {
    let mut dm = Dm::new(learnt(tx));
    for x in tx.iter().filter(|x| !skip(x)) {
        dm.feed(&x.op, x.addr, &x.data, x.s, x.e);
    }
    dm.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(addr: i64, data: i64) -> Dmi {
        Dmi {
            op: "W".into(),
            addr: Some(addr),
            data: Data::Num(data),
            ok: true,
            long_failed: false,
            s: Some(addr),
            e: None,
        }
    }

    fn r(addr: i64, data: i64) -> Dmi {
        Dmi {
            op: "R".into(),
            ..w(addr, data)
        }
    }

    #[test]
    fn access_memory_and_a_program_buffer_load() {
        // Access Memory: data1 = address, command 0x02200000 (32-bit read), data0 read
        let tx = vec![
            w(0x05, 0x0800_0000),
            w(0x17, 0x0220_0000),
            r(0x04, 0x1234_5678),
            // a program: lw x8, 0(x8); sw x8, 0(x9) with x9 = data0's address
            r(0x12, 0x0000_0380), // hartinfo: data registers in memory at 0x380
            w(0x20, 0x0004_2403), // lw s0, 0(s0)
            w(0x21, 0x0084_a023), // sw s0, 0(s1)
            w(0x22, 0x0010_0073), // ebreak
            w(0x04, 0x380),
            w(0x17, 0x0023_1009), // write x9
            w(0x04, 0x2000_0010),
            w(0x17, 0x0027_1008), // write x8, then run the program
            r(0x04, 0xCAFE_F00D),
        ];
        let out = accesses(&tx);
        let mem: Vec<(&str, Option<i64>, Data)> = out
            .iter()
            .filter(|e| e.op.starts_with("MEM"))
            .map(|e| (e.op, e.addr, e.value.clone()))
            .collect();
        assert_eq!(mem[0], ("MEMR", Some(0x0800_0000), Data::Num(0x1234_5678)));
        assert_eq!(mem[1], ("MEMR", Some(0x2000_0010), Data::Num(0xCAFE_F00D)));
    }
}
