//! USB low speed (1.5 Mb/s) and full speed (12 Mb/s) on D+ and D- (Python's `_engine/usb.py`): the pair's states J,
//! K and SE0, packets from the first K after idle to an end of packet (SE0), NRZI (a change is 0), a 0 stuffed after
//! six 1s, SYNC, the PID with its complement, and the packet's fields: a token's address, endpoint and CRC5, a start
//! of frame's number, data with its CRC16, a handshake. Full speed idles with D+ high, low speed with D- high.

use crate::Candidate;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const FS: f64 = 12e6;
pub const LS: f64 = 1.5e6;
pub const MIN_SAMPLES: f64 = 2.0;
pub const SKEW: f64 = 0.4;
pub const COUNTS: f64 = 0.25;
pub const OPPOSITE: f64 = 0.6;
pub const SKEW_S: f64 = 150e-9;
pub const TOGETHER: f64 = 0.8;
pub const PACKETS: f64 = 4.0;
pub const EVIDENCE_BITS: f64 = 8.0;
pub const LINE_DEGLITCH_OF_MEDIAN: f64 = 0.25;
pub const LINE_DEGLITCH_MAX: i64 = 64;

fn pid_name(p: u32) -> Option<&'static str> {
    Some(match p {
        0x1 => "OUT",
        0x9 => "IN",
        0x5 => "SOF",
        0xD => "SETUP",
        0x3 => "DATA0",
        0xB => "DATA1",
        0x7 => "DATA2",
        0xF => "MDATA",
        0x2 => "ACK",
        0xA => "NAK",
        0xE => "STALL",
        0x6 => "NYET",
        0xC => "PRE",
        0x8 => "SPLIT",
        0x4 => "PING",
        _ => return None,
    })
}

const TOKENS: [&str; 4] = ["OUT", "IN", "SETUP", "PING"];
const DATA: [&str; 4] = ["DATA0", "DATA1", "DATA2", "MDATA"];
const HANDSHAKES: [&str; 4] = ["ACK", "NAK", "STALL", "NYET"];

/// CRC5 of the bits as sent (LSB first): the value to send, LSB first as a number.
pub fn crc5(bits: &[u8]) -> u32 {
    let mut reg = 0x1Fu32;
    for &b in bits {
        let fb = (reg >> 4) ^ b as u32;
        reg = ((reg << 1) & 0x1F) ^ if fb == 1 { 0x05 } else { 0 };
    }
    reg ^= 0x1F;
    (0..5).fold(0, |a, i| a | (((reg >> (4 - i)) & 1) << i))
}

pub fn crc16(data: &[u8]) -> u32 {
    let mut crc = 0xFFFFu32;
    for &x in data {
        crc ^= x as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc ^ 0xFFFF
}

fn value(bits: &[u8]) -> u32 {
    bits.iter().enumerate().fold(0, |a, (i, &b)| a | ((b as u32) << i))
}

/// The pair's states (ticks where they change, the state from each: 1 D+ high only, 2 D- high only, 0 SE0, 3 both).
fn states(dp: &Channel, dm: &Channel) -> (Vec<i64>, Vec<u8>) {
    let (mut t, mut st) = (vec![], vec![]);
    let (a, b) = (dp.edges, dm.edges);
    let (mut i, mut j) = (0usize, 0usize);
    let (mut la, mut lb) = (dp.initial, dm.initial);
    let code = |x: u8, y: u8| match (x, y) {
        (1, 0) => 1u8,
        (0, 1) => 2,
        (0, 0) => 0,
        _ => 3,
    };
    // (changes at tick 0 are the level from the start)
    while i < a.len() && a[i] <= 0 {
        la ^= 1;
        i += 1;
    }
    while j < b.len() && b[j] <= 0 {
        lb ^= 1;
        j += 1;
    }
    t.push(0);
    st.push(code(la, lb));
    while i < a.len() || j < b.len() {
        let x = a
            .get(i)
            .copied()
            .unwrap_or(i64::MAX)
            .min(b.get(j).copied().unwrap_or(i64::MAX));
        while i < a.len() && a[i] == x {
            la ^= 1;
            i += 1;
        }
        while j < b.len() && b[j] == x {
            lb ^= 1;
            j += 1;
        }
        let s = code(la, lb);
        if s != *st.last().unwrap() {
            t.push(x);
            st.push(s);
        }
    }
    (t, st)
}

/// The states with a change's skew left out: an SE0 or both-high shorter than SKEW of a bit joins the state after it.
fn deskew(t: &[i64], st: &[u8], n: i64, u: f64) -> (Vec<i64>, Vec<u8>) {
    let (mut t2, mut s2): (Vec<i64>, Vec<u8>) = (vec![], vec![]);
    for k in 0..t.len() {
        let end = if k + 1 < t.len() { t[k + 1] } else { n };
        if (st[k] == 0 || st[k] == 3) && ((end - t[k]) as f64) < SKEW * u {
            continue;
        }
        if s2.last() != Some(&st[k]) {
            t2.push(t[k]);
            s2.push(st[k]);
        }
    }
    (t2, s2)
}

/// NRZI bits of a packet's runs of J / K, unstuffed; None on a stuffing error.
fn nrzi(dur: &[f64], u: f64) -> Option<Vec<u8>> {
    let mut out = vec![];
    let mut ones = 0;
    for &d in dur {
        let k = ((d / u).round_ties_even() as i64).max(1);
        for b in std::iter::once(0u8).chain(std::iter::repeat_n(1u8, (k - 1) as usize)) {
            if ones == 6 {
                if b != 0 {
                    return None;
                }
                ones = 0;
                continue;
            }
            out.push(b);
            ones = if b == 1 { ones + 1 } else { 0 };
        }
    }
    Some(out)
}

/// The packets with the states read as given at a speed (J: state 1 full speed, 2 low speed).
fn packets(t: &[i64], st: &[u8], n: i64, rate: f64, speed: f64) -> Vec<Map<String, Value>> {
    let u = rate / speed;
    let (t, st) = deskew(t, st, n, u);
    let idle = if speed == FS { 1 } else { 2 };
    let m = st.len();
    let mut out = vec![];
    let mut i = 0usize;
    while i + 1 < m {
        if !(st[i] == idle && st[i + 1] == 3 - idle) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut dur = vec![];
        while j < m && (st[j] == 1 || st[j] == 2) {
            let end = if j + 1 < m { t[j + 1] } else { n };
            dur.push((end - t[j]) as f64);
            j += 1;
        }
        let s0 = t[i + 1];
        if j >= m || st[j] != 0 {
            i = j;
            continue;
        }
        let e = t[j];
        let se0 = (if j + 1 < m { t[j + 1] } else { n } - t[j]) as f64;
        i = j;
        if se0 > 4.0 * u * if speed == FS { 8.0 } else { 1.0 } || dur.len() < 2 {
            continue;
        }
        let mut row = Map::new();
        row.insert("s".into(), json!(s0));
        row.insert("e".into(), json!(e));
        let fail = |mut row: Map<String, Value>, why: &str| {
            row.insert("ok".into(), json!(false));
            row.insert("error".into(), json!(why));
            row
        };
        let bits = match nrzi(&dur, u) {
            Some(b) if b.len() >= 16 => b,
            _ => {
                out.push(fail(row, "stuff"));
                continue;
            }
        };
        if bits[..8] != [0, 0, 0, 0, 0, 0, 0, 1] {
            out.push(fail(row, "sync"));
            continue;
        }
        let pid = value(&bits[8..12]);
        let name = match pid_name(pid) {
            Some(x) if value(&bits[12..16]) == pid ^ 0xF => x,
            _ => {
                out.push(fail(row, "pid"));
                continue;
            }
        };
        row.insert("pid".into(), json!(name));
        let rest = &bits[16..];
        if TOKENS.contains(&name) || name == "SOF" {
            if rest.len() < 16 {
                out.push(fail(row, "short"));
                continue;
            }
            let ok = crc5(&rest[..11]) == value(&rest[11..16]);
            if name == "SOF" {
                row.insert("frame".into(), json!(value(&rest[..11])));
            } else {
                row.insert("addr".into(), json!(value(&rest[..7])));
                row.insert("endp".into(), json!(value(&rest[7..11])));
            }
            row.insert("ok".into(), json!(ok));
        } else if DATA.contains(&name) {
            let nb = rest.len() / 8;
            if nb < 2 {
                out.push(fail(row, "short"));
                continue;
            }
            let data: Vec<u8> = (0..nb).map(|k| value(&rest[8 * k..8 * k + 8]) as u8).collect();
            let body = &data[..nb - 2];
            let ok = crc16(body) == (data[nb - 2] as u32 | (data[nb - 1] as u32) << 8);
            row.insert(
                "data".into(),
                json!(body.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            );
            row.insert("ok".into(), json!(ok));
        } else {
            row.insert("ok".into(), json!(true));
        }
        out.push(row);
    }
    out
}

fn setup_request(d: &[u8]) -> Value {
    let (rt, req) = (d[0], d[1]);
    let w = |k: usize| d[k] as u32 | (d[k + 1] as u32) << 8;
    let ty = ["standard", "class", "vendor", "reserved"][((rt >> 5) & 3) as usize];
    let mut m = Map::new();
    m.insert("bmRequestType".into(), json!(rt));
    m.insert("bRequest".into(), json!(req));
    m.insert("wValue".into(), json!(w(2)));
    m.insert("wIndex".into(), json!(w(4)));
    m.insert("wLength".into(), json!(w(6)));
    m.insert("direction".into(), json!(if rt & 0x80 != 0 { "in" } else { "out" }));
    m.insert("type".into(), json!(ty));
    let names = [
        (0, "GET_STATUS"),
        (1, "CLEAR_FEATURE"),
        (3, "SET_FEATURE"),
        (5, "SET_ADDRESS"),
        (6, "GET_DESCRIPTOR"),
        (7, "SET_DESCRIPTOR"),
        (8, "GET_CONFIGURATION"),
        (9, "SET_CONFIGURATION"),
        (10, "GET_INTERFACE"),
        (11, "SET_INTERFACE"),
        (12, "SYNCH_FRAME"),
    ];
    if ty == "standard"
        && let Some((_, nm)) = names.iter().find(|(k, _)| *k == req)
    {
        m.insert("request".into(), json!(nm));
    }
    Value::Object(m)
}

/// Packets grouped as the host's transactions: a token, its data packet if any, the handshake ending it; a SOF alone.
fn transactions(pk: &[Map<String, Value>]) -> Vec<Value> {
    let mut out = vec![];
    let mut cur: Option<Map<String, Value>> = None;
    let close = |cur: &mut Option<Map<String, Value>>, out: &mut Vec<Value>| {
        if let Some(c) = cur.take() {
            out.push(Value::Object(c));
        }
    };
    for p in pk {
        let pid = p.get("pid").and_then(|x| x.as_str()).unwrap_or("");
        if p.get("ok") != Some(&json!(true)) {
            close(&mut cur, &mut out);
            continue;
        }
        if pid == "SOF" {
            close(&mut cur, &mut out);
            out.push(json!({"s": p["s"], "e": p["e"], "token": "SOF", "frame": p["frame"]}));
        } else if TOKENS.contains(&pid) {
            close(&mut cur, &mut out);
            let mut m = Map::new();
            for (k, v) in [
                ("s", &p["s"]),
                ("e", &p["e"]),
                ("addr", &p["addr"]),
                ("endp", &p["endp"]),
            ] {
                m.insert(k.into(), v.clone());
            }
            m.insert("token".into(), json!(pid));
            cur = Some(m);
        } else if DATA.contains(&pid) && cur.as_ref().is_some_and(|c| !c.contains_key("data")) {
            let c = cur.as_mut().unwrap();
            c.insert("e".into(), p["e"].clone());
            c.insert("data_pid".into(), json!(pid));
            c.insert("data".into(), p["data"].clone());
            let hex = p["data"].as_str().unwrap_or("");
            if c["token"] == "SETUP" && hex.len() == 16 {
                let d: Vec<u8> = (0..8)
                    .map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).unwrap())
                    .collect();
                c.insert("setup".into(), setup_request(&d));
            }
        } else if HANDSHAKES.contains(&pid) && cur.is_some() {
            let c = cur.as_mut().unwrap();
            c.insert("e".into(), p["e"].clone());
            c.insert("handshake".into(), json!(pid));
            close(&mut cur, &mut out);
        } else {
            close(&mut cur, &mut out);
        }
    }
    close(&mut cur, &mut out);
    out
}

/// Two lines of a differential pair: opposite most of the time, changing together (skewed by a sample or two).
pub fn usb_pair(a: &Channel, b: &Channel, n: i64, rate: f64) -> bool {
    let (ea, eb) = (a.edges, b.edges);
    if ea.len() < 16 || eb.len() < 16 {
        return false;
    }
    let tick = (2.0f64).max(SKEW_S * rate);
    let (few, many) = if ea.len() <= eb.len() { (ea, eb) } else { (eb, ea) };
    if (few.len() as f64) < COUNTS * many.len() as f64 {
        return false;
    }
    let near = few
        .iter()
        .filter(|&&x| {
            let i = many.partition_point(|&y| y < x).clamp(1, many.len() - 1);
            ((x - many[i - 1]).abs().min((many[i] - x).abs()) as f64) <= tick
        })
        .count() as f64
        / few.len() as f64;
    if near < TOGETHER {
        return false;
    }
    let m = 4096;
    let last = (n - 1).max(0) as f64;
    let xs = (0..m).map(|k| (last * k as f64 / (m - 1) as f64) as i64);
    let la = crate::kernels::levels_at_sorted(a, xs.clone());
    let lb = crate::kernels::levels_at_sorted(b, xs);
    la.iter().zip(&lb).filter(|(x, y)| x != y).count() as f64 / m as f64 >= OPPOSITE
}

fn read(view: &View, a: &Channel, b: &Channel, k: i64) -> Option<Candidate> {
    let (n, rate) = (view.n_samples, view.rate);
    if !usb_pair(a, b, n, rate) {
        return None;
    }
    let (t, st) = states(a, b);
    let swapped: Vec<u8> = st
        .iter()
        .map(|&s| match s {
            1 => 2,
            2 => 1,
            x => x,
        })
        .collect();
    type Reading<'a> = (usize, Vec<Map<String, Value>>, f64, &'a str, &'a str);
    let mut best: Option<Reading> = None;
    for speed in [FS, LS] {
        if rate / speed < MIN_SAMPLES {
            continue;
        }
        for (dp, dm, s) in [(a, b, &st), (b, a, &swapped)] {
            let pk = packets(&t, s, n, rate, speed);
            let good = pk.iter().filter(|p| p.get("ok") == Some(&json!(true))).count();
            if best.as_ref().is_none_or(|x| good > x.0) {
                best = Some((good, pk, speed, dp.name, dm.name));
            }
        }
    }
    let (good, pk, speed, dp, dm) = best?;
    if good == 0 {
        return None;
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut info = 0.0;
    for p in pk.iter().filter(|p| p.get("ok") == Some(&json!(true))) {
        let key: Vec<String> = ["pid", "addr", "endp", "frame", "data"]
            .iter()
            .map(|f| p.get(*f).map(|v| v.to_string()).unwrap_or_default())
            .collect();
        if !seen.insert(key) {
            continue;
        }
        info += 4.0;
        if p.contains_key("frame") || p.contains_key("addr") {
            info += 5.0;
        }
        if p.contains_key("data") {
            info += 16.0;
        }
    }
    let share = good as f64 / pk.len().max(1) as f64;
    let score = (1.0 - 0.5f64.powf(info / EVIDENCE_BITS)) * share * (1.0 - 0.5f64.powf(good as f64 / PACKETS));
    let mut params = Map::new();
    params.insert("speed".into(), json!(if speed == FS { "full" } else { "low" }));
    params.insert("bitrate".into(), json!(speed));
    if k > 0 {
        params.insert("deglitch".into(), json!(k));
    }
    let mut evidence = Map::new();
    evidence.insert("packets".into(), json!(pk.len()));
    evidence.insert("ok".into(), json!(good));
    evidence.insert("evidence_bits".into(), json!(info));
    evidence.insert("share".into(), json!(share));
    let tr = transactions(&pk);
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "packets": pk, "transactions": tr })).unwrap());
    Some(Candidate {
        protocol: "usb".into(),
        roles: [("dp".to_string(), dp.to_string()), ("dm".to_string(), dm.to_string())].into(),
        score,
        alt: None,
        params,
        evidence,
        layers,
    })
}

/// A line's pulses far shorter than its shorter level's usual runs taken out (bounces, spikes), and the width.
fn settled(ch: &Channel, n: i64) -> Option<(Vec<i64>, i64)> {
    let (_, ln, lv) = crate::kernels::runs(ch, n);
    if ln.len() < 10 {
        return None;
    }
    let (ln, lv) = (&ln[1..ln.len() - 1], &lv[1..lv.len() - 1]);
    let med = (0..2u8)
        .filter_map(|l| {
            let v: Vec<f64> = ln
                .iter()
                .zip(lv)
                .filter(|&(_, &x)| x == l)
                .map(|(&r, _)| r as f64)
                .collect();
            (!v.is_empty()).then(|| crate::kernels::median(&v))
        })
        .fold(f64::INFINITY, f64::min);
    let k = ((LINE_DEGLITCH_OF_MEDIAN * med) as i64).clamp(2, LINE_DEGLITCH_MAX);
    Some((crate::kernels::shortest_first(ch.edges, k), k))
}

/// The USB readings of a part: each two lines of 16 changes or more as a pair, as they are and with their short
/// pulses taken out; the best reading of each pair.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let act: Vec<&Channel> = view.channels.iter().filter(|c| c.edges.len() >= 16).collect();
    let mut out = vec![];
    for (i, a) in act.iter().enumerate() {
        for b in &act[i + 1..] {
            let mut best = read(view, a, b, 0);
            if let (Some((ea, ka)), Some((eb, kb))) = (settled(a, view.n_samples), settled(b, view.n_samples))
                && (ea.len() != a.edges.len() || eb.len() != b.edges.len())
            {
                let (a2, b2) = (
                    Channel::new(a.name, a.initial, &ea),
                    Channel::new(b.name, b.initial, &eb),
                );
                if let Some(c) = read(view, &a2, &b2, ka.max(kb))
                    && best.as_ref().is_none_or(|x| c.score > x.score)
                {
                    best = Some(c);
                }
            }
            out.extend(best);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits as sent (LSB first per field) to NRZI levels of D+ at full speed, stuffed; SE0 for two bits after.
    fn encode(pk: &[Vec<u8>], u: i64) -> (Vec<i64>, Vec<i64>, i64) {
        let (mut dp, mut dm) = (vec![], vec![]);
        let mut t = 50i64;
        for p in pk {
            let mut bits = vec![0, 0, 0, 0, 0, 0, 0, 1];
            bits.extend(p);
            let mut stuffed = vec![];
            let mut ones = 0;
            for &b in &bits {
                stuffed.push(b);
                ones = if b == 1 { ones + 1 } else { 0 };
                if ones == 6 {
                    stuffed.push(0);
                    ones = 0;
                }
            }
            let mut lv = 1u8; // J: D+ high
            for &b in &stuffed {
                if b == 0 {
                    lv ^= 1;
                    dp.push(t);
                    dm.push(t);
                }
                t += u;
            }
            // SE0: both low for two bits, then J
            if lv == 1 {
                dp.push(t);
            } else {
                dm.push(t);
            }
            t += 2 * u;
            dp.push(t);
            t += 40 * u;
        }
        (dp, dm, t + 10)
    }

    fn field(v: u32, n: usize) -> Vec<u8> {
        (0..n).map(|i| ((v >> i) & 1) as u8).collect()
    }

    #[test]
    fn reads_packets() {
        let u = 8; // 96 MHz sampling at full speed
        let token = |pid: u32, addr: u32, ep: u32| {
            let mut b = field(pid | (pid ^ 0xF) << 4, 8);
            let mut f = field(addr, 7);
            f.extend(field(ep, 4));
            let c = crc5(&f);
            b.extend(f);
            b.extend(field(c, 5));
            b
        };
        let data = |pid: u32, d: &[u8]| {
            let mut b = field(pid | (pid ^ 0xF) << 4, 8);
            for &x in d {
                b.extend(field(x as u32, 8));
            }
            b.extend(field(crc16(d), 16));
            b
        };
        let ack = field(0x2 | 0xD << 4, 8);
        let pk = vec![
            token(0xD, 0, 0),
            data(0x3, &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00]),
            ack.clone(),
            token(0x9, 5, 1),
            data(0xB, &[0xff, 0xff, 0x12]),
            ack,
        ];
        let (dp, dm, n) = encode(&pk, u);
        let v = View::new(96e6, n, vec![Channel::new("DP", 1, &dp), Channel::new("DM", 0, &dm)]).unwrap();
        let c = candidates(&v).into_iter().next().expect("a USB reading");
        assert_eq!(c.roles["dp"], "DP");
        assert_eq!(c.params["speed"], json!("full"));
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["packets"][3]["addr"], json!(5), "{lay}");
        assert_eq!(lay["packets"][4]["data"], json!("ffff12"));
        assert_eq!(lay["transactions"][0]["setup"]["request"], json!("GET_DESCRIPTOR"));
        assert!(c.score > 0.6, "{:?}", c.evidence);
    }
}
