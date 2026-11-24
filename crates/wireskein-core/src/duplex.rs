//! Two UART lines as a pair (Python's `_engine/duplex.py`): same bit time and character length, one side's messages
//! coming right after the other's, little overlap in time; and SCPI on the pair (a controller's commands, the
//! instrument's answers to its queries).

use serde_json::{Value, json};

/// A UART claim's bytes as the pair reads them.
pub struct Line {
    pub pin: String,
    pub baud: f64,
    pub l: Option<f64>,
    pub values: Vec<u8>,
    pub starts: Vec<i64>,
    pub char_samples: f64,
}

#[derive(Clone)]
pub struct Message {
    pub pin: String,
    pub t0: i64,
    pub t1: i64,
    pub data: Vec<u8>,
}

/// A line's messages: its characters up to a pause of more than 3 characters.
pub fn messages(ln: &Line) -> Vec<Message> {
    let mut out: Vec<Message> = vec![];
    let cs = ln.char_samples;
    for (i, (&v, &t)) in ln.values.iter().zip(&ln.starts).enumerate() {
        if i == 0 || (t - ln.starts[i - 1]) as f64 > 4.0 * cs {
            out.push(Message {
                pin: ln.pin.clone(),
                t0: t,
                t1: t,
                data: vec![],
            });
        }
        let m = out.last_mut().unwrap();
        m.data.push(v);
        m.t1 = (t as f64 + cs) as i64;
    }
    out
}

/// The pair's messages in time and its score, or None where the two lines are no pair.
pub fn pair(a: &Line, b: &Line) -> Option<(Vec<Message>, f64)> {
    if (a.baud / b.baud - 1.0).abs() > 0.03 || a.l != b.l {
        return None;
    }
    let (ma, mb) = (messages(a), messages(b));
    if ma.is_empty() || mb.is_empty() || ma.len() + mb.len() < 3 {
        return None;
    }
    let mut msgs: Vec<Message> = ma.iter().chain(&mb).cloned().collect();
    msgs.sort_by_key(|m| m.t0);
    let busy: i64 = msgs.iter().map(|m| m.t1 - m.t0).sum();
    let (mut overlap, mut i, mut j) = (0i64, 0, 0);
    while i < ma.len() && j < mb.len() {
        overlap += (ma[i].t1.min(mb[j].t1) - ma[i].t0.max(mb[j].t0)).max(0);
        if ma[i].t1 < mb[j].t1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    let ov = overlap as f64 / busy.max(1) as f64;
    // every message of the responding side right after one of the other's
    let follows = |pin: &str| -> (f64, usize) {
        let idx: Vec<usize> = (0..msgs.len()).filter(|&i| msgs[i].pin == pin).collect();
        let ok = idx.iter().filter(|&&i| i > 0 && msgs[i - 1].pin != pin).count();
        (ok as f64 / idx.len().max(1) as f64, idx.len())
    };
    let (fa, fb) = (follows(&a.pin), follows(&b.pin));
    let (f, n) = if fa.0 > fb.0 || (fa.0 == fb.0 && fa.1 >= fb.1) {
        fa
    } else {
        fb
    };
    let score = f * (1.0 - (2.0 * ov).min(1.0)) * (1.0 - 0.5f64.powi(n as i32));
    Some((msgs, score))
}

fn text(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// A command line of SCPI's grammar: `*XXX[?]` or `[:]Word[:Word...][?]`, then arguments after whitespace.
fn scpi_line(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    let tail_ok =
        |k: usize| -> bool { k == c.len() || (c[k].is_whitespace() && c[k..].iter().any(|ch| !ch.is_whitespace())) };
    // *IDN?
    if c.len() >= 4 && c[0] == '*' && c[1..4].iter().all(|x| x.is_ascii_alphabetic()) {
        let k = if c.get(4) == Some(&'?') { 5 } else { 4 };
        if tail_ok(k) {
            return true;
        }
    }
    let mut k = (c.first() == Some(&':')) as usize;
    let s0 = k;
    while k < c.len() && c[k].is_ascii_alphabetic() {
        k += 1;
    }
    if k - s0 < 3 {
        return false;
    }
    while k + 1 < c.len() && c[k] == ':' && c[k + 1].is_ascii_alphanumeric() {
        k += 1;
        while k < c.len() && c[k].is_ascii_alphanumeric() {
            k += 1;
        }
    }
    if c.get(k) == Some(&'?') {
        k += 1;
    }
    tail_ok(k)
}

/// SCPI on the pair with `host` as the controller: (score, exchanges [[command, answer or null]]), queries > 0.
fn scpi(msgs: &[Message], host: &str) -> Option<(f64, Value)> {
    let (mut lines, mut answered, mut queries, mut grammar, mut total) = (vec![], 0, 0, 0, 0);
    for (i, m) in msgs.iter().enumerate() {
        if m.pin != host {
            continue;
        }
        for cmd in text(&m.data).split('\n') {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                continue;
            }
            total += 1;
            grammar += scpi_line(cmd) as usize;
            let mut resp = Value::Null;
            if cmd.ends_with('?') {
                queries += 1;
                if let Some(n) = msgs.get(i + 1)
                    && n.pin != host
                {
                    answered += 1;
                    resp = json!(text(&n.data).trim());
                }
            }
            lines.push(json!([cmd, resp]));
        }
    }
    if queries == 0 {
        return None;
    }
    // an instrument's messages that do not follow a query are stray
    let dev: Vec<usize> = (0..msgs.len()).filter(|&i| msgs[i].pin != host).collect();
    let stray = dev
        .iter()
        .filter(|&&i| i == 0 || msgs[i - 1].pin != host || !text(&msgs[i - 1].data).trim().ends_with('?'))
        .count();
    let g = grammar as f64 / total.max(1) as f64;
    let ans = answered as f64 / queries as f64;
    let st = stray as f64 / dev.len().max(1) as f64;
    Some((g * ans * (1.0 - st) * (1.0 - 0.5f64.powi(queries)), json!(lines)))
}

/// SCPI on the pair, the better of the two lines as the controller: (score, host, exchanges).
pub fn scpi_of(msgs: &[Message], a: &str, b: &str) -> Option<(f64, String, Value)> {
    let mut best: Option<(f64, String, Value)> = None;
    for host in [a, b] {
        if let Some((s, ex)) = scpi(msgs, host)
            && best.as_ref().is_none_or(|x| s > x.0)
        {
            best = Some((s, host.to_string(), ex));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pin: &str, msgs: &[(i64, &str)]) -> Line {
        let (mut values, mut starts) = (vec![], vec![]);
        for &(t, s) in msgs {
            for (k, b) in s.bytes().enumerate() {
                values.push(b);
                starts.push(t + 100 * k as i64);
            }
        }
        Line {
            pin: pin.into(),
            baud: 9600.0,
            l: Some(10.0),
            values,
            starts,
            char_samples: 100.0,
        }
    }

    #[test]
    fn queries_and_answers() {
        assert!(scpi_line("*IDN?") && scpi_line(":MEAS:VOLT:DC?") && scpi_line("VOLT 5.0"));
        assert!(!scpi_line("hello1 world") && !scpi_line("AB") && !scpi_line("4.99"));
        let host = line("TX", &[(0, "*IDN?\n"), (10_000, "MEAS:VOLT?\n"), (20_000, "VOLT 5\n")]);
        let dev = line("RX", &[(2_000, "ACME,1\n"), (12_000, "4.99\n")]);
        let (msgs, score) = pair(&host, &dev).unwrap();
        assert!(score > 0.6, "{score}");
        let (s, h, ex) = scpi_of(&msgs, "TX", "RX").unwrap();
        assert_eq!(h, "TX");
        assert!(s > 0.7, "{s}");
        assert_eq!(ex[0], json!(["*IDN?", "ACME,1"]));
        assert_eq!(ex[2], json!(["VOLT 5", null]));
    }
}
