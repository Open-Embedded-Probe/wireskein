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

/// A UART claim's bytes as the upper readings take them: values, start ticks.
#[derive(serde::Deserialize)]
struct UartLayers {
    bytes: Option<UartBytes>,
}

#[derive(serde::Deserialize)]
struct UartBytes {
    s: Vec<i64>,
    hex: String,
}

/// Upper layers on a UART's bytes: lines, NMEA, Modbus RTU (markers on the lines); the score raised by the best.
fn bytes_upper(c: &Claim, rate: f64, up: &mut Upper) -> Option<crate::duplex::Line> {
    let Ok(UartLayers { bytes: Some(b) }) = serde_json::from_str::<UartLayers>(c.layers.get()) else {
        return None;
    };
    let baud = c.params.get("baud").and_then(|v| v.as_f64())?;
    let v: Vec<i64> = (0..b.hex.len() / 2)
        .map(|i| i64::from_str_radix(&b.hex[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect();
    let l = c.params.get("L").and_then(|v| v.as_f64()).unwrap_or(10.0);
    let cs = rate / baud * l;
    let ch = crate::textup::children(&v, &b.s, cs);
    let best = ch.iter().map(|x| x.total).fold(0.0, f64::max);
    up.score = support(c.total, best, c.total);
    for x in ch {
        if x.total >= crate::textup::SHOWN {
            up.layers.insert(x.name.into(), x.items);
        }
    }
    Some(crate::duplex::Line {
        pin: c.roles.get("data").cloned().unwrap_or_default(),
        baud,
        l: c.params.get("L").and_then(|v| v.as_f64()),
        values: v.iter().map(|&x| (x & 0xFF) as u8).collect(),
        starts: b.s,
        char_samples: cs,
    })
}

/// SD in SPI mode on an SPI claim of two data lines: the score raised by its commands, the `sd` layer. Each byte's
/// tick is taken in its transfer by the clock's steady pace (the transfers give their first and last bit's).
fn sd_upper(c: &Claim, up: &mut Upper) {
    let Ok(lay) = serde_json::from_str::<Map<String, Value>>(c.layers.get()) else {
        return;
    };
    let Some(tr) = lay.get("transfers").and_then(|v| v.as_array()) else {
        return;
    };
    let pins: Vec<String> = tr
        .first()
        .and_then(|x| x.as_object())
        .map(|o| o.keys().filter(|k| *k != "s" && *k != "e").cloned().collect())
        .unwrap_or_default();
    if pins.len() != 2 {
        return;
    }
    let bytes = |x: &Value, p: &str| -> Vec<u8> {
        let h = x.get(p).and_then(|v| v.as_str()).unwrap_or("");
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or(0))
            .collect()
    };
    let (mut a, mut b): (Vec<crate::sdspi::Row>, Vec<crate::sdspi::Row>) = (vec![], vec![]);
    for x in tr {
        let (s0, e0) = (x["s"].as_i64().unwrap_or(0), x["e"].as_i64().unwrap_or(0));
        let (p, q) = (bytes(x, &pins[0]), bytes(x, &pins[1]));
        let n = p.len().min(q.len());
        let span = (8 * n).saturating_sub(1).max(1) as f64;
        for k in 0..n {
            let t = s0 + ((e0 - s0) as f64 * (8 * k) as f64 / span).round() as i64;
            a.push((t, p[k], q[k]));
            b.push((t, q[k], p[k]));
        }
    }
    if let Some((score, sd)) = crate::sdspi::sd_spi(&a, &b) {
        up.layers.insert("sd".into(), Value::Array(sd));
        up.score = support(c.total, score, c.total);
    }
}

/// Device packs on an I2C bus's transactions or an SPI bus's transfers: the `devices` layer (matches and
/// candidates); a pack identified (its identity read, or its checks passed) raises the bus.
fn devices_upper(c: &Claim, packs: &crate::devices::PackSet, up: &mut Upper) {
    let Ok(lay) = serde_json::from_str::<Map<String, Value>>(c.layers.get()) else {
        return;
    };
    let hex = |h: &str| -> Vec<u8> {
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or(0))
            .collect()
    };
    let ms = if c.protocol == "i2c" {
        let txs: Vec<crate::devices::Tx> = lay
            .get("transactions")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| {
                        Some(crate::devices::Tx {
                            s: t.get("s").cloned().unwrap_or(Value::Null),
                            addr: t.get("addr")?.as_i64()?,
                            read: t.get("rw").and_then(|v| v.as_str()).map(|r| r == "read"),
                            ack: t.get("addr_ack").and_then(|v| v.as_bool()).unwrap_or(false),
                            bytes: hex(t.get("bytes").and_then(|v| v.as_str()).unwrap_or("")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        crate::devices::match_i2c(&txs, packs)
    } else {
        let tr = lay
            .get("transfers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let pins: Vec<String> = tr
            .first()
            .and_then(|x| x.as_object())
            .map(|o| o.keys().filter(|k| *k != "s" && *k != "e").cloned().collect())
            .unwrap_or_default();
        let lines: std::collections::BTreeMap<String, Vec<Vec<u8>>> = pins
            .iter()
            .map(|p| {
                let v = tr
                    .iter()
                    .map(|x| hex(x.get(p).and_then(|v| v.as_str()).unwrap_or("")))
                    .collect();
                (p.clone(), v)
            })
            .collect();
        crate::devices::match_spi(&lines, packs)
    };
    if ms.is_empty() {
        return;
    }
    if ms.iter().any(|m| m.level == "identified") {
        up.score = support(up.score, up.score, up.score);
    }
    up.layers
        .insert("devices".into(), Value::Array(crate::devices::exported(&ms)));
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

/// Layers written as JSON with more added (spliced: a UART's characters are millions, no tree of them).
fn with_layers(lay: &serde_json::value::RawValue, extra: Map<String, Value>) -> Box<serde_json::value::RawValue> {
    let s = lay.get().trim_end();
    let body = s.strip_suffix('}').unwrap_or(s);
    let mut out = String::with_capacity(s.len() + 64);
    out.push_str(body);
    let mut first = body.trim_end().ends_with('{');
    for (k, v) in extra {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&serde_json::to_string(&k).unwrap());
        out.push(':');
        out.push_str(&serde_json::to_string(&v).unwrap());
    }
    out.push('}');
    raw(out)
}

/// Every two UART lines paired: SCPI on the pair, its support flowing down to both lines (their scores in `ups`).
/// -> the relations shown (score at SHOWN or more), as the analysis format has them.
fn pairs(lines: &[(usize, crate::duplex::Line)], ups: &mut [Option<Upper>]) -> Vec<Value> {
    let mut rel = vec![];
    for i in 0..lines.len() {
        for j in i + 1..lines.len() {
            let ((ia, a), (ib, b)) = (&lines[i], &lines[j]);
            let sa = ups[*ia].as_ref().map_or(0.0, |u| u.score);
            let sb = ups[*ib].as_ref().map_or(0.0, |u| u.score);
            if sa <= 0.3 || sb <= 0.3 || a.pin == b.pin {
                continue;
            }
            let Some((msgs, m)) = crate::duplex::pair(a, b) else {
                continue;
            };
            if m < 0.4 {
                continue;
            }
            if let Some((s, host, ex)) = crate::duplex::scpi_of(&msgs, &a.pin, &b.pin) {
                for k in [*ia, *ib] {
                    if let Some(u) = ups[k].as_mut() {
                        u.score = support(u.score, s, m);
                    }
                }
                if s >= crate::textup::SHOWN {
                    let dev = if host == a.pin { &b.pin } else { &a.pin };
                    rel.push(json!({"protocol": "scpi", "roles": {"tx": host, "rx": dev},
                                    "score": (s * 1e4).round() / 1e4, "exchanges": ex}));
                }
            }
        }
    }
    rel
}

/// The stage over the engine's claims: each one's upper layers added to its layers, its score raised, its margin
/// raised as much, its verdict made again; the UART pairs' relations beside them. Every built-in device pack is
/// tried (`stage_with_packs` for the packs a caller picks).
pub fn stage_with(claims: Vec<Claim>, rate: f64) -> (Vec<Claim>, Vec<Value>) {
    stage_with_packs(claims, rate, &crate::devices::PackSet::builtin(&["**".into()]))
}

/// The stage with the device packs a caller picks (`PackSet::chosen`, `PackSet::from_hints`): only these are matched
/// on I2C and SPI buses, and only these raise a bus they identify.
pub fn stage_with_packs(
    mut claims: Vec<Claim>,
    rate: f64,
    packs: &crate::devices::PackSet,
) -> (Vec<Claim>, Vec<Value>) {
    let mut ups: Vec<Option<Upper>> = Vec::with_capacity(claims.len());
    let mut lines = vec![];
    for (i, c) in claims.iter().enumerate() {
        if c.bus.is_some() {
            ups.push(None);
            continue;
        }
        let mut up = Upper {
            score: c.total,
            layers: Map::new(),
        };
        if c.protocol == "uart" {
            if let Some(l) = bytes_upper(c, rate, &mut up) {
                lines.push((i, l));
            }
        } else if c.protocol == "i2c" {
            devices_upper(c, packs, &mut up);
        } else if c.protocol == "spi" {
            devices_upper(c, packs, &mut up);
            sd_upper(c, &mut up);
        } else if matches!(c.protocol.as_str(), "rvswd" | "swio")
            && let Ok(lay) = serde_json::from_str::<Map<String, Value>>(c.layers.get())
        {
            up = read(&c.protocol, c.total, &lay);
        }
        ups.push(Some(up));
    }
    let rel = pairs(&lines, &mut ups);
    for (c, up) in claims.iter_mut().zip(ups) {
        let Some(up) = up else { continue };
        let d = up.score - c.total;
        if d != 0.0 {
            c.total = up.score;
            let margin = c.margin + d;
            c.score = (up.score * 1e4).round() / 1e4;
            c.margin = (margin * 1e4).round() / 1e4;
            c.verdict = crate::verdict(up.score, margin, c.rival).into();
        }
        if !up.layers.is_empty() {
            c.layers = with_layers(&c.layers, up.layers);
        }
    }
    (claims, rel)
}

/// The stage's claims (the relations left out).
pub fn stage(claims: Vec<Claim>, rate: f64) -> Vec<Claim> {
    stage_with(claims, rate).0
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

    /// An SHT30 read (a measure command, then six bytes with their CRCs) on an I2C claim of 0.8.
    fn sht30_claim() -> Claim {
        fn crc(d: &[u8]) -> u8 {
            let mut c = 0xFFu8;
            for &b in d {
                c ^= b;
                for _ in 0..8 {
                    c = if c & 0x80 != 0 { (c << 1) ^ 0x31 } else { c << 1 };
                }
            }
            c
        }
        let (t, h) = ([0x66u8, 0x4C], [0x5Au8, 0x10]);
        let r: String = [t[0], t[1], crc(&t), h[0], h[1], crc(&h)]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let lay = json!({"transactions": [
            {"s": 0, "addr": 0x44, "rw": "write", "addr_ack": true, "bytes": "2400"},
            {"s": 1, "addr": 0x44, "rw": "read", "addr_ack": true, "bytes": r},
        ]});
        Claim {
            protocol: "i2c".into(),
            roles: [
                ("scl".to_string(), "A".to_string()),
                ("sda".to_string(), "B".to_string()),
            ]
            .into(),
            verdict: crate::verdict(0.8, 0.8, false).into(),
            score: 0.8,
            margin: 0.8,
            params: Map::new(),
            evidence: Map::new(),
            layers: crate::claim::raw(lay.to_string()),
            total: 0.8,
            rival: false,
            bus: None,
        }
    }

    #[test]
    fn only_the_packs_picked_raise_a_bus() {
        use crate::devices::PackSet;
        let all = stage_with(vec![sht30_claim()], 1e6).0;
        assert!(all[0].total > 0.8 && all[0].layers.get().contains("SHT30"));
        // the hints' patterns, as Python's --devices: the SHT30's pack left out, the score stays
        let none = PackSet::from_hints(&json!({"devices": ["!i2c/sensirion/**"]}));
        assert_eq!(none.len(), 3);
        let out = stage_with_packs(vec![sht30_claim()], 1e6, &none).0;
        assert_eq!(out[0].total, 0.8);
        assert!(!out[0].layers.get().contains("SHT30"));
        let some = PackSet::chosen(&["i2c/sensirion/*".to_string()], &[] as &[&str]);
        assert_eq!(some.names(), ["i2c/sensirion/sht30.toml"]);
        assert_eq!(
            stage_with_packs(vec![sht30_claim()], 1e6, &some).0[0].total,
            all[0].total
        );
        // no patterns: every pack, as before
        assert_eq!(PackSet::from_hints(&json!({"devices": []})).len(), 4);
        assert_eq!(PackSet::from_hints(&Value::Null).len(), 4);
    }

    #[test]
    fn a_directorys_packs_beside_the_built_in_ones() {
        use crate::devices::PackSet;
        let dir = std::env::temp_dir().join(format!("wireskein-core-packs-{}", std::process::id()));
        let sub = dir.join("i2c/acme");
        std::fs::create_dir_all(&sub).unwrap();
        let sht = include_str!("../../../src/wireskein/decl/devices/i2c/sensirion/sht30.toml");
        std::fs::write(sub.join("sht30_clone.toml"), sht.replacen("\"SHT30\"", "\"ACME30\"", 1)).unwrap();
        let d = dir.to_string_lossy().to_string();
        let only = PackSet::from_hints(&json!({"devices": ["!i2c/sensirion/**"], "device_dirs": [d]}));
        assert!(
            only.names().contains(&"i2c/acme/sht30_clone.toml"),
            "{:?}",
            only.names()
        );
        let out = stage_with_packs(vec![sht30_claim()], 1e6, &only).0;
        // every pack: WireSkein's first, then the directory's (Python's PackSets), not all of them by their paths
        let every = PackSet::from_hints(&json!({"device_dirs": [d]}));
        let both = stage_with_packs(vec![sht30_claim()], 1e6, &every).0;
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            every.names(),
            [
                "i2c/qst/qmp6988.toml",
                "i2c/sensirion/sht30.toml",
                "spi/flash/spi_nor.toml",
                "spi/radio/cc1101.toml",
                "i2c/acme/sht30_clone.toml"
            ]
        );
        // the indistinguishable match's packs in the order tried
        assert!(
            both[0]
                .layers
                .get()
                .contains("[\"i2c/sensirion/sht30.toml\",\"i2c/acme/sht30_clone.toml\"]"),
            "{}",
            both[0].layers.get()
        );
        assert!(
            out[0].total > 0.8 && out[0].layers.get().contains("ACME30"),
            "{}",
            out[0].layers.get()
        );
    }
}
