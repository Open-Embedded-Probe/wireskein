//! A Value Change Dump (Python's `_engine/vcdio.py`): its changes as logic lines. The tick: the `$comment wireskein
//! {...}` block's where WireSkein wrote it, else the greatest common divisor of the change times. 1-bit variables
//! are lines, a vector one line a bit (`NAME[k]`), x and z read as 0 (counted); `real` variables are left out
//! (named).

use crate::{Capture, Kind, Line, Part, fraction};
use serde_json::{Map, Value, json};

const MARK: &str = "wireskein ";

/// Femtoseconds a timescale unit.
fn unit_fs(u: &str) -> Option<i128> {
    Some(match u {
        "s" => 1_000_000_000_000_000,
        "ms" => 1_000_000_000_000,
        "us" => 1_000_000_000,
        "ns" => 1_000_000,
        "ps" => 1_000,
        "fs" => 1,
        _ => return None,
    })
}

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// The text between `$kw` and its `$end`, for each one in `head`.
fn blocks<'a>(head: &'a str, kw: &str) -> Vec<&'a str> {
    let mut out = vec![];
    let mut rest = head;
    let key = format!("${kw}");
    while let Some(i) = rest.find(&key) {
        let after = &rest[i + key.len()..];
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            rest = after;
            continue;
        }
        let Some(j) = after.find("$end") else { break };
        out.push(&after[..j]);
        rest = &after[j + 4..];
    }
    out
}

/// A VCD read.
pub fn read(text: &str) -> Result<Capture, String> {
    let Some(at) = text.find("$enddefinitions") else {
        return Err("no $enddefinitions: not a VCD".into());
    };
    let (head, body) = (&text[..at], &text[at + "$enddefinitions".len()..]);
    let mut extra = Value::Null;
    for c in blocks(head, "comment") {
        if let Some(j) = c.trim().strip_prefix(MARK) {
            extra = serde_json::from_str(j).unwrap_or(Value::Null);
        }
    }
    if extra["format"] != json!("wireskein-vcd-extra/1") {
        extra = Value::Null;
    }
    let mut unit: i128 = 1_000_000; // 1 ns
    if let Some(ts) = blocks(head, "timescale").first() {
        let t: String = ts.split_whitespace().collect();
        let k = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        let (n, u) = t.split_at(k);
        unit = n.parse::<i128>().map_err(|_| format!("timescale {ts:?}"))?
            * unit_fs(u).ok_or_else(|| format!("timescale {ts:?}"))?;
    }
    // the variables in order (the same id under another name: the first kept), scopes not named
    let mut vars: Vec<(String, String, usize, String)> = vec![]; // (id, type, width, name)
    let mut order_text: Vec<(usize, &str)> = vec![];
    for kw in ["var"] {
        let mut rest = head;
        let mut pos = 0;
        let key = format!("${kw}");
        while let Some(i) = rest.find(&key) {
            let after = &rest[i + key.len()..];
            let Some(j) = after.find("$end") else { break };
            order_text.push((pos + i, &after[..j]));
            pos += i + key.len() + j + 4;
            rest = &after[j + 4..];
        }
    }
    for (_, t) in order_text {
        let w: Vec<&str> = t.split_whitespace().collect();
        if w.len() < 4 {
            continue;
        }
        if vars.iter().any(|v| v.0 == w[2]) {
            continue;
        }
        vars.push((w[2].into(), w[0].into(), w[1].parse().unwrap_or(1), w[3].into()));
    }
    let index: std::collections::HashMap<&str, usize> =
        vars.iter().enumerate().map(|(i, v)| (v.0.as_str(), i)).collect();
    let mut changes: Vec<Vec<(i128, String)>> = vec![vec![]; vars.len()];
    let mut t: i128 = 0;
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut i = if words.first() == Some(&"$end") { 1 } else { 0 };
    while i < words.len() {
        let w = words[i];
        i += 1;
        let c = w.chars().next().unwrap();
        if c == '#' {
            t = w[1..].parse().unwrap_or(t);
        } else if "01xXzZ".contains(c) && w.len() > 1 {
            if let Some(&k) = index.get(&w[1..]) {
                changes[k].push((t, c.to_string()));
            }
        } else if "bBrR".contains(c) && i < words.len() {
            let vid = words[i];
            i += 1;
            if let Some(&k) = index.get(vid) {
                changes[k].push((t, w[1..].to_string()));
            }
        }
    }
    // the tick: the extra's, else the gcd of the change times
    let (tick_hz, per_tick_num, per_tick_den): ((i64, i64), i128, i128) = match extra["tick_hz"].as_array() {
        Some(a) if a.len() == 2 => {
            let (n, d) = (a[0].as_i64().unwrap_or(1) as i128, a[1].as_i64().unwrap_or(1) as i128);
            // per tick (in units) = (1 / tick) / unit = d * 1e15 / (n * unit_fs)
            let (pn, pd) = (d * 1_000_000_000_000_000, n * unit);
            let g = gcd(pn, pd).max(1);
            (fraction(n as i64, d as i64), pn / g, pd / g)
        }
        _ => {
            let g = changes.iter().flatten().fold(0i128, |a, &(x, _)| gcd(a, x)).max(1);
            // tick = 1 / (g * unit fs)
            let (n, d) = (1_000_000_000_000_000i128, g * unit);
            let gg = gcd(n, d).max(1);
            (fraction((n / gg) as i64, (d / gg) as i64), g, 1)
        }
    };
    let to_tick = |x: i128| -> i64 {
        // x / per_tick, rounded where not whole
        let num = x * per_tick_den;
        ((num as f64) / per_tick_num as f64).round() as i64
    };
    let t_end = changes.iter().flatten().map(|x| x.0).max().unwrap_or(0);
    let ticks = extra["ticks"]
        .as_i64()
        .filter(|&x| x > 0)
        .unwrap_or_else(|| ((t_end * per_tick_den) as f64 / per_tick_num as f64).ceil() as i64 + 1);
    let mut xz = 0usize;
    let mut lines = vec![];
    let mut skipped = vec![];
    let mut logic = |name: String, seq: Vec<(i128, char)>, lines: &mut Vec<Line>| {
        let lv: Vec<(i128, u8)> = seq
            .into_iter()
            .map(|(x, v)| {
                if "xXzZ".contains(v) {
                    xz += 1;
                    (x, 0)
                } else {
                    (x, (v == '1') as u8)
                }
            })
            .collect();
        let initial = match lv.first() {
            Some(&(0, v)) => v,
            _ => 0,
        };
        let (mut e, mut cur) = (vec![], initial);
        for (x, v) in lv {
            if v != cur {
                e.push(to_tick(x));
                cur = v;
            }
        }
        let step = extra["channels"][&name]["step"].as_i64().unwrap_or(1);
        lines.push(Line {
            name,
            initial,
            edges: e,
            step,
        });
    };
    for (k, (_, vtype, width, name)) in vars.iter().enumerate() {
        let seq = &changes[k];
        if vtype == "real" {
            skipped.push(json!({"name": name, "encoding": "vcd real", "analog": true}));
        } else if *width == 1 {
            logic(
                name.clone(),
                seq.iter().map(|(x, v)| (*x, v.chars().next().unwrap_or('0'))).collect(),
                &mut lines,
            );
        } else {
            let base = name.split('[').next().unwrap_or(name).to_string();
            for b in 0..*width {
                let bits = seq
                    .iter()
                    .map(|(x, v)| {
                        let pad = if v.starts_with(['0', '1']) {
                            '0'
                        } else {
                            v.chars().next().unwrap_or('0')
                        };
                        let s: String = std::iter::repeat_n(pad, width.saturating_sub(v.len()))
                            .chain(v.chars())
                            .collect();
                        (*x, s.chars().nth(width - 1 - b).unwrap_or('0'))
                    })
                    .collect();
                logic(format!("{base}[{b}]"), bits, &mut lines);
            }
        }
    }
    let mut meta: Map<String, Value> = extra["meta"].as_object().cloned().unwrap_or_default();
    if xz > 0 {
        meta.insert("vcd_x_or_z".into(), json!(xz));
    }
    Ok(Capture {
        kind: Kind::Vcd,
        tick_hz,
        id: extra["id"].as_str().map(String::from),
        complete: true,
        lost: vec![],
        layout: "timeline".into(),
        n_parts: 1,
        meta: meta.clone(),
        parts: vec![Part {
            part: 0,
            parts: None,
            ticks,
            tick0: Some(0),
            meta,
            lines,
            skipped,
        }],
    })
}
