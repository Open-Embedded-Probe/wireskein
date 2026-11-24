//! Upper layers on a UART's bytes (Python's `_engine/analyzers/upper.py`, scored by `_engine/scoring.py`'s
//! DefaultScorer): printable text split into lines (and the marker lines of the I2CDeviceDB experiments on them),
//! NMEA sentences with their checksum, Modbus RTU messages split on silences with their CRC-16. Each a decoder and
//! evidence for the bytes below: a passing checksum on random bytes is very unlikely, plain text weaker evidence.

use serde_json::{Value, json};

const UPPER_WEIGHT: f64 = 0.5;
pub const SHOWN: f64 = 0.3; // an upper reading at this score or more is shown

/// A child reading: its score with its own children's support, and what it shows.
pub struct Child {
    pub name: &'static str,
    pub total: f64,
    pub items: Value,
}

fn combine(layer: f64, support: f64) -> f64 {
    layer + UPPER_WEIGHT * (1.0 - layer) * support * layer
}

fn checked(ok: usize, bad: usize) -> f64 {
    if ok == 0 {
        return 0.0;
    }
    (1.0 - 0.5f64.powi(ok as i32)) * ok as f64 / (ok + bad) as f64
}

fn printable(v: i64) -> bool {
    (0x20..0x7F).contains(&v) || v == 0x09 || v == 0x0A || v == 0x0D
}

/// The complete lines of the bytes (split at \n or \r\n), as Latin-1 text.
fn split_lines(b: &[u8]) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Vec<u8> = vec![];
    for &x in b {
        if x == b'\n' {
            if cur.last() == Some(&b'\r') {
                cur.pop();
            }
            out.push(cur.iter().map(|&c| c as char).collect());
            cur.clear();
        } else {
            cur.push(x);
        }
    }
    out
}

/// The marker lines' kinds (CASE_BEGIN, CASE_END, PHASE, INPUT, RESULT): their score as the marker grammar's.
fn markers(lines: &[String]) -> f64 {
    let mut kinds = vec![];
    for l in lines {
        let t = l.trim();
        for k in ["CASE_BEGIN", "CASE_END", "PHASE", "INPUT", "RESULT"] {
            // the word whole: followed by none of [A-Za-z0-9_]
            if t.starts_with(k)
                && !t[k.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                kinds.push(k);
                break;
            }
        }
    }
    if kinds.is_empty() {
        return 0.0;
    }
    let (mut depth, mut balanced) = (0i64, true);
    for k in &kinds {
        if *k == "CASE_BEGIN" {
            depth += 1;
        } else if *k == "CASE_END" {
            depth -= 1;
            balanced &= depth >= 0;
        }
    }
    let bal = (balanced && (depth == 0 || depth == 1) && kinds.contains(&"CASE_BEGIN")) as u8 as f64;
    let rate = if lines.is_empty() {
        0.0
    } else {
        kinds.len() as f64 / lines.len() as f64
    };
    rate * (0.5 + 0.5 * bal)
}

fn lines(v: &[i64]) -> Option<Child> {
    if v.is_empty() {
        return None;
    }
    let b: Vec<u8> = v.iter().map(|&x| (x & 0xFF) as u8).collect();
    let pr = v.iter().filter(|&&x| printable(x)).count() as f64 / v.len() as f64;
    let newlines = v.iter().filter(|&&x| x == 0x0A).count() as f64;
    // words of letters: real text has them, random printable bytes rarely do
    let (mut words, mut run) = (0usize, 0usize);
    for &c in b.iter().chain(std::iter::once(&0u8)) {
        if c.is_ascii_alphabetic() {
            run += 1;
        } else {
            if run >= 3 {
                words += run;
            }
            run = 0;
        }
    }
    let word_share = words as f64 / v.len() as f64;
    let layer = pr.powi(4) * (newlines / 2.0).min(1.0) * (0.5 + 0.5 * (3.0 * word_share).min(1.0));
    let ls = split_lines(&b);
    let mk = markers(&ls);
    let mk_total = combine(mk, 0.0);
    Some(Child {
        name: "lines",
        total: combine(layer, mk_total),
        items: json!(ls),
    })
}

fn nmea(v: &[i64]) -> Child {
    let b: Vec<u8> = v.iter().map(|&x| (x & 0xFF) as u8).collect();
    let (mut ok, mut bad, mut covered) = (0usize, 0usize, 0usize);
    let mut msgs = vec![];
    let hex = |c: u8| (c as char).to_digit(16);
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && j - i - 1 < 80 && !matches!(b[j], b'$' | b'*' | b'\r' | b'\n') {
            j += 1;
        }
        let body = &b[i + 1..j];
        if !body.is_empty()
            && j + 2 < b.len()
            && b[j] == b'*'
            && let (Some(h), Some(l)) = (hex(b[j + 1]), hex(b[j + 2]))
        {
            let cs = (h * 16 + l) as u8;
            if body.iter().fold(0u8, |a, &c| a ^ c) == cs {
                ok += 1;
                covered += j + 3 - i;
                msgs.push(body.iter().map(|&c| c as char).collect::<String>());
            } else {
                bad += 1;
            }
            i = j + 3;
        } else {
            i += 1;
        }
    }
    let _ = covered;
    Child {
        name: "nmea",
        total: combine(checked(ok, bad), 0.0),
        items: json!(msgs),
    }
}

/// CRC-16/Modbus of every prefix of the data.
fn crc16_running(data: &[u8]) -> Vec<u16> {
    let mut crc = 0xFFFFu16;
    let mut out = vec![crc];
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
        out.push(crc);
    }
    out
}

const T15: f64 = 1.5; // silences in characters: inside a message at most
const T35: f64 = 3.5; // between messages at least
const JUNK: usize = 2; // bytes after a message's CRC left as junk
const RESTARTS: usize = 2; // starts tried again after junk, in a chunk at most
const MAX: usize = 256; // bytes a message at most

fn modbus(v: &[i64], st: &[i64], cs: f64) -> Option<Child> {
    if v.len() < 4 {
        return None;
    }
    let vb: Vec<u8> = v.iter().map(|&x| (x & 0xFF) as u8).collect();
    let gaps: Vec<f64> = st.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let cut = |t: f64| -> Vec<usize> {
        (0..gaps.len())
            .filter(|&i| gaps[i] > (1.0 + t) * cs)
            .map(|i| i + 1)
            .collect()
    };
    let (chunk_cut, piece_cut) = (cut(T35), cut(T15));
    let (mut ok, mut bad) = (0usize, 0usize);
    let mut msgs = vec![];
    let mut bounds = vec![0usize];
    bounds.extend(&chunk_cut);
    bounds.push(v.len());
    for z in bounds.windows(2) {
        let (c0, c1) = (z[0], z[1]);
        let (mut pos, mut restarts) = (c0, 0);
        while c1 >= pos + 4 {
            let top = c1.min(pos + MAX + JUNK);
            let mut ends: Vec<usize> = piece_cut.iter().copied().filter(|&p| p > pos && p <= top).collect();
            if c1 <= top {
                ends.push(c1);
            }
            let Some(&last) = ends.last() else { break };
            let crcs = crc16_running(&vb[pos..last]);
            let mut found = None;
            'e: for &e in &ends {
                for junk in 0..=JUNK {
                    if e < junk + pos {
                        continue;
                    }
                    let m = e - junk - pos;
                    if m >= 4 && crcs[m - 2] == vb[pos + m - 2] as u16 | (vb[pos + m - 1] as u16) << 8 {
                        found = Some((e, m));
                        break 'e;
                    }
                }
            }
            let Some((e, m)) = found else {
                let nxt: Vec<usize> = ends.iter().copied().filter(|&e| e > pos && e < c1).collect();
                if !nxt.is_empty() && restarts < RESTARTS {
                    pos = nxt[0];
                    restarts += 1;
                    continue;
                }
                bad += 1;
                break;
            };
            ok += 1;
            msgs.push(crate::pulse::hex(&vb[pos..pos + m]));
            pos = e;
        }
    }
    Some(Child {
        name: "modbus_rtu",
        total: combine(checked(ok, bad), 0.0),
        items: json!(msgs),
    })
}

/// The upper readings of a UART's bytes (values, their start ticks, a character's ticks).
pub fn children(v: &[i64], st: &[i64], char_samples: f64) -> Vec<Child> {
    let mut out = vec![];
    out.extend(lines(v));
    out.push(nmea(v));
    out.extend(modbus(v, st, char_samples));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_nmea_and_modbus() {
        let s = b"$GPGGA,1*55\r\nhello world\r\n";
        let v: Vec<i64> = s.iter().map(|&c| c as i64).collect();
        let st: Vec<i64> = (0..v.len() as i64).map(|i| i * 10).collect();
        let ch = children(&v, &st, 10.0);
        let l = ch.iter().find(|c| c.name == "lines").unwrap();
        assert_eq!(l.items, json!(["$GPGGA,1*55", "hello world"]));
        let n = ch.iter().find(|c| c.name == "nmea").unwrap();
        // GPGGA,1 xor: the test's own sum
        let x = b"GPGGA,1".iter().fold(0u8, |a, &c| a ^ c);
        assert_eq!(n.items.as_array().unwrap().len(), (x == 0x55) as usize);
        // Modbus: a request and its answer, each with its CRC, 5 characters apart
        let mut msg = vec![0x01u8, 0x03, 0x00, 0x00, 0x00, 0x02];
        let c = crc16_running(&msg)[msg.len()];
        msg.extend([(c & 0xFF) as u8, (c >> 8) as u8]);
        let mut v = vec![];
        let mut st = vec![];
        for k in 0..3 {
            for (i, &b) in msg.iter().enumerate() {
                v.push(b as i64);
                st.push(k * 1000 + i as i64 * 10);
            }
        }
        let m = modbus(&v, &st, 10.0).unwrap();
        assert_eq!(m.items.as_array().unwrap().len(), 3);
        assert!(m.total > 0.8);
    }
}
