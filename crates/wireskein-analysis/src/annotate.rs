//! Decoding results as rows of labelled spans for a viewer (Python's `wireskein.annotate`, decode/annotations.json,
//! docs/wireskein-format.ja.md §5.3): for each protocol found, the row is its highest decoded layer that has times,
//! placed under its data line; pairs read together (a command line and its answers) a row of their messages.

use crate::claim_doc;
use std::path::Path;
use wireskein_core::{Channel, View};
use wireskein_io::j::{J, py_eq, py_str};
use wireskein_io::jd;
use wireskein_io::write::j_of;

pub const FORMAT: &str = "wireskein-annotations/1";
/// per row: beyond this the viewer would only draw noise
pub const MAX_ITEMS: usize = 50_000;
const DATA_ROLES: [&str; 7] = ["sda", "data", "dio", "mosi", "miso", "rx", "tx"];

/// An int as Python's isinstance(v, int) takes it (a bool is one).
fn int_of(v: &J) -> Option<i64> {
    match v {
        J::Int(i) => Some(*i),
        J::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// Python's `format(v, "#0{width}x")` (a sign before the 0x).
fn hex(v: i64, width: usize) -> String {
    let body = format!("{:x}", v.unsigned_abs());
    let sign = if v < 0 { "-" } else { "" };
    let pad = width.saturating_sub(sign.len() + 2 + body.len());
    format!("{sign}0x{}{body}", "0".repeat(pad))
}

/// Python's int() of a number (a float cut toward zero).
fn int_trunc(v: &J) -> i64 {
    match v {
        J::Float(f) => f.trunc() as i64,
        x => x.i().unwrap_or(0),
    }
}

fn text(protocol: &str, it: &J) -> String {
    if it.has("text") {
        return py_str(it.get("text"));
    }
    if it.has("addr") && it.has("rw") {
        // a bus transaction
        let rw: String = py_str(it.get("rw")).chars().take(1).collect::<String>().to_uppercase();
        // an address not read (null): "?" as Python's annotate writes it
        let addr = match it.get("addr") {
            a @ (J::Int(_) | J::Float(_) | J::Bool(_)) => hex(int_trunc(a), 4),
            _ => "?".into(),
        };
        let mut s = format!("{rw} {addr}");
        let data = it.get("bytes");
        if data.truthy() {
            let d = py_str(data);
            let pairs: Vec<String> = d
                .chars()
                .collect::<Vec<_>>()
                .chunks(2)
                .map(|c| c.iter().collect())
                .collect();
            s += &format!(" {}", pairs.join(" "));
        }
        if it.get("addr_ack") == &J::Bool(false) {
            s += " NACK";
        }
        return s;
    }
    if it.has("op") && (it.has("reg") || it.has("addr")) {
        // a register access
        let reg = if it.get("reg").truthy() {
            it.get("reg")
        } else {
            it.get("addr")
        };
        let mut s = format!("{} {}", py_str(it.get("op")), py_str(reg));
        if let Some(d) = int_of(it.get("data")) {
            s += &format!(" {}", hex(d, 10));
        }
        return s;
    }
    if let Some(v) = int_of(it.get("value")) {
        // a character or word
        if protocol == "uart" {
            match v {
                32 => return "\u{2423}".into(), // open box: a space would draw nothing
                33..=126 => return char::from(v as u8).to_string(),
                9 => return "\\t".into(),
                10 => return "\\n".into(),
                13 => return "\\r".into(),
                0 => return "\\0".into(),
                _ => {}
            }
        }
        return if v < 256 { hex(v, 4) } else { hex(v, 0) };
    }
    let rest: Vec<String> = it
        .items()
        .iter()
        .filter(|(k, _)| k != "s" && k != "e")
        .map(|(k, v)| format!("{k}={}", py_str(v)))
        .collect();
    rest.join(", ").chars().take(60).collect()
}

fn level(it: &J) -> &'static str {
    match it.get("level").s() {
        Some("ok") => return "ok",
        Some("warn") => return "warn",
        Some("error") => return "error",
        _ => {}
    }
    if it.get("ok") == &J::Bool(false) || it.get("error").truthy() {
        return "error";
    }
    let mut acks = it.get("acks").list();
    let is_list = matches!(it.get("acks"), J::List(_));
    let rw = if it.has("rw") {
        py_str(it.get("rw"))
    } else {
        String::new()
    };
    if is_list && rw.starts_with('r') && !acks.is_empty() {
        acks = &acks[..acks.len() - 1]; // a read ends with the master's NACK: normal
    }
    if it.get("addr_ack") == &J::Bool(false) || (is_list && acks.iter().any(|a| py_eq(a, &J::Bool(false)))) {
        return "warn";
    }
    "ok"
}

fn detail(it: &J) -> J {
    J::Dict(
        it.items()
            .iter()
            .filter(|(k, _)| k != "s" && k != "e")
            .cloned()
            .collect(),
    )
}

/// Items with times: a list of dicts whose first has "s".
fn timed(v: &J) -> bool {
    matches!(v, J::List(x) if x.first().is_some_and(|f| matches!(f, J::Dict(_)) && f.has("s")))
}

/// Rows from an `analyze --mode all` part: its claims (with their layers) and relations.
pub fn rows_of(claims: &[J], relations: &[J]) -> Vec<J> {
    let mut rows = vec![];
    for claim in claims {
        let proto = claim.get("protocol").s().unwrap_or("");
        let layers = claim.get("layers");
        let mut found: Vec<(&str, &[J])> = layers
            .items()
            .iter()
            .filter(|(_, v)| timed(v))
            .map(|(k, v)| (k.as_str(), v.list()))
            .collect();
        // a protocol of one's own: {"messages": [...]}, its messages with times a row too
        found.extend(
            layers
                .items()
                .iter()
                .filter(|(_, v)| matches!(v, J::Dict(_)) && timed(v.get("messages")))
                .map(|(k, v)| (k.as_str(), v.get("messages").list())),
        );
        let Some(&(name, items)) = found.last() else { continue }; // the highest layer with times
        let blocks = layers.get("blocks").list();
        let unit = blocks
            .first()
            .filter(|b| b.has("unit_samples"))
            .and_then(|b| b.get("unit_samples").f()); // a character's bit time: its span is 10 bits
        let roles = claim.get("roles");
        let near = DATA_ROLES
            .iter()
            .find(|r| roles.has(r))
            .map(|r| roles.get(r).clone())
            .or_else(|| roles.items().first().map(|(_, v)| v.clone()));
        let mut out = vec![];
        for it in items.iter().take(MAX_ITEMS) {
            let mut a = jd! {"s" => int_trunc(it.get("s")), "text" => text(proto, it), "level" => level(it)};
            if it.has("e") {
                a.set("e", int_trunc(it.get("e")));
            } else if let (Some(u), "uart") = (unit, proto) {
                let s = it.get("s").f().unwrap_or(0.0);
                a.set("e", (s + 10.0 * u).trunc() as i64);
            }
            a.set("detail", detail(it));
            out.push(a);
        }
        let mut row = jd! {"name" => format!("{proto} {name}"), "items" => J::List(out)};
        if claim.get("verdict").truthy() {
            row.set("verdict", claim.get("verdict").clone()); // how sure the analysis is
        }
        if matches!(claim.get("score"), J::Int(_) | J::Float(_) | J::Bool(_)) {
            row.set("score", claim.get("score").clone());
        }
        if let Some(n) = near.filter(J::truthy) {
            row.set("near", n);
        }
        if items.len() > MAX_ITEMS {
            row.set("truncated", items.len());
        }
        rows.push(row);
    }
    // pairs read together: scpi's exchanges have no times; a protocol of one's own on a UART pair has messages with
    // them
    for rel in relations {
        let items = rel.get("messages");
        if !timed(items) {
            continue;
        }
        let proto = rel.get("protocol").s().unwrap_or("");
        let mut out = vec![];
        for it in items.list().iter().take(MAX_ITEMS) {
            let mut a = jd! {"s" => int_trunc(it.get("s")), "text" => text(proto, it), "level" => level(it),
            "detail" => detail(it)};
            if it.has("e") {
                a.set("e", int_trunc(it.get("e")));
            }
            out.push(a);
        }
        let near = rel
            .get("roles")
            .items()
            .first()
            .map(|(_, v)| v.clone())
            .unwrap_or(J::Null);
        let mut row = jd! {"name" => format!("{proto} messages"), "items" => J::List(out), "near" => near};
        if matches!(rel.get("score"), J::Int(_) | J::Float(_) | J::Bool(_)) {
            row.set("score", rel.get("score").clone());
        }
        if items.list().len() > MAX_ITEMS {
            row.set("truncated", items.list().len());
        }
        rows.push(row);
    }
    rows
}

/// A tick of a run of parts read as one, as (the file's part, the tick in it) (Python's `fileio.locate`).
fn locate(starts: &[(usize, i64)], first: usize, tick: i64) -> (usize, i64) {
    let Some(&(mut k, mut off)) = starts.first() else {
        return (first, tick);
    };
    for &(kk, o) in starts {
        if tick >= o {
            (k, off) = (kk, o);
        }
    }
    (k, tick - off)
}

/// A row of a joined stretch as rows of the file's parts (§5.3: a row's ticks are its part's), each item in the part
/// its start is in (Python's `annotate._per_part`).
fn per_part(row: &J, starts: &[(usize, i64)], first: usize, only: Option<usize>) -> Vec<J> {
    let mut by: Vec<(usize, Vec<J>)> = vec![];
    for it in row.get("items").list() {
        let s0 = it.get("s").i().unwrap_or(0);
        let (k, s) = locate(starts, first, s0);
        let mut x = it.clone();
        x.set("s", s);
        if it.has("e") {
            x.set("e", s + (it.get("e").i().unwrap_or(0) - s0));
        }
        match by.iter_mut().find(|(kk, _)| *kk == k) {
            Some((_, v)) => v.push(x),
            None => by.push((k, vec![x])),
        }
    }
    if by.is_empty() {
        by.push((locate(starts, first, 0).0, vec![]));
    }
    by.sort_by_key(|(k, _)| *k);
    by.into_iter()
        .filter(|(k, _)| only.is_none_or(|o| o == *k))
        .map(|(k, items)| {
            let mut r = row.clone();
            r.set("items", J::List(items));
            r.set("part", k);
            r
        })
        .collect()
}

/// The annotations document of a capture file (Python's `annotate.build_file`): each part (a run of parts that follow
/// on without a gap as one stretch) decoded on its own with the core, its rows naming their part when the file has
/// several; `part`: that one only. source: what made it ("wireskein VERSION analyze").
pub fn annotations_file(
    path: &Path,
    hints: &serde_json::Value,
    part: Option<usize>,
    source: &str,
) -> Result<J, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let cap = wireskein_io::read(path)?;
    let rate = cap.rate();
    let several = cap.n_parts > 1;
    if several
        && let Some(k) = part
        && k >= cap.n_parts
    {
        return Err(at(format!(
            "part {k}: the file has {} (0 .. {})",
            cap.n_parts,
            cap.n_parts - 1
        )));
    }
    let mut rows = vec![];
    for p in &cap.parts {
        let span = p.parts.unwrap_or((p.part, p.part));
        if several
            && let Some(k) = part
            && !(span.0 <= k && k <= span.1)
        {
            continue;
        }
        let chans: Vec<Channel> = p
            .lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect();
        let view = View::new(rate, p.ticks, chans).map_err(|e| at(format!("part {}: {e}", p.part)))?;
        let (claims, relations) = wireskein_core::analyze_full(&view, hints);
        // the claims as `analyze --mode all` gives them, their layers in the order the core wrote them (the highest
        // layer with times is the last)
        let claims: Vec<J> = claims
            .iter()
            .map(|c| {
                let mut d = j_of(&claim_doc(c, "all", &p.lines, rate, p.ticks, None, false));
                if let Ok(lay) = J::parse(c.layers.get()) {
                    d.set("layers", lay);
                }
                d
            })
            .collect();
        let relations: Vec<J> = relations.iter().map(j_of).collect();
        let got = rows_of(&claims, &relations);
        if !several {
            rows.extend(got);
            continue;
        }
        let starts: Vec<(usize, i64)> = p
            .meta
            .get("part_starts")
            .and_then(|v| v.as_array())
            .map_or(vec![], |a| {
                a.iter()
                    .filter_map(|x| Some((x.get(0)?.as_u64()? as usize, x.get(1)?.as_i64()?)))
                    .collect()
            });
        for r in &got {
            rows.extend(per_part(r, &starts, p.part, if several { part } else { None }));
        }
    }
    Ok(jd! {"format" => FORMAT, "source" => source, "rows" => J::List(rows)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_level_as_python() {
        let it =
            J::parse(r#"{"s": 5, "e": 9, "addr": 80, "rw": "read", "bytes": "a1b2", "acks": [true, false]}"#).unwrap();
        assert_eq!(text("i2c", &it), "R 0x50 a1 b2");
        assert_eq!(level(&it), "ok"); // a read's last NACK is the master's
        let it = J::parse(r#"{"s": 5, "value": 65}"#).unwrap();
        assert_eq!((text("uart", &it), text("spi", &it)), ("A".into(), "0x41".into()));
        assert_eq!(text("spi", &J::parse(r#"{"value": 4660}"#).unwrap()), "0x1234");
        assert_eq!(
            text(
                "x",
                &J::parse(r#"{"s": 1, "a": true, "b": null, "c": [1, "x"]}"#).unwrap()
            ),
            "a=True, b=None, c=[1, 'x']"
        );
        assert_eq!(
            text(
                "rvswd",
                &J::parse(r#"{"op": "rd", "reg": "0x10", "data": 255}"#).unwrap()
            ),
            "rd 0x10 0x000000ff"
        );
        assert_eq!(
            text("i2c", &J::parse(r#"{"addr": null, "rw": "write"}"#).unwrap()),
            "W ?"
        );
        assert_eq!(level(&J::parse(r#"{"ok": false}"#).unwrap()), "error");
        assert_eq!(level(&J::parse(r#"{"rw": "write", "acks": [1, 0]}"#).unwrap()), "warn");
    }
}
