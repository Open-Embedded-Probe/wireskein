//! The upper stage (Python's `wireskein/_upper.py`): what reads the buses' output rather than the lines, after the
//! engine has chosen its claims - RISC-V's Debug Module on RVSWD / SWIO's DMI accesses, and more to come. It reads a
//! claim as the analysis exports it (its layers) and gives it upper layers and a score raised by the checks that held
//! (`s + 0.5 (1 - s) support s`); the margin is raised as much and the verdict made again by the engine's rule.

use crate::claim::{Claim, raw};
use crate::riscv_dm::{Data, Dmi, accesses};
use crate::rvswd::dm_name;
use serde_json::{Map, Value, json};

/// What the upper stage gives a reading.
pub struct Upper {
    pub score: f64,
    pub layers: Map<String, Value>,
}

/// A reading at t raised by a child of score s, weighed by w.
pub fn support(t: f64, s: f64, w: f64) -> f64 {
    t + 0.5 * (1.0 - t) * s * w
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_u64().map(|x| x as i64))
}

/// The DMI accesses of an RVSWD / SWIO claim's `dmi` layer.
fn dmi_of(lay: &Map<String, Value>) -> Vec<Dmi> {
    let Some(items) = lay.get("dmi").and_then(|v| v.as_array()) else {
        return vec![];
    };
    items
        .iter()
        .map(|x| Dmi {
            op: match &x["op"] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            },
            addr: int(&x["addr"]),
            data: match &x["data"] {
                Value::Array(a) => Data::List(a.iter().filter_map(int).collect()),
                v => int(v).map_or(Data::Null, Data::Num),
            },
            ok: x.get("ok").and_then(|v| v.as_bool()).unwrap_or(true),
            long_failed: x.get("form").and_then(|v| v.as_str()) == Some("long")
                && x.get("status").and_then(int).is_some_and(|s| s != 0),
            s: x.get("s").and_then(int),
            e: x.get("e").and_then(int),
        })
        .collect()
}

/// RISC-V's Debug Module on the DMI accesses: (score, the `dm` layer). The score: the share of accesses to the
/// Debug Module's registers, half again with dmcontrol and dmstatus both seen.
pub fn riscv_dm(lay: &Map<String, Value>) -> Option<(f64, Vec<Value>)> {
    let tx = dmi_of(lay);
    if tx.is_empty() {
        return None;
    }
    let plain: Vec<&Dmi> = tx.iter().filter(|x| x.op != "BURST").collect();
    let known = if plain.is_empty() {
        0.0
    } else {
        plain
            .iter()
            .filter(|x| x.addr.is_some_and(|a| dm_name(a as u32).is_some()))
            .count() as f64
            / plain.len() as f64
    };
    let has = |a: i64| tx.iter().any(|x| x.addr == Some(a));
    let ctrl = has(0x10) && has(0x11);
    let log: Vec<Value> = accesses(&tx)
        .into_iter()
        .map(|e| {
            let mut m = Map::new();
            if let Some(s) = e.s {
                m.insert("s".into(), json!(s));
            }
            if let Some(t) = e.e {
                m.insert("e".into(), json!(t));
            }
            m.insert("op".into(), json!(e.op));
            m.insert("addr".into(), json!(e.addr));
            m.insert(
                "value".into(),
                match e.value {
                    Data::Num(v) => json!(v),
                    Data::List(v) => json!(v),
                    Data::Null => Value::Null,
                },
            );
            if let Some(sz) = e.size {
                m.insert("size".into(), json!(sz));
            }
            Value::Object(m)
        })
        .collect();
    Some((known * (0.5 + 0.5 * ctrl as u8 as f64), log))
}

/// The upper stage on one claim's reading.
pub fn read(protocol: &str, score: f64, lay: &Map<String, Value>) -> Upper {
    let mut up = Upper {
        score,
        layers: Map::new(),
    };
    if (protocol == "rvswd" || protocol == "swio")
        && let Some((s, log)) = riscv_dm(lay)
    {
        up.layers.insert("dm".into(), Value::Array(log));
        up.score = support(score, s, score);
    }
    up
}

/// The stage over the engine's claims: each one's upper layers added to its layers, its score raised, its margin
/// raised as much, its verdict made again.
pub fn stage(mut claims: Vec<Claim>, _rate: f64) -> Vec<Claim> {
    for c in claims.iter_mut().filter(|c| !c.bus) {
        if !matches!(c.protocol.as_str(), "rvswd" | "swio") {
            continue;
        }
        let Ok(mut lay) = serde_json::from_str::<Map<String, Value>>(c.layers.get()) else {
            continue;
        };
        let up = read(&c.protocol, c.total, &lay);
        let d = up.score - c.total;
        if d != 0.0 {
            c.total = up.score;
            let margin = c.margin + d;
            c.score = (up.score * 1e4).round() / 1e4;
            c.margin = (margin * 1e4).round() / 1e4;
            c.verdict = crate::verdict(up.score, margin, c.rival).into();
        }
        if !up.layers.is_empty() {
            lay.extend(up.layers);
            c.layers = raw(serde_json::to_string(&lay).unwrap());
        }
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_modules_reading_raises_its_bus() {
        let lay: Map<String, Value> = serde_json::from_value(json!({"dmi": [
            {"s": 1, "op": "W", "addr": 0x10, "data": 1, "ok": true},
            {"s": 2, "op": "R", "addr": 0x11, "data": 0x382, "ok": true},
            {"s": 3, "op": "W", "addr": 0x05, "data": 0x0800_0000u32, "ok": true},
            {"s": 4, "op": "W", "addr": 0x17, "data": 0x0220_0000, "ok": true},
            {"s": 5, "op": "R", "addr": 0x04, "data": 0x1234_5678, "ok": true},
        ]}))
        .unwrap();
        let up = read("rvswd", 0.79, &lay);
        assert!((up.score - support(0.79, 1.0, 0.79)).abs() < 1e-12);
        let dm = up.layers["dm"].as_array().unwrap();
        assert_eq!(dm[0]["op"], "MEMR");
        assert_eq!(dm[0]["value"], 0x1234_5678);
        assert_eq!((dm[0]["s"].clone(), dm[0]["e"].clone()), (json!(4), json!(5)));
    }
}
