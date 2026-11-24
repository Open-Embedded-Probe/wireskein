//! Classic CAN on one line at the bus level (Python's `_engine/canbus.py`, read as a receiving node does, and the
//! main engine's `decl/can.toml`): a frame starts at a recessive-to-dominant edge after the bus was idle (11
//! recessive bits; 3 after a frame's end), each bit sampled at the sample point and resynchronized at each
//! recessive-to-dominant edge; stuffing after five equal bits from the start of frame to the CRC; the CRC-15; the
//! fixed-form bits recessive. Each distinct frame's CRC is the evidence.

use crate::Candidate;
use crate::features::features;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const CRC_POLY: u32 = 0x4599;
pub const SP: f64 = 0.75;
pub const MAX_NOMINAL: f64 = 1.05e6;
pub const FORBIDDEN_RUNS: f64 = 0.05;
pub const EVIDENCE_BITS: f64 = 8.0;
pub const GIVE_UP: usize = 16; // errors with no frame read, classic: a line of no CAN at this bit time
pub const FD_GIVE_UP: usize = 2; // ... and at each nominal rate tried for CAN FD
pub const SHORT_PCT: f64 = 0.05; // the shortest runs' quantile: a data phase's bit, a base for the nominal one
pub const FD_CHECK_BITS: f64 = 4.0; // an FD frame's stuff count (three bits Gray-coded and a parity) checked

fn crc15(bits: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bits {
        let nxt = b as u32 ^ ((crc >> 14) & 1);
        crc = (crc << 1) & 0x7FFF;
        if nxt == 1 {
            crc ^= CRC_POLY;
        }
    }
    crc
}

struct Line<'a> {
    e: &'a [i64],
    initial: u8,
    idle: u8,
    falls: Vec<i64>,
    rises: Vec<i64>,
}

impl<'a> Line<'a> {
    fn new(ch: &'a Channel, idle: u8) -> Self {
        let (mut falls, mut rises) = (vec![], vec![]);
        for (k, &t) in ch.edges.iter().enumerate() {
            let after = ch.initial ^ (((k + 1) & 1) as u8);
            if after != idle { falls.push(t) } else { rises.push(t) }
        }
        Line {
            e: ch.edges,
            initial: ch.initial,
            idle,
            falls,
            rises,
        }
    }
    fn level(&self, t: f64) -> u8 {
        let k = self.e.partition_point(|&x| (x as f64) <= t);
        self.initial ^ (k as u8 & 1)
    }
    fn fall_between(&self, a: f64, b: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) <= a);
        (k < self.falls.len() && self.falls[k] as f64 <= b).then(|| self.falls[k])
    }
    fn next_fall(&self, t: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) < t);
        self.falls.get(k).copied()
    }
    fn recessive_since(&self, t: f64) -> f64 {
        let k = self.rises.partition_point(|&x| (x as f64) <= t);
        if k > 0 {
            self.rises[k - 1] as f64
        } else if self.initial == self.idle {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }
}

enum Stop {
    Error(f64),
    Cut,
}

pub const FD_LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];
pub const CRC17_POLY: u32 = 0x1685B;
pub const CRC21_POLY: u32 = 0x102899;
pub const GRAY: [u32; 8] = [0, 1, 3, 2, 6, 7, 5, 4]; // the stuff count mod 8, Gray-coded, as sent
pub const FD_RATIOS: [f64; 6] = [1.0, 2.0, 4.0, 5.0, 8.0, 10.0]; // data rate over nominal, tried in turn
pub const SP_DATA: f64 = 0.75;

/// CAN FD's CRC-17 / CRC-21 (the register starts with its top bit set).
pub fn crc_fd(bits: &[u8], width: u32, poly: u32) -> u32 {
    let top = 1u32 << (width - 1);
    let mask = (1u32 << width) - 1;
    let mut crc = top;
    for &b in bits {
        let nxt = b as u32 ^ u32::from(crc & top != 0);
        crc = (crc << 1) & mask;
        if nxt == 1 {
            crc ^= poly;
        }
    }
    crc
}

/// What an FD reading saw before it failed (Python's `seen`): FDF recessive, BRS, five equal bits ending the data.
#[derive(Default, Clone, Copy)]
struct Seen {
    fd: bool,
    brs: bool,
    five: bool,
}

/// A node reading one frame: its bit timing (resynchronized at each recessive-to-dominant edge) and its stuffing.
struct Reader<'a> {
    line: &'a Line<'a>,
    n: f64,
    edge: f64,
    prev: f64,
    k: usize,
    u: f64,
    sp: f64,
    rv: u8,
    rc: usize,
    stuff: usize,
    body: Vec<u8>,
    sent: Vec<u8>,
}

impl Reader<'_> {
    /// The next bit at the sample point, resynchronized at a recessive-to-dominant edge since the last sample.
    fn raw(&mut self) -> Result<(u8, f64), Stop> {
        let nominal = self.edge + self.sp * self.u;
        if self.k > 0
            && let Some(f) = self.line.fall_between(self.prev, nominal)
        {
            self.edge = f as f64;
        }
        let t = self.edge + self.sp * self.u;
        if t >= self.n {
            return Err(Stop::Cut);
        }
        self.prev = t;
        self.edge += self.u;
        self.k += 1;
        Ok((u8::from(self.line.level(t) == self.line.idle), t))
    }

    /// At the last bit's sample point the bit time changes: the rest of that bit is the new timing's phase 2.
    fn switch(&mut self, u: f64, sp: f64) {
        self.u = u;
        self.sp = sp;
        self.edge = self.prev + (1.0 - sp) * u;
    }

    /// A bit of the stuffed part (a stuff bit after five equal ones read and checked first).
    fn stuffed(&mut self) -> Result<u8, Stop> {
        let (mut b, t) = self.raw()?;
        if self.rc == 5 {
            if b == self.rv {
                return Err(Stop::Error(t));
            }
            self.sent.push(b);
            self.rv = b;
            self.rc = 1;
            self.stuff += 1;
            b = self.raw()?.0;
        }
        self.rc = if b == self.rv { self.rc + 1 } else { 1 };
        self.rv = b;
        self.body.push(b);
        self.sent.push(b);
        Ok(b)
    }

    fn bits(&mut self, m: usize) -> Result<u64, Stop> {
        let mut v = 0u64;
        for _ in 0..m {
            v = (v << 1) | self.stuffed()? as u64;
        }
        Ok(v)
    }

    fn fixed(&mut self) -> Result<(), Stop> {
        let (b, t) = self.raw()?;
        if b != 1 { Err(Stop::Error(t)) } else { Ok(()) }
    }

    /// An FD frame's fixed stuff bit: the complement of the bit before.
    fn fixed_stuff(&mut self) -> Result<(), Stop> {
        let (b, t) = self.raw()?;
        if b == self.rv {
            return Err(Stop::Error(t));
        }
        self.rv = b;
        Ok(())
    }

    fn plain(&mut self) -> Result<u8, Stop> {
        let (b, _) = self.raw()?;
        self.rv = b;
        Ok(b)
    }

    /// From ACK to the end of frame (a dominant last bit an overload's start).
    fn ack_and_eof(&mut self, f: &mut Map<String, Value>) -> Result<(), Stop> {
        let (ack, _) = self.raw()?;
        self.fixed()?; // ACK delimiter
        for _ in 0..6 {
            self.fixed()?;
        }
        let (last, t) = self.raw()?;
        f.insert("e".into(), json!(t as i64));
        f.insert("ack".into(), json!(ack == 0));
        if last == 0 {
            f.insert("overload".into(), json!(t as i64));
        }
        Ok(())
    }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// One frame from its start of frame: (the frame, the tick its end of frame ends). `fd_ratio`: CAN FD's frames read
/// too (FDF recessive), their data phase (BRS recessive) at u / ratio; None: classic CAN alone (r0 / r1 whatever
/// their value). `boundary`: a dynamic stuff bit read after five equal bits ending an FD frame's data.
fn read_frame(
    line: &Line,
    sof: i64,
    u: f64,
    n: i64,
    fd_ratio: Option<f64>,
    boundary: bool,
    seen: &mut Seen,
) -> Result<(Map<String, Value>, f64), Stop> {
    let mut r = Reader {
        line,
        n: n as f64,
        edge: sof as f64,
        prev: sof as f64,
        k: 0,
        u,
        sp: SP,
        rv: 2,
        rc: 0,
        stuff: 0,
        body: vec![],
        sent: vec![],
    };
    if r.bits(1)? != 0 {
        return Err(Stop::Error(sof as f64 + SP * u));
    }
    let id_a = r.bits(11)?;
    let rtr_srr = r.bits(1)?;
    let ide = r.bits(1)?;
    let (ident, remote, mut fdf) = if ide == 1 {
        let id_b = r.bits(18)?;
        let rtr = r.bits(1)?;
        let r1 = r.bits(1)?;
        ((id_a << 18) | id_b, rtr, r1)
    } else {
        (id_a, rtr_srr, 0)
    };
    let mut f = Map::new();
    f.insert("s".into(), json!(sof));
    f.insert("start".into(), json!(sof));
    f.insert("id".into(), json!(ident));
    f.insert("ide".into(), json!(ide == 1));
    match fd_ratio {
        None => {
            r.bits(1)?; // r0
        }
        Some(ratio) => {
            if ide == 0 {
                fdf = r.bits(1)?; // r0's place in a classic frame
            }
            if fdf == 1 {
                seen.fd = true;
                read_fd(&mut r, sof, u, ratio, boundary, seen, &mut f)?;
                return Ok((f, r.edge));
            }
            if ide == 1 {
                r.bits(1)?; // r0
            }
        }
    }
    let dlc = r.bits(4)?;
    let nbytes = if remote == 1 { 0 } else { (dlc as usize).min(8) };
    let mut data = vec![];
    for _ in 0..nbytes {
        data.push(r.bits(8)? as u8);
    }
    let covered = r.body.len();
    let crc = r.bits(15)?;
    if r.rc == 5 {
        let (b, t) = r.raw()?;
        if b == r.rv {
            return Err(Stop::Error(t));
        }
    }
    if crc as u32 != crc15(&r.body[..covered]) {
        let (_, t) = r.raw()?;
        return Err(Stop::Error(t));
    }
    r.fixed()?; // CRC delimiter
    r.ack_and_eof(&mut f)?;
    f.insert("rtr".into(), json!(remote == 1));
    f.insert("dlc".into(), json!(dlc));
    f.insert("data".into(), json!(hex(&data)));
    f.insert("ok".into(), json!(true));
    Ok((f, r.edge))
}

/// The rest of a CAN FD frame after FDF: res, BRS, ESI, DLC, data (dynamically stuffed), the stuff count and the CRC
/// (a fixed stuff bit, the complement of the bit before, ahead of them and after every 4 bits), the delimiter, ACK.
/// With BRS recessive the bits from after BRS's sample point to the CRC delimiter's are at u / ratio.
fn read_fd(
    r: &mut Reader,
    sof: i64,
    u: f64,
    ratio: f64,
    boundary: bool,
    seen: &mut Seen,
    f: &mut Map<String, Value>,
) -> Result<(), Stop> {
    r.bits(1)?; // res
    let brs = r.bits(1)?;
    seen.brs = brs == 1;
    if brs == 1 {
        r.switch(u / ratio, SP_DATA);
    }
    let esi = r.bits(1)?;
    let dlc = r.bits(4)? as usize;
    let nbytes = FD_LENGTHS[dlc];
    let mut data = vec![];
    for _ in 0..nbytes {
        data.push(r.bits(8)? as u8);
    }
    seen.five = r.rc == 5;
    if boundary {
        if r.rc != 5 {
            return Err(Stop::Error(sof as f64)); // (no such bit: the reading without it is the one)
        }
        // five equal bits ending the data: a dynamic stuff bit before the fixed one, counted and under the CRC
        let (b, t) = r.raw()?;
        if b == r.rv {
            return Err(Stop::Error(t));
        }
        r.sent.push(b);
        r.stuff += 1;
        r.rv = b;
    }
    let mut covered = r.sent.clone();
    let (width, poly) = if nbytes <= 16 {
        (17, CRC17_POLY)
    } else {
        (21, CRC21_POLY)
    };
    r.fixed_stuff()?;
    let mut sc = [0u8; 4];
    for x in sc.iter_mut() {
        *x = r.plain()?;
    }
    let mut crc_bits = vec![];
    let mut k = 0;
    while crc_bits.len() < width as usize {
        if k % 4 == 0 {
            r.fixed_stuff()?;
        }
        crc_bits.push(r.plain()?);
        k += 1;
    }
    let count = ((sc[0] as usize) << 2) | ((sc[1] as usize) << 1) | sc[2] as usize;
    if (sc[0] + sc[1] + sc[2]) & 1 != sc[3] || count as u32 != GRAY[r.stuff % 8] {
        let (_, t) = r.raw()?;
        return Err(Stop::Error(t));
    }
    let crc = crc_bits.iter().fold(0u32, |a, &b| (a << 1) | b as u32);
    covered.extend(sc);
    if crc != crc_fd(&covered, width, poly) {
        let (_, t) = r.raw()?;
        return Err(Stop::Error(t));
    }
    r.fixed()?; // CRC delimiter
    if brs == 1 {
        r.switch(u, SP);
    }
    r.ack_and_eof(f)?;
    f.insert("rtr".into(), json!(false));
    f.insert("dlc".into(), json!(dlc));
    f.insert("data".into(), json!(hex(&data)));
    f.insert("ok".into(), json!(true));
    f.insert("fd".into(), json!(true));
    f.insert("brs".into(), json!(brs == 1));
    f.insert("esi".into(), json!(esi == 1));
    Ok(())
}

/// A frame read classic, or with CAN FD's frames at each data rate in turn (with and without a dynamic stuff bit at
/// the data's end): the first that holds; else the error of the reading that got furthest.
fn read_any(line: &Line, sof: i64, u: f64, n: i64, fd: bool) -> Result<(Map<String, Value>, f64), Stop> {
    if !fd {
        return read_frame(line, sof, u, n, None, false, &mut Seen::default());
    }
    let (mut worst, mut cut) = (None::<f64>, false);
    let mut first: Option<Seen> = None;
    let mut seen = Seen::default();
    'rates: for (ri, &ratio) in FD_RATIOS.iter().enumerate() {
        for bs in [false, true] {
            if ri > 0 && first.is_some_and(|f| !f.brs) {
                break 'rates; // no BRS: every data rate reads the same
            }
            if bs && !seen.five {
                continue; // no five equal bits at the data's end: no second reading of the boundary
            }
            seen = Seen::default();
            match read_frame(line, sof, u, n, Some(ratio), bs, &mut seen) {
                Ok((mut fr, end)) => {
                    if fr.get("brs") == Some(&json!(true)) {
                        fr.insert("data_ratio".into(), json!(ratio));
                    }
                    return Ok((fr, end));
                }
                Err(e) if !seen.fd => return Err(e), // classic, or before FDF: the same at every data rate
                Err(Stop::Error(t)) => {
                    first.get_or_insert(seen);
                    if worst.is_none_or(|w| t > w) {
                        worst = Some(t);
                    }
                }
                Err(Stop::Cut) => {
                    first.get_or_insert(seen);
                    cut = true; // (a data rate too slow runs past the capture's end)
                }
            }
        }
    }
    if cut {
        return Err(Stop::Cut);
    }
    Err(Stop::Error(worst.unwrap_or(sof as f64)))
}

/// The frames and the errors a node at bit time u (ticks) reads (Python's `canbus.read`); `fd`: CAN FD's frames read
/// too, each at the first data rate its stuff count and CRC hold at (`data_ratio`); `give_up`: errors with no frame
/// read yet after which the line is no CAN at this bit time.
pub fn read(ch: &Channel, n: i64, u: f64, idle: u8, fd: bool, give_up: usize) -> (Vec<Map<String, Value>>, usize) {
    let line = Line::new(ch, idle);
    let (mut frames, mut errors) = (vec![], 0usize);
    let (mut need, mut after, mut pos) = (11.0, f64::NEG_INFINITY, 0.0f64);
    while let Some(f) = line.next_fall(pos) {
        let since = line.recessive_since(f as f64).max(after);
        if (f as f64) - since < (need - 0.5) * u {
            need = 11.0;
            pos = f as f64 + 1.0;
            continue;
        }
        match read_any(&line, f, u, n, fd) {
            Err(Stop::Cut) => break,
            Err(Stop::Error(t)) => {
                errors += 1;
                need = 11.0;
                after = 0.0;
                pos = t.max(f as f64 + 1.0);
                if frames.is_empty() && errors >= give_up {
                    break;
                }
            }
            Ok((mut fr, end)) => {
                if fr.remove("overload").is_some() {
                    need = 11.0;
                    after = 0.0;
                } else {
                    need = 3.0;
                    after = end;
                }
                pos = end;
                frames.push(fr);
            }
        }
    }
    (frames, errors)
}

/// The bit time from the intervals between recessive-to-dominant edges near whole bits of u0, least squares.
fn bit_time(ch: &Channel, u0: f64, idle: u8) -> Option<f64> {
    let line = Line::new(ch, idle);
    let d: Vec<f64> = line.falls.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.is_empty() {
        return None;
    }
    let mut u = u0;
    for tol in [0.3, 0.3, 0.3, 0.15] {
        let (mut num, mut den) = (0.0, 0.0);
        for &x in &d {
            let k = (x / u).round_ties_even();
            if (1.0..=10.0).contains(&k) && (x / u - k).abs() < tol {
                num += x;
                den += k;
            }
        }
        if den == 0.0 {
            return None;
        }
        u = num / den;
    }
    Some(u)
}

pub const NOMINAL_BITS: f64 = 16.0; // an FD frame's bits from its start to BRS at least (base format)

/// The nominal bit time refit to the intervals between recessive-to-dominant edges in the frames' nominal phases
/// alone (a frame switching its rate: its first NOMINAL_BITS bits; a frame that does not: all of it). Fit to the whole
/// line, a data phase's runs of 3 or 5 of its bits pass for nominal ones and pull the bit time short (a line of long
/// FD frames 3.7 % off); unchanged where those phases hold few intervals.
fn nominal_refit(ch: &Channel, idle: u8, u: f64, frames: &[Map<String, Value>]) -> f64 {
    let line = Line::new(ch, idle);
    let (mut num, mut den, mut n) = (0.0, 0.0, 0usize);
    for f in frames {
        let s = f["s"].as_i64().unwrap_or(0);
        let e = if f.get("brs") == Some(&json!(true)) {
            s + (NOMINAL_BITS * u) as i64
        } else {
            f["e"].as_i64().unwrap_or(s)
        };
        let lo = line.falls.partition_point(|&x| x < s);
        let hi = line.falls.partition_point(|&x| x <= e);
        for w in line.falls[lo..hi].windows(2) {
            let d = (w[1] - w[0]) as f64;
            let k = (d / u).round_ties_even();
            if (1.0..=10.0).contains(&k) && (d / u - k).abs() < 0.15 {
                num += d;
                den += k;
                n += 1;
            }
        }
    }
    if n < 8 { u } else { num / den }
}

/// The CAN FD reading of a line (Python's `CanFdPlugin`): the nominal bit time a whole multiple of a base (the line's
/// unit, its shortest runs: a data phase's bit, its commonest run: a nominal bit where most bits are nominal ones),
/// tried in turn up to a nominal rate of 1 Mb/s, read with FD's frames at each data rate; the first bit time an FD
/// frame is read at. A line read as classic CAN at its own unit is no FD's. The evidence: each distinct frame's CRC
/// and an FD frame's stuff count.
fn fd_reading(view: &View, ch: &Channel, u0: f64, idle: u8, inner: &[f64]) -> Option<Candidate> {
    let mut bases = vec![u0];
    if !inner.is_empty() {
        let mut sorted = inner.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let short = crate::kernels::quantile(&sorted, SHORT_PCT);
        if (short / u0 - 1.0).abs() > 0.1 {
            bases.push(short);
        }
        let mut hist = std::collections::BTreeMap::new();
        for &x in inner.iter().filter(|&&x| x < 64.0 * u0) {
            *hist.entry(x.round_ties_even() as i64).or_insert(0usize) += 1;
        }
        let top = hist.values().max().copied().unwrap_or(0);
        if let Some((&m, _)) = hist.iter().find(|&(_, &c)| c == top) {
            let m = m as f64;
            if m >= 2.0 && bases.iter().all(|&b| (m / b - 1.0).abs() > 0.1) {
                bases.push(m);
            }
        }
    }
    let mut tried: Vec<f64> = vec![];
    let mut found = None;
    'bases: for &b0 in &bases {
        for &m in &FD_RATIOS {
            if view.rate / (b0 * m) > MAX_NOMINAL {
                continue;
            }
            let u = bit_time(ch, b0 * m, idle).unwrap_or(b0 * m);
            if u < 2.0 || tried.iter().any(|&v| (u / v - 1.0).abs() < 0.03) {
                continue;
            }
            tried.push(u);
            let forbidden = inner.iter().filter(|&&x| x > 5.5 * u && x < 10.5 * u).count() as f64;
            if !inner.is_empty() && forbidden / inner.len() as f64 > FORBIDDEN_RUNS {
                continue;
            }
            let (frames, errors) = read(ch, view.n_samples, u, idle, true, FD_GIVE_UP);
            if frames.iter().any(|f| f.contains_key("fd")) {
                found = Some((u, frames, errors));
                break 'bases;
            }
            if b0 == u0 && m == 1.0 && frames.len() >= 2 && frames.len() >= 4 * errors {
                return None; // classic CAN at the line's own unit
            }
        }
    }
    let (u, frames, errors) = found?;
    let u = nominal_refit(ch, idle, u, &frames);
    let mut seen = std::collections::BTreeSet::new();
    let mut info = 0.0;
    for f in &frames {
        let key: Vec<String> = ["id", "ide", "dlc", "data", "fd", "brs"]
            .iter()
            .map(|k| f.get(*k).map(|v| v.to_string()).unwrap_or_default())
            .collect();
        if !seen.insert(key) {
            continue;
        }
        info += if f.contains_key("fd") {
            let n = FD_LENGTHS[f["dlc"].as_u64().unwrap() as usize];
            (if n <= 16 { 17.0 } else { 21.0 }) + FD_CHECK_BITS
        } else {
            15.0
        };
    }
    let rate = frames.len() as f64 / (frames.len() + errors) as f64;
    let score = rate * rate * (1.0 - 0.5f64.powf(info / EVIDENCE_BITS));
    let ratios: std::collections::BTreeSet<i64> = frames
        .iter()
        .filter_map(|f| f.get("data_ratio").and_then(|r| r.as_f64()).map(|r| r as i64))
        .collect();
    let mut params = Map::new();
    params.insert("bitrate".into(), json!(view.rate / u));
    params.insert("fd".into(), json!(true));
    params.insert("idle".into(), json!(idle));
    if ratios.len() == 1 {
        params.insert(
            "data_bitrate".into(),
            json!(view.rate / u * *ratios.first().unwrap() as f64),
        );
    }
    let mut evidence = Map::new();
    evidence.insert("frames".into(), json!(frames.len()));
    evidence.insert(
        "fd_frames".into(),
        json!(frames.iter().filter(|f| f.contains_key("fd")).count()),
    );
    evidence.insert("errors".into(), json!(errors));
    evidence.insert("ok_rate".into(), json!(rate));
    evidence.insert("evidence_bits".into(), json!(info));
    Some(Candidate {
        protocol: "can".into(),
        roles: [("data".to_string(), ch.name.to_string())].into(),
        score,
        alt: None,
        params,
        evidence,
        layers: crate::claim::raw(serde_json::to_string(&json!({ "frames": frames })).unwrap()),
    })
}

/// The CAN readings of a part: each line at its bit time (its units' candidates, refitted), under its idle level.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    for ch0 in &view.channels {
        if ch0.edges.len() < 40 {
            continue;
        }
        // the line as it is, its short pulses out, its bounces merged (spikes and bounces are bits too short to be a
        // node's: they break the stuffing and the CRC): the best reading of the line over them. The widths a lone
        // line's runs give (a quarter of the median), and the async lines' own (`staged.single_views`: spikes of a
        // sample or two, bounces of 4 ticks), narrow enough for an FD data phase's bits of a few samples
        let mut line_best: Option<Candidate> = None;
        let mut views = crate::pulse::line_views(ch0.edges);
        for (e, k) in crate::uart::views(ch0, &features(ch0, view.n_samples), view.n_samples) {
            if !views.iter().any(|(v, _)| *v == e) {
                views.push((e, k));
            }
        }
        for (e, k) in views {
            let ch = Channel::new(ch0.name, ch0.initial, &e);
            for mut c in read_line(view, &ch) {
                if k != 0 {
                    c.params.insert("deglitch".into(), json!(k));
                }
                if line_best.as_ref().is_none_or(|b| c.score > b.score) {
                    line_best = Some(c);
                }
            }
        }
        out.extend(line_best);
    }
    out
}

/// The CAN readings of one line (a view of it): classic CAN at its bit time and CAN FD.
fn read_line(view: &View, ch: &Channel) -> Vec<Candidate> {
    let mut out = vec![];
    {
        let f = features(ch, view.n_samples);
        let (_, length, _) = crate::kernels::runs(ch, view.n_samples);
        let inner: Vec<f64> = if length.len() > 2 {
            length[1..length.len() - 1].iter().map(|&x| x as f64).collect()
        } else {
            vec![]
        };
        let mut best: Option<Candidate> = None;
        let mut tried: Vec<f64> = vec![];
        // the bit time from the shortest runs (a single bit is common on CAN: stuffing makes them) and from the
        // commonest one, beside the line's own units (which fractions of a bit can fit as well)
        let mut sorted = inner.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut bases: Vec<f64> = vec![];
        if !sorted.is_empty() {
            bases.push(crate::kernels::quantile(&sorted, 0.05));
            let mut hist = std::collections::BTreeMap::new();
            for &x in &sorted {
                *hist.entry(x.round() as i64).or_insert(0usize) += 1;
            }
            if let Some((&m, _)) = hist.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) {
                bases.push(m as f64);
            }
        }
        bases.extend(f.units.iter().take(3).map(|u| u.samples));
        for base in bases {
            let idle = f.idle;
            if base < 2.0 {
                continue;
            }
            let Some(u) = bit_time(ch, base, idle) else {
                continue;
            };
            if u < 2.0 || view.rate / u > MAX_NOMINAL || tried.iter().any(|&v| (u / v - 1.0).abs() < 0.03) {
                continue;
            }
            tried.push(u);
            // stuffing keeps a frame's runs to 5 bits, the bus idle 11 or more: runs between in more than a few
            let forbidden = inner.iter().filter(|&&x| x > 5.5 * u && x < 10.5 * u).count() as f64;
            if !inner.is_empty() && forbidden / inner.len() as f64 > FORBIDDEN_RUNS {
                continue;
            }
            let (frames, errors) = read(ch, view.n_samples, u, idle, false, GIVE_UP);
            if frames.is_empty() {
                continue;
            }
            // the evidence: each distinct frame's CRC once (a frame sent again tells nothing new)
            let mut seen = std::collections::BTreeSet::new();
            for fr in &frames {
                seen.insert((fr["id"].to_string(), fr["data"].to_string()));
            }
            let info = 15.0 * seen.len() as f64;
            let datas: Vec<String> = frames.iter().map(|fr| fr["data"].to_string()).collect();
            let degenerate = {
                let mut c = std::collections::HashMap::new();
                for v in &datas {
                    *c.entry(v.clone()).or_insert(0usize) += 1;
                }
                let top = c.values().max().copied().unwrap_or(0) as f64 / datas.len().max(1) as f64;
                if datas.len() < 4 || top <= 0.5 {
                    1.0
                } else {
                    (1.0 - (top - 0.5) / 0.5).max(0.05)
                }
            };
            let score = frames.len() as f64 / (frames.len() + errors) as f64
                * (1.0 - 0.5f64.powf(info / EVIDENCE_BITS))
                * (0.5 + 0.5 * degenerate);
            if best.as_ref().is_some_and(|b| b.score >= score) {
                continue;
            }
            let mut params = Map::new();
            params.insert("bitrate".into(), json!(view.rate / u));
            params.insert("idle".into(), json!(idle));
            let mut evidence = Map::new();
            evidence.insert("frames".into(), json!(frames.len()));
            evidence.insert("errors".into(), json!(errors));
            evidence.insert("distinct".into(), json!(seen.len()));
            best = Some(Candidate {
                protocol: "can".into(),
                roles: [("data".to_string(), ch.name.to_string())].into(),
                score,
                alt: None,
                params,
                evidence,
                layers: crate::claim::raw(serde_json::to_string(&json!({ "frames": frames })).unwrap()),
            });
        }
        out.extend(best);
        if let Some(u0) = f.units.first().map(|x| x.samples).filter(|&x| x >= 1.0) {
            out.extend(fd_reading(view, ch, u0, f.idle, &inner));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A classic frame's bits as sent: stuffed from the start of frame through the CRC, then the fixed tail.
    fn frame_bits(id: u32, data: &[u8], ack: bool) -> Vec<u8> {
        let mut b: Vec<u8> = vec![0];
        b.extend((0..11).rev().map(|k| ((id >> k) & 1) as u8));
        b.extend([0, 0, 0]); // RTR, IDE, r0
        b.extend((0..4).rev().map(|k| ((data.len() >> k) & 1) as u8));
        for &x in data {
            b.extend((0..8).rev().map(|k| (x >> k) & 1));
        }
        let crc = crc15(&b);
        b.extend((0..15).rev().map(|k| ((crc >> k) & 1) as u8));
        let mut s = vec![];
        let (mut v, mut c) = (2u8, 0);
        for &x in &b {
            s.push(x);
            c = if x == v { c + 1 } else { 1 };
            v = x;
            if c == 5 {
                s.push(1 - x);
                v = 1 - x;
                c = 1;
            }
        }
        s.push(1);
        s.push(if ack { 0 } else { 1 });
        s.extend([1; 7]);
        s.extend([1; 3]);
        s
    }

    #[test]
    fn reads_frames() {
        let msgs: [(u32, &[u8]); 4] = [
            (0x123, &[0x11, 0x22, 0x33]),
            (0x7FF, &[]),
            (0x055, &[0, 0, 0, 0, 0xFF]),
            (0x321, &[1]),
        ];
        let (mut e, mut lv, mut t) = (vec![], 1u8, 100i64);
        let bit = 8i64; // 250 kbit/s at 2 MS/s
        for _ in 0..3 {
            for (id, d) in msgs {
                for x in frame_bits(id, d, true) {
                    if x != lv {
                        e.push(t);
                        lv = x;
                    }
                    t += bit;
                }
                t += 20 * bit;
            }
        }
        let v = View::new(2e6, t + 100, vec![Channel::new("CAN", 1, &e)]).unwrap();
        let c = candidates(&v).into_iter().next().expect("a CAN reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["frames"].as_array().unwrap().len(), 12, "{:?}", c.evidence);
        assert_eq!(lay["frames"][0]["id"], json!(0x123));
        assert_eq!(lay["frames"][2]["data"], json!("00000000ff"));
        assert!((c.params["bitrate"].as_f64().unwrap() / 250e3 - 1.0).abs() < 0.01);
        assert!(c.score > 0.9, "{:?}", c.evidence);
    }

    /// An FD frame (base format, BRS) as (bit, ticks) pieces: nominal bits of `u` to BRS's sample point, the data
    /// phase's of `ud` to the CRC delimiter's; dynamic stuffing (a stuff bit before the bit after five equal ones,
    /// none at the data's end), the stuff count and the CRC with their fixed stuff bits.
    fn fd_frame(id: u32, data: &[u8], u: i64, ud: i64) -> Vec<(u8, i64)> {
        let dlc = FD_LENGTHS.iter().position(|&n| n == data.len()).unwrap();
        let mut b: Vec<u8> = vec![0];
        b.extend((0..11).rev().map(|k| ((id >> k) & 1) as u8));
        b.extend([0, 0, 1, 0, 1, 0]); // RRS, IDE, FDF, res, BRS, ESI
        b.extend((0..4).rev().map(|k| ((dlc >> k) & 1) as u8));
        for &x in data {
            b.extend((0..8).rev().map(|k| (x >> k) & 1));
        }
        let mut sent = vec![];
        let (mut v, mut c, mut stuff) = (2u8, 0, 0usize);
        let mut brs_at = 0;
        for (i, &x) in b.iter().enumerate() {
            if c == 5 {
                sent.push(1 - v);
                v = 1 - v;
                c = 1;
                stuff += 1;
            }
            c = if x == v { c + 1 } else { 1 };
            v = x;
            sent.push(x);
            if i == 16 {
                brs_at = sent.len() - 1; // BRS
            }
        }
        let g = GRAY[stuff % 8];
        let sc: Vec<u8> = vec![
            (g >> 2 & 1) as u8,
            (g >> 1 & 1) as u8,
            (g & 1) as u8,
            (g.count_ones() & 1) as u8,
        ];
        let mut covered = sent.clone();
        covered.extend(&sc);
        let (width, poly) = if data.len() <= 16 {
            (17, CRC17_POLY)
        } else {
            (21, CRC21_POLY)
        };
        let crc = crc_fd(&covered, width, poly);
        let mut tail = vec![1 - *sent.last().unwrap()];
        tail.extend(&sc);
        for k in 0..width {
            if k % 4 == 0 {
                let last = *tail.last().unwrap();
                tail.push(1 - last);
            }
            tail.push(((crc >> (width - 1 - k)) & 1) as u8);
        }
        let mut out: Vec<(u8, i64)> = vec![];
        for (i, &x) in sent.iter().enumerate() {
            let w = if i < brs_at {
                u
            } else if i == brs_at {
                u * 3 / 4 + ud / 4
            } else {
                ud
            };
            out.push((x, w));
        }
        out.extend(tail.iter().map(|&x| (x, ud)));
        out.push((1, ud * 3 / 4 + u / 4)); // CRC delimiter
        out.push((0, u)); // ACK
        out.extend([(1, u); 11]); // ACK delimiter, EOF, intermission
        out
    }

    #[test]
    fn reads_fd_frames() {
        let (u, ud) = (80i64, 20i64); // 500 kbit/s nominal, 2 Mbit/s data at 40 MS/s
        let msgs: [(u32, &[u8]); 4] = [
            (
                0x123,
                &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc],
            ),
            (0x7F0, &[0xff; 20]),
            (0x055, &[0, 1, 2]),
            (0x321, &[0x5a; 8]),
        ];
        let (mut e, mut lv, mut t) = (vec![], 1u8, 100i64);
        for _ in 0..2 {
            for (id, d) in msgs {
                for (x, w) in fd_frame(id, d, u, ud) {
                    if x != lv {
                        e.push(t);
                        lv = x;
                    }
                    t += w;
                }
                t += 20 * u;
            }
        }
        let v = View::new(40e6, t + 100, vec![Channel::new("CAN", 1, &e)]).unwrap();
        let c = candidates(&v)
            .into_iter()
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
            .expect("a CAN FD reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["frames"].as_array().unwrap().len(), 8, "{:?} {lay}", c.evidence);
        assert_eq!(lay["frames"][1]["data"], json!("ff".repeat(20)));
        assert_eq!(lay["frames"][0]["brs"], json!(true));
        assert!(
            (c.params["bitrate"].as_f64().unwrap() / 500e3 - 1.0).abs() < 0.01,
            "{:?}",
            c.params
        );
        assert_eq!(c.params["data_bitrate"].as_f64().map(|x| (x / 1e5).round()), Some(20.0));
        assert!(c.score > 0.9, "{:?}", c.evidence);
    }
}
