//! Decoding the buses a spec names (Python's `decode.decode_file`; docs/analysis-format.md §2): every part of a
//! capture read with the spec's roles, however weak the evidence, with what is needed to trust it later - each bus's
//! result at the spec's depth, how well it read (`reading`), what it did not read (`uninterpreted`, `shared` with
//! another bus of the spec), the declared lines' changes, the pins the spec did not name (`undeclared`, with the
//! claims on them). A long part is read in pieces (`split`), the pieces' results joined; the items may go to a file
//! as they are read (ndjson) for a capture whose items are more than memory holds.

use crate::report::{self, IDLE_BITS, IDLE_S, STOPPED_AFTER};
use crate::split::{self, Options, Piece, Splitter};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use wireskein_core::{Candidate, Claim};
use wireskein_io::Line;
use wireskein_io::j::J;
use wireskein_io::write::j_of;

/// The depths a spec reads to (Python's `export.DEPTHS`).
pub const DEPTHS: [&str; 4] = ["transport", "frames", "protocol", "device"];
const REANALYSES: i64 = 16;
const DOUBT: f64 = 0.3;

/// A bus the spec names.
#[derive(Debug, Clone)]
pub struct SpecBus {
    pub protocol: String,
    /// role -> pin, in the spec's order
    pub pins: Vec<(String, String)>,
    pub params: Map<String, Value>,
    pub idle_s: Option<f64>,
    pub layers: Option<Vec<String>>,
}

impl SpecBus {
    fn pin(&self, role: &str) -> Option<&str> {
        self.pins.iter().find(|(r, _)| r == role).map(|(_, p)| p.as_str())
    }
    fn pin_set(&self) -> BTreeSet<&str> {
        self.pins.iter().map(|(_, p)| p.as_str()).collect()
    }
    fn pins_doc(&self) -> Value {
        Value::Object(self.pins.iter().map(|(r, p)| (r.clone(), json!(p))).collect())
    }
}

/// A decode spec as Python's `decode._spec` makes it.
#[derive(Debug, Clone)]
pub struct Spec {
    pub buses: Vec<SpecBus>,
    pub lines: Vec<String>,
    pub devices: Value,
    pub depth: String,
}

impl Spec {
    /// From the spec as given ({"buses": [{"protocol", "pins", "params", "idle_s", "layers"}], "lines", "devices",
    /// "depth"}).
    pub fn parse(text: &str) -> Result<Spec, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("spec: {e}"))?;
        let mut s = Spec::of(&v)?;
        // each bus's pins in the spec's order (its lines are told in that order)
        let j = wireskein_io::j::J::parse(text)?;
        for (b, jb) in s.buses.iter_mut().zip(j.get("buses").list()) {
            let order: Vec<&str> = jb.get("pins").items().iter().map(|(k, _)| k.as_str()).collect();
            b.pins.sort_by_key(|(r, _)| order.iter().position(|x| x == r));
        }
        Ok(s)
    }

    /// From the spec as a value (its pins in the roles' order).
    pub fn of(v: &Value) -> Result<Spec, String> {
        let mut buses = vec![];
        for b in v.get("buses").and_then(|x| x.as_array()).into_iter().flatten() {
            let protocol = b.get("protocol").and_then(|x| x.as_str()).unwrap_or("");
            let pins = b.get("pins").and_then(|x| x.as_object()).filter(|o| !o.is_empty());
            let (true, Some(pins)) = (!protocol.is_empty(), pins) else {
                return Err(format!("a bus needs protocol and pins: {b}"));
            };
            buses.push(SpecBus {
                protocol: protocol.into(),
                pins: pins
                    .iter()
                    .map(|(r, p)| (r.clone(), p.as_str().map_or_else(|| p.to_string(), String::from)))
                    .collect(),
                params: b.get("params").and_then(|x| x.as_object()).cloned().unwrap_or_default(),
                idle_s: b.get("idle_s").and_then(|x| x.as_f64()),
                layers: b
                    .get("layers")
                    .and_then(|x| x.as_array())
                    .filter(|a| !a.is_empty())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
            });
        }
        let depth = v.get("depth").and_then(|x| x.as_str()).unwrap_or("device").to_string();
        if !DEPTHS.contains(&depth.as_str()) {
            return Err(format!("depth: one of {}", DEPTHS.join(", ")));
        }
        Ok(Spec {
            buses,
            lines: v
                .get("lines")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
            devices: v.get("devices").cloned().unwrap_or(Value::Null),
            depth,
        })
    }

    /// As the document gives it.
    pub fn doc(&self) -> Value {
        let buses: Vec<Value> = self
            .buses
            .iter()
            .map(|b| {
                let mut d = json!({"protocol": b.protocol, "pins": b.pins_doc(), "params": b.params});
                if let Some(x) = b.idle_s {
                    d["idle_s"] = json!(x);
                }
                if let Some(l) = &b.layers {
                    d["layers"] = json!(l);
                }
                d
            })
            .collect();
        json!({"buses": buses, "lines": self.lines, "devices": self.devices, "depth": self.depth})
    }

    /// As the document gives it, its keys in Python's order (`decode._spec`) and each bus's pins in the spec's.
    pub fn doc_j(&self) -> J {
        let buses: Vec<J> = self
            .buses
            .iter()
            .map(|b| {
                let mut d = J::Dict(vec![
                    ("protocol".into(), J::from(b.protocol.as_str())),
                    (
                        "pins".into(),
                        J::Dict(b.pins.iter().map(|(r, p)| (r.clone(), J::from(p.as_str()))).collect()),
                    ),
                    ("params".into(), j_of(&Value::Object(b.params.clone()))),
                ]);
                if let Some(x) = b.idle_s {
                    d.set("idle_s", x);
                }
                if let Some(l) = &b.layers {
                    d.set("layers", J::List(l.iter().map(|x| J::from(x.as_str())).collect()));
                }
                d
            })
            .collect();
        J::Dict(vec![
            ("buses".into(), J::List(buses)),
            (
                "lines".into(),
                J::List(self.lines.iter().map(|x| J::from(x.as_str())).collect()),
            ),
            ("devices".into(), j_of(&self.devices)),
            ("depth".into(), J::from(self.depth.as_str())),
        ])
    }

    /// Each bus's roles in the spec's order.
    pub fn roles_order(&self) -> Vec<Vec<String>> {
        self.buses
            .iter()
            .map(|b| b.pins.iter().map(|(r, _)| r.clone()).collect())
            .collect()
    }

    /// A decode document of this spec in Python's order (`order::decoding`).
    pub fn ordered(&self, doc: &Value) -> J {
        crate::order::decoding(doc, Some(&self.doc_j()), &self.roles_order())
    }
}

fn line<'a>(lines: &'a [Line], name: &str) -> Option<&'a Line> {
    lines.iter().find(|l| l.name == name)
}

fn after(l: &Line, i: usize) -> u8 {
    l.initial ^ (((i + 1) & 1) as u8)
}

/// The hints a set of buses is read with (Python's `decode._hints`): strict, only their protocols and their pins.
fn hints_of(buses: &[&SpecBus], names: &[String], strict: bool) -> Value {
    let mut pins = Map::new();
    for b in buses {
        for (role, pin) in &b.pins {
            let mut h = json!({"protocol": b.protocol, "role": role});
            for (k, v) in &b.params {
                h[k] = v.clone();
            }
            pins.insert(pin.clone(), h);
        }
    }
    let mut out = json!({"pins": pins,
                         "buses": buses.iter().map(|b| json!({"protocol": b.protocol, "pins": b.pins_doc()})).collect::<Vec<_>>()});
    if strict && !buses.is_empty() {
        let protos: BTreeSet<&str> = buses.iter().map(|b| b.protocol.as_str()).collect();
        out["protocols"] = json!(protos);
        let named: BTreeSet<&str> = buses.iter().flat_map(|b| b.pin_set()).collect();
        let mut ex: Vec<&String> = names.iter().filter(|n| !named.contains(n.as_str())).collect();
        ex.sort();
        out["exclude_pins"] = json!(ex);
    }
    out
}

/// Whether some of these buses share a line.
fn shares_lines(buses: &[&SpecBus]) -> bool {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for b in buses {
        let p = b.pin_set();
        if !p.is_disjoint(&seen) {
            return true;
        }
        seen.extend(p);
    }
    false
}

/// The spec's buses in sets that name no pin under two protocols (each read on its own).
fn apart(buses: &[SpecBus]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = vec![];
    'b: for (i, b) in buses.iter().enumerate() {
        for g in groups.iter_mut() {
            let owner: BTreeMap<&str, &str> = g
                .iter()
                .flat_map(|&j| {
                    buses[j]
                        .pins
                        .iter()
                        .map(move |(_, p)| (p.as_str(), buses[j].protocol.as_str()))
                })
                .collect();
            if b.pins
                .iter()
                .all(|(_, p)| owner.get(p.as_str()).is_none_or(|x| *x == b.protocol))
            {
                g.push(i);
                continue 'b;
            }
        }
        groups.push(vec![i]);
    }
    if groups.is_empty() {
        groups.push(vec![]);
    }
    groups
}

/// A set of buses read: the engine's claims before and after the upper stage, the readings no claim is (the best
/// of each protocol and roles).
struct Reads {
    pre: Vec<Claim>,
    up: Vec<Claim>,
    others: Vec<Candidate>,
}

fn read(view: &wireskein_core::View, hints: &Value) -> Reads {
    let cands = wireskein_core::readings(view, hints);
    let pre = wireskein_core::choose(cands.clone());
    let (up, _) = wireskein_core::upper::stage_with(pre.clone(), view.rate);
    let key = |p: &str, r: &BTreeMap<String, String>| (p.to_string(), r.clone());
    let held: BTreeSet<(String, BTreeMap<String, String>)> = pre.iter().map(|c| key(&c.protocol, &c.roles)).collect();
    let mut best: Vec<Candidate> = vec![];
    for c in cands {
        if held.contains(&key(&c.protocol, &c.roles)) {
            continue;
        }
        match best.iter_mut().find(|b| b.protocol == c.protocol && b.roles == c.roles) {
            Some(b) if c.score > b.score => *b = c,
            Some(_) => {}
            None => best.push(c),
        }
    }
    Reads { pre, up, others: best }
}

/// The edge an SPI bus samples on: params' sample_edge, else from its mode, else read from the lines (Python's
/// `decode._sample_edge`).
fn sample_edge(lines: &[Line], n: i64, b: &SpecBus) -> Option<String> {
    if let Some(e) = b.params.get("sample_edge").and_then(|v| v.as_str())
        && (e == "rise" || e == "fall")
    {
        return Some(e.into());
    }
    if b.protocol != "spi" {
        return None;
    }
    if let Some(m) = b.params.get("mode") {
        let m = m
            .as_i64()
            .or_else(|| m.as_f64().map(|x| x as i64))
            .or_else(|| m.as_str()?.parse().ok())?;
        return Some(if m >> 1 == m & 1 { "rise" } else { "fall" }.into());
    }
    use wireskein_verify::cap::Ch;
    let ch = |name: &str| line(lines, name).map(|l| Ch::new(&l.name, l.initial, l.edges.clone()));
    let clk = ch(b.pin("clk")?)?;
    let data: Vec<Ch> = ["mosi", "miso"].iter().filter_map(|r| b.pin(r).and_then(ch)).collect();
    let cs = b.pin("cs").and_then(ch);
    let active = b.params.get("cs_active").and_then(|v| v.as_i64()).unwrap_or(0) as u8;
    let m = wireskein_verify::timing::spi_mode(&clk, &data.iter().collect::<Vec<_>>(), n, cs.as_ref(), active);
    let cpha = m.lines.iter().find_map(|(_, v)| v.cpha)?;
    let cpol = m.cpol?;
    Some(if cpol == cpha { "rise" } else { "fall" }.into())
}

/// The reading a bus is: its layers after the upper stage, its params, its fit, whether it is claimed (and its
/// verdict then).
struct Node {
    protocol: String,
    roles: BTreeMap<String, String>,
    params: Map<String, Value>,
    lay: Map<String, Value>,
    score: f64,
    claimed: Option<String>,
}

/// A reading through the upper stage on its own (no pairs).
fn alone(
    protocol: &str,
    roles: &BTreeMap<String, String>,
    score: f64,
    layers: &serde_json::value::RawValue,
    rate: f64,
) -> (Map<String, Value>, f64) {
    let c = Claim {
        protocol: protocol.into(),
        roles: roles.clone(),
        verdict: String::new(),
        score,
        margin: 0.0,
        params: Map::new(),
        evidence: Map::new(),
        layers: wireskein_core::claim::raw(layers.get().to_string()),
        total: score,
        rival: false,
        bus: None,
    };
    let (up, _) = wireskein_core::upper::stage_with(vec![c], rate);
    let c = &up[0];
    (crate::order::layers(&c.layers), c.total)
}

/// The decode with exactly the bus's pins, the best one (on the edge it samples on, when there is one).
fn node(r: &Reads, b: &SpecBus, edge: Option<&str>, rate: f64) -> Option<Node> {
    let pins = b.pin_set();
    let has = |p: &str, roles: &BTreeMap<String, String>| {
        p == b.protocol && roles.values().map(|s| s.as_str()).collect::<BTreeSet<_>>() == pins
    };
    // (claims first, then the other readings, as Python's res.roots)
    let mut cands: Vec<(usize, bool, f64, &Map<String, Value>)> = vec![];
    for (i, c) in r.pre.iter().enumerate() {
        if has(&c.protocol, &c.roles) {
            cands.push((i, true, c.score, &c.params));
        }
    }
    for (i, c) in r.others.iter().enumerate() {
        if has(&c.protocol, &c.roles) {
            cands.push((i, false, c.score, &c.params));
        }
    }
    if let Some(e) = edge {
        let same: Vec<_> = cands
            .iter()
            .copied()
            .filter(|c| c.3.get("sample_edge") == Some(&json!(e)))
            .collect();
        if !same.is_empty() {
            cands = same;
        }
    }
    let mut best: Option<(usize, bool, f64)> = None;
    for &(i, cl, s, _) in &cands {
        if best.is_none_or(|b| s > b.2) {
            best = Some((i, cl, s));
        }
    }
    let (i, claimed, _) = best?;
    if claimed {
        let c = &r.up[i];
        let (lay, score) = if c.bus.is_none() {
            (crate::order::layers(&c.layers), c.total)
        } else {
            // a target of a bus of several: the upper stage passed it by; read on its own
            alone(&c.protocol, &c.roles, r.pre[i].total, &r.pre[i].layers, rate)
        };
        Some(Node {
            protocol: c.protocol.clone(),
            roles: c.roles.clone(),
            params: r.pre[i].params.clone(),
            lay,
            score,
            claimed: Some(c.verdict.clone()),
        })
    } else {
        let c = &r.others[i];
        let (lay, score) = alone(&c.protocol, &c.roles, c.score, &c.layers, rate);
        Some(Node {
            protocol: c.protocol.clone(),
            roles: c.roles.clone(),
            params: c.params.clone(),
            lay,
            score,
            claimed: None,
        })
    }
}

/// The SYNC roles a clocked protocol has (Python's `staged.SYNC_ROLES`).
fn sync_roles(protocol: &str) -> Option<&'static [&'static str]> {
    match protocol {
        "i2c" => Some(&["scl", "sda"]),
        "spi" => Some(&["clk", "cs", "miso", "mosi"]),
        "rvswd" => Some(&["clk", "dio"]),
        _ => None,
    }
}

/// The result at a depth (Python's `export.final_of` and decode's `_bus`).
fn result_of(n: &Node, b: &SpecBus, depth: &str, devices: &[Value]) -> (Map<String, Value>, Option<Vec<Value>>) {
    let mut res = Map::new();
    let low = report::lower(&n.protocol);
    let mut devs = None;
    match depth {
        "transport" => {}
        "frames" => {
            if let Some(v) = n.lay.get(low) {
                res.insert(low.into(), v.clone());
            }
        }
        _ => {
            if let Some(k) = crate::final_layers(&n.protocol)
                .iter()
                .find(|k| n.lay.get(**k).is_some_and(crate::nonempty))
            {
                res.insert(k.to_string(), n.lay[*k].clone());
            }
            if let Some(v) = n.lay.get(low)
                && !res.contains_key(low)
            {
                res.insert(low.into(), v.clone());
            }
            if depth == "device" {
                let ds: Vec<Value> = devices
                    .iter()
                    .filter(|m| matches!(m["level"].as_str(), Some("identified" | "consistent" | "address_only")))
                    .cloned()
                    .collect();
                if !ds.is_empty() {
                    devs = Some(ds);
                }
            }
        }
    }
    if let Some(ls) = &b.layers {
        let keys: Vec<String> = if ls.iter().any(|k| k == "*") {
            n.lay.keys().cloned().collect()
        } else {
            ls.clone()
        };
        for k in keys {
            if let Some(v) = n.lay.get(&k) {
                res.insert(k, v.clone());
            }
        }
    }
    (res, devs)
}

/// An SPI bus whose bytes go least significant bit first: its transfers' bytes read so.
fn lsb_first(res: &mut Map<String, Value>, b: &SpecBus) {
    let pins = b.pin_set();
    let rev = |h: &str| -> String {
        (0..h.len() / 2)
            .map(|i| {
                format!(
                    "{:02x}",
                    u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or(0).reverse_bits()
                )
            })
            .collect()
    };
    if let Some(Value::Array(tr)) = res.get_mut("transfers") {
        for x in tr.iter_mut().filter_map(|x| x.as_object_mut()) {
            for (k, v) in x.iter_mut() {
                if pins.contains(k.as_str())
                    && let Value::String(h) = v
                {
                    *h = rev(h);
                }
            }
        }
    }
}

/// The idle a bus's activity is cut into bursts at (Python's `decode._idle` for another engine's reading): idle_s
/// when given, else 16 periods of its clock's rises, else 1 ms.
fn idle_of(lines: &[Line], b: &SpecBus, n: Option<&Node>, rate: f64) -> i64 {
    if let Some(s) = b.idle_s {
        return (s * rate).round_ties_even() as i64;
    }
    if let Some(n) = n {
        let clk = b.pin("clk").or_else(|| n.roles.get("clk").map(|s| s.as_str()));
        if let Some(c) = clk.and_then(|c| line(lines, c)) {
            let rises: Vec<i64> = (0..c.edges.len())
                .filter(|&i| after(c, i) == 1)
                .map(|i| c.edges[i])
                .collect();
            if rises.len() > 1 {
                let mut d: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
                return (IDLE_BITS as f64 * report::median(&mut d)) as i64;
            }
        }
    }
    (IDLE_S * rate).round_ties_even() as i64
}

/// SDA changing in the same sample as an SCL edge (Python's `timing.i2c_same_sample`): the ticks.
fn same_sample(scl: &Line, sda: &Line, lo: i64, hi: i64) -> usize {
    if scl.step != 1 || sda.step != 1 {
        return 0;
    }
    let s: BTreeSet<i64> = sda.edges.iter().copied().collect();
    scl.edges
        .iter()
        .filter(|t| s.contains(t) && **t >= lo && **t < hi)
        .count()
}

/// One bus of the spec read in a part (Python's `decode._bus`); its frames' spans under "_spans".
#[allow(clippy::too_many_arguments)]
fn bus(
    r: &Reads,
    lines: &[Line],
    rate: f64,
    n_ticks: i64,
    b: &SpecBus,
    depth: &str,
    gf: Option<&Value>,
) -> Map<String, Value> {
    let edge = sample_edge(lines, n_ticks, b);
    let mut nd = node(r, b, edge.as_deref(), rate);
    let mut out = Map::new();
    out.insert("protocol".into(), json!(b.protocol));
    out.insert("pins".into(), b.pins_doc());
    let mut devices = vec![];
    if let Some(n) = nd.as_mut()
        && let Some(Value::Array(d)) = n.lay.remove("devices")
    {
        devices = d;
    }
    let clk_name = b.pin("clk");
    let clk = clk_name.and_then(|c| line(lines, c));
    // each frame given its clock (frame_clocks), before the result takes them
    if let (Some(n), Some(c)) = (nd.as_mut(), clk)
        && let Some(fr) = report::frame_items(&mut n.lay, &b.protocol)
    {
        report::frame_clocks(fr, c, rate);
    }
    match &nd {
        None => {
            out.insert("decoded".into(), json!(false));
            out.insert("result".into(), json!({}));
        }
        Some(n) => {
            out.insert("decoded".into(), json!(true));
            let mut params = n.params.clone();
            let mut fit = json!({"score": wireskein_io::pyfmt::round_n(n.score, 4), "claimed": n.claimed.is_some()});
            if let Some(v) = &n.claimed {
                fit["verdict"] = json!(v);
            }
            let (mut res, devs) = result_of(n, b, depth, &devices);
            if b.protocol == "spi" && b.params.get("bit_order").and_then(|v| v.as_str()) == Some("lsb") {
                lsb_first(&mut res, b);
                params.insert("bit_order".into(), json!("lsb"));
            }
            out.insert("params".into(), Value::Object(params));
            out.insert("fit".into(), fit);
            out.insert("result".into(), Value::Object(res));
            if let Some(d) = devs {
                out.insert("devices".into(), json!(d));
            }
        }
    }
    if let Some(known) = sync_roles(&b.protocol) {
        let mut odd: Vec<&str> = b
            .pins
            .iter()
            .map(|(r, _)| r.as_str())
            .filter(|r| {
                !known.contains(r)
                    && !(r.starts_with("data") && r.len() > 4 && r[4..].bytes().all(|c| c.is_ascii_digit()))
            })
            .collect();
        odd.sort();
        if !odd.is_empty() {
            out.insert("roles_unknown".into(), json!({"given": odd, "roles": known}));
        }
    }
    let idle = idle_of(lines, b, nd.as_ref(), rate);
    let pin_lines: Vec<&Line> = b.pins.iter().filter_map(|(_, p)| line(lines, p)).collect();
    let sel = if b.protocol == "spi" {
        b.pin("cs").and_then(|p| line(lines, p))
    } else {
        None
    };
    let empty_lay = Map::new();
    let lay = nd.as_ref().map_or(&empty_lay, |n| &n.lay);
    let mut empty = vec![];
    let proto = if nd.is_some() { b.protocol.as_str() } else { "" };
    let un = report::unread(proto, &pin_lines, lay, idle, sel, n_ticks, &mut empty);
    out.insert("uninterpreted".into(), json!(un));
    if !empty.is_empty() {
        out.insert("empty_selections".into(), json!(empty));
    }
    out.insert("idle_ticks".into(), json!(idle));
    let frames: Vec<Value> = nd
        .as_ref()
        .and_then(|n| {
            let k = if n.lay.get("dmi").is_some_and(|v| v.is_array()) {
                "dmi"
            } else {
                report::lower(&n.protocol)
            };
            n.lay.get(k).and_then(|v| v.as_array()).cloned()
        })
        .unwrap_or_default();
    let speeds = if clk.is_some() {
        report::clock_speeds(rate, &frames)
    } else {
        vec![]
    };
    if !speeds.is_empty() {
        out.insert("clock_speeds".into(), json!(speeds));
    }
    if let Some(c) = clk_name
        && let Some(mut us) = report::undersampled(rate, &speeds, c)
    {
        us["restored"] = json!(frames.iter().filter(|x| x.get("restored").is_some()).count());
        out.insert("undersampled".into(), us);
    }
    if nd.is_some() {
        let data: Vec<&Line> = b
            .pins
            .iter()
            .filter(|(r, _)| r != "clk")
            .filter_map(|(_, p)| line(lines, p))
            .collect();
        let mut rd = report::reading(&frames, Some(&un), &speeds, &pin_lines, clk, &data, n_ticks);
        if !empty.is_empty() {
            rd.insert("empty_selections".into(), json!(empty.len()));
        }
        if let Some(g) = gf
            && (rd.contains_key("stopped") || rd.contains_key("oscillating"))
        {
            rd.insert("glitch_filter_ns".into(), g.clone());
        }
        if b.protocol == "i2c"
            && let (Some(scl), Some(sda)) = (
                b.pin("scl").and_then(|p| line(lines, p)),
                b.pin("sda").and_then(|p| line(lines, p)),
            )
        {
            rd.insert("same_sample".into(), json!(same_sample(scl, sda, i64::MIN, i64::MAX)));
        }
        out.insert("reading".into(), Value::Object(rd));
    }
    let spans: Vec<Value> = frames
        .iter()
        .filter_map(|x| {
            let s = x.get("s")?.as_i64()?;
            Some(json!([s, x.get("e").and_then(|v| v.as_i64()).unwrap_or(s)]))
        })
        .collect();
    out.insert("_spans".into(), Value::Array(spans));
    out
}

fn edges_of(lines: &[Line], pins: &BTreeSet<&str>) -> Vec<i64> {
    let mut e: Vec<i64> = lines
        .iter()
        .filter(|l| pins.contains(l.name.as_str()))
        .flat_map(|l| l.edges.iter().copied())
        .collect();
    e.sort_unstable();
    e
}

/// Buses of a spec that share lines (one SPI bus, a target on each CS): a stretch one bus did not read that another
/// sharing a line with it read is that one's (`shared`), not unread; a line stopping while the others go on is no
/// fault where the changes after it are the other buses' (Python's `decode._shared`).
fn shared(buses: &mut [Map<String, Value>], lines: &[Line]) {
    let pins_of = |b: &Map<String, Value>| -> BTreeSet<String> {
        b["pins"]
            .as_object()
            .map(|o| o.values().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let spans_of = |b: &Map<String, Value>| -> Vec<(i64, i64)> {
        b.get("_spans")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|x| (x[0].as_i64().unwrap_or(0), x[1].as_i64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let all_pins: Vec<BTreeSet<String>> = buses.iter().map(pins_of).collect();
    let all_spans: Vec<Vec<(i64, i64)>> = buses.iter().map(spans_of).collect();
    for i in 0..buses.len() {
        let pins = &all_pins[i];
        let others: Vec<usize> = (0..buses.len())
            .filter(|&j| j != i && !pins.is_disjoint(&all_pins[j]) && !all_spans[j].is_empty())
            .collect();
        if others.is_empty() {
            continue;
        }
        let b = &mut buses[i];
        let idle = b.get("idle_ticks").and_then(|v| v.as_i64()).unwrap_or(0);
        let margin = (idle / IDLE_BITS * 2).max(1);
        let mut spans: Vec<(i64, i64, usize)> = others
            .iter()
            .flat_map(|&j| all_spans[j].iter().map(move |&(s, e)| (s, e, j)))
            .collect();
        spans.sort_unstable();
        let arr: Vec<(i64, i64)> = spans.iter().map(|&(s, e, _)| (s, e)).collect();
        let pset: BTreeSet<&str> = pins.iter().map(|s| s.as_str()).collect();
        let e_all = edges_of(lines, &pset);
        let (mut keep, mut moved): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
        for u in b
            .get("uninterpreted")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let (us, ue) = (u["s"].as_i64().unwrap_or(0), u["e"].as_i64().unwrap_or(0));
            let x: Vec<i64> = e_all.iter().copied().filter(|&t| t >= us && t <= ue).collect();
            let left = report::uncovered(&x, &arr, margin);
            if left.len() == x.len() {
                keep.push(u);
                continue;
            }
            let near: Vec<usize> = spans
                .iter()
                .filter(|&&(s0, e0, _)| s0 <= ue && e0 >= us)
                .map(|&(_, _, j)| j)
                .collect();
            // the commonest (Python's max(set(near), key=near.count): on a tie the set's first, the smallest)
            let set: BTreeSet<usize> = near.iter().copied().collect();
            let mut bus_j = None;
            let mut most = 0;
            for &j in &set {
                let k = near.iter().filter(|&&x| x == j).count();
                if k > most {
                    most = k;
                    bus_j = Some(j);
                }
            }
            let mut m = u.as_object().cloned().unwrap_or_default();
            m.insert("edges".into(), json!(x.len() - left.len()));
            m.insert("bus".into(), json!(bus_j));
            moved.push(Value::Object(m));
            for y in report::split_at_gaps(&left, idle) {
                keep.push(json!({"s": y[0], "e": y[y.len() - 1], "edges": y.len()}));
            }
        }
        if b["protocol"] == "rvswd" || b["protocol"] == "swio" {
            keep.retain(|u| u["edges"].as_i64().unwrap_or(0) >= 3);
        }
        if moved.is_empty() {
            continue;
        }
        let sum = |v: &[Value]| v.iter().map(|u| u["edges"].as_i64().unwrap_or(0)).sum::<i64>();
        let (nk, ek, nm, em) = (keep.len(), sum(&keep), moved.len(), sum(&moved));
        b.insert("uninterpreted".into(), json!(keep));
        b.insert("shared".into(), json!(moved));
        let Some(Value::Object(r)) = b.get_mut("reading") else {
            continue;
        };
        r.insert("unread".into(), json!(nk));
        r.insert("unread_edges".into(), json!(ek));
        r.insert("shared".into(), json!(nm));
        r.insert("shared_edges".into(), json!(em));
        if let Some(Value::Array(st)) = r.get("stopped").cloned() {
            let lo: Vec<i64> = spans.iter().map(|x| x.0).collect();
            let mut hi = vec![];
            let mut m = i64::MIN;
            for x in &spans {
                m = m.max(x.1);
                hi.push(m);
            }
            let mut still = vec![];
            for x in st {
                let at = x["tick"].as_i64().unwrap_or(0);
                let name = x["line"].as_str().unwrap_or("");
                let mut n_after = 0usize;
                for p in pins.iter().filter(|p| p.as_str() != name) {
                    let Some(l) = line(lines, p) else { continue };
                    for &e in l.edges.iter().filter(|&&e| e > at) {
                        let k = lo.partition_point(|&v| v <= e) as i64 - 1;
                        if k < 0 || e > hi[k as usize] {
                            n_after += 1;
                        }
                    }
                }
                if n_after >= STOPPED_AFTER {
                    let mut y = x.as_object().cloned().unwrap_or_default();
                    y.insert("others_after".into(), json!(n_after));
                    still.push(Value::Object(y));
                }
            }
            if still.is_empty() {
                r.remove("stopped");
            } else {
                r.insert("stopped".into(), json!(still));
            }
        }
    }
}

/// The changes on one line cut where none comes for more than `idle` ticks.
fn bursts(e: &[i64], idle: i64) -> Vec<Vec<i64>> {
    report::split_at_gaps(e, idle)
}

/// A part read with the spec's roles (Python's `decode._part`). strict: only the spec's protocols on the spec's pins
/// (a piece of a long part). head: the part's keys (part, tick0, start_ns ...).
pub fn part(
    lines: &[Line],
    rate: f64,
    n: i64,
    spec: &Spec,
    head: &Map<String, Value>,
    strict: bool,
    gf: Option<&Value>,
) -> Result<Map<String, Value>, String> {
    let names: Vec<String> = lines.iter().map(|l| l.name.clone()).collect();
    for b in &spec.buses {
        let miss: Vec<&str> = b
            .pins
            .iter()
            .map(|(_, p)| p.as_str())
            .filter(|p| !names.iter().any(|x| x == p))
            .collect();
        if !miss.is_empty() {
            return Err(format!("{}: pins not in the capture: {}", b.protocol, miss.join(", ")));
        }
    }
    let miss: Vec<&str> = spec
        .lines
        .iter()
        .map(|s| s.as_str())
        .filter(|p| !names.iter().any(|x| x == p))
        .collect();
    if !miss.is_empty() {
        return Err(format!("lines not in the capture: {}", miss.join(", ")));
    }
    let view = split::view_of(lines, rate, n)?;
    let groups = apart(&spec.buses);
    let mut reads = vec![];
    let mut of = vec![0usize; spec.buses.len()];
    for (gi, g) in groups.iter().enumerate() {
        let bs: Vec<&SpecBus> = g.iter().map(|&i| &spec.buses[i]).collect();
        let mut hints = hints_of(&bs, &names, strict);
        if shares_lines(&bs) {
            // buses sharing lines (an SPI bus with a target on each CS): the engine reads them as one bus of
            // targets, whose selects are roles of their own ("cs", "cs1" ...) - not the spec's: the spec's buses'
            // protocols on their pins only, the roles left to the engine (each target matched by its pins)
            let named: BTreeSet<&str> = bs.iter().flat_map(|b| b.pin_set()).collect();
            let protos: BTreeSet<&str> = bs.iter().map(|b| b.protocol.as_str()).collect();
            let mut ex: Vec<&String> = names.iter().filter(|n| !named.contains(n.as_str())).collect();
            ex.sort();
            hints = json!({"protocols": protos, "exclude_pins": ex});
        }
        reads.push(read(&view, &hints));
        for &i in g {
            of[i] = gi;
        }
    }
    let mut out = head.clone();
    out.insert("ticks".into(), json!(n));
    let mut buses: Vec<Map<String, Value>> = spec
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| bus(&reads[of[i]], lines, rate, n, b, &spec.depth, gf))
        .collect();
    shared(&mut buses, lines);
    for b in buses.iter_mut() {
        b.remove("_spans");
    }
    out.insert("buses".into(), json!(buses));
    out.insert(
        "lines".into(),
        json!(
            spec.lines
                .iter()
                .map(|p| {
                    let l = line(lines, p).unwrap();
                    json!({"pin": p, "initial": l.initial, "edges": l.edges})
                })
                .collect::<Vec<_>>()
        ),
    );
    let named: BTreeSet<&str> = spec
        .buses
        .iter()
        .flat_map(|b| b.pin_set())
        .chain(spec.lines.iter().map(|s| s.as_str()))
        .collect();
    let idle = (IDLE_S * rate).round_ties_even() as i64;
    let mut rest = vec![];
    for l in lines {
        if named.contains(l.name.as_str()) || l.edges.is_empty() {
            continue;
        }
        let bs = bursts(&l.edges, idle);
        rest.push(json!({"pin": l.name, "edges": l.edges.len(), "s": l.edges[0], "e": l.edges[l.edges.len() - 1], "bursts": bs.len()}));
    }
    out.insert("undeclared".into(), json!(rest));
    let others: Vec<Value> = reads[0]
        .up
        .iter()
        .filter(|c| c.roles.values().all(|p| !named.contains(p.as_str())))
        .map(|c| {
            let mut d = crate::claim_doc(c, "final", lines, rate, n, None, false);
            if let Some(bus) = &c.bus {
                d["bus"] = json!(bus);
            }
            d
        })
        .collect();
    if !others.is_empty() {
        out.insert("claims".into(), json!(others));
    }
    Ok(out)
}

// ---------------- a long part in pieces ----------------

fn is_whole(p: &Piece) -> bool {
    p.cut == "end" && p.a == 0
}

/// The items of a piece's bus that start in its own ticks and read well (Python's `decode._owned`).
fn owned(p: &Piece, b: &Map<String, Value>) -> usize {
    let mut n = 0;
    for v in b
        .get("result")
        .and_then(|v| v.as_object())
        .into_iter()
        .flat_map(|o| o.values())
    {
        match v {
            Value::Array(a) => {
                n += a
                    .iter()
                    .filter(|x| {
                        x.get("s")
                            .and_then(|s| s.as_i64())
                            .is_some_and(|s| p.s <= s + p.a && s + p.a < p.e)
                            && x.get("ok") != Some(&json!(false))
                    })
                    .count()
            }
            Value::Object(o) => {
                if let Some(Value::Array(s)) = o.get("s") {
                    n += s
                        .iter()
                        .filter_map(|s| s.as_i64())
                        .filter(|s| p.s <= s + p.a && s + p.a < p.e)
                        .count();
                }
            }
            _ => {}
        }
    }
    n
}

fn doubt_why(p: &Piece, o: &Map<String, Value>) -> Vec<String> {
    let mut why = vec![];
    for (j, b) in o["buses"].as_array().into_iter().flatten().enumerate() {
        let Some(b) = b.as_object() else { continue };
        let proto = b["protocol"].as_str().unwrap_or("");
        let own = b
            .get("_owned")
            .and_then(|v| v.as_u64())
            .map_or_else(|| owned(p, b), |v| v as usize);
        let score = b
            .get("fit")
            .and_then(|f| f.get("score"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);
        if b.get("decoded") != Some(&json!(true)) {
            why.push(format!("bus {j} ({proto}) not decoded"));
        } else if score < DOUBT {
            why.push(format!(
                "bus {j} ({proto}) fits {}",
                wireskein_io::pyfmt::repr_f64(score)
            ));
        } else if own == 0 {
            why.push(format!("bus {j} ({proto}) read nothing well in this piece's own ticks"));
        }
    }
    why
}

/// One piece read with the spec's roles (Python's `decode._piece`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn piece(
    p: &Piece,
    w: &[Line],
    rate: f64,
    spec: &Spec,
    head: &Map<String, Value>,
    reanalyze: bool,
    edges: &BTreeMap<usize, String>,
    span: i64,
    gf: Option<&Value>,
) -> Result<Map<String, Value>, String> {
    let whole = is_whole(p);
    let n = p.b - p.a;
    let mut o = part(w, rate, n, spec, head, !whole, gf)?;
    let other: BTreeMap<usize, &String> = edges
        .iter()
        .filter(|(j, e)| {
            let got = o["buses"][**j]["params"]["sample_edge"].as_str();
            got.is_some_and(|g| g != e.as_str())
        })
        .map(|(j, e)| (*j, e))
        .collect();
    if !other.is_empty() {
        let mut given = spec.clone();
        for (j, e) in &other {
            given.buses[*j].params.insert("sample_edge".into(), json!(e));
        }
        o = part(w, rate, n, &given, head, !whole, gf)?;
    }
    if let Some(Value::Array(bs)) = o.get_mut("buses") {
        for (j, b) in spec.buses.iter().enumerate() {
            let bo = bs[j].as_object_mut().unwrap();
            let fr: Option<Vec<Value>> = bo.get("result").and_then(|res| {
                let v = if res.get("dmi").is_some_and(|v| v.is_array()) {
                    res.get("dmi")
                } else {
                    res.get(report::lower(&b.protocol))
                };
                v.and_then(|v| v.as_array()).cloned()
            });
            let Some(Value::Object(r)) = bo.get_mut("reading") else {
                continue;
            };
            if let Some(fr) = fr {
                let own: Vec<&Value> = fr
                    .iter()
                    .filter(|x| {
                        x.get("s")
                            .and_then(|s| s.as_i64())
                            .is_some_and(|s| p.s <= s + p.a && s + p.a < p.e)
                    })
                    .collect();
                r.insert("frames".into(), json!(own.len()));
                r.insert(
                    "ok".into(),
                    json!(own.iter().filter(|x| x.get("ok") != Some(&json!(false))).count()),
                );
                r.insert(
                    "unanswered".into(),
                    json!(own.iter().filter(|x| x.get("answered") == Some(&json!(false))).count()),
                );
                r.insert(
                    "restored".into(),
                    json!(own.iter().filter(|x| x.get("restored").is_some()).count()),
                );
                r.insert(
                    "repaired".into(),
                    json!(own.iter().filter(|x| x.get("repaired").is_some()).count()),
                );
            }
            if r.contains_key("same_sample")
                && let (Some(scl), Some(sda)) = (
                    b.pin("scl").and_then(|q| line(w, q)),
                    b.pin("sda").and_then(|q| line(w, q)),
                )
            {
                r.insert("same_sample".into(), json!(same_sample(scl, sda, p.s - p.a, p.e - p.a)));
            }
            if let Some(c) = b.pin("clk").and_then(|q| line(w, q)) {
                let data: Vec<&Line> = b
                    .pins
                    .iter()
                    .filter(|(k, _)| k != "clk")
                    .filter_map(|(_, q)| line(w, q))
                    .collect();
                r.extend(report::noise(&[], c, &data, p.s - p.a, p.e - p.a));
                report::shares(r);
            }
        }
    }
    let sampled = span <= 0
        || (0..REANALYSES).any(|k| {
            let t = ((k as f64 + 0.5) * span as f64 / REANALYSES as f64) as i64;
            p.s <= t && t < p.e
        });
    if reanalyze && !whole && p.edges > 0 && sampled && !doubt_why(p, &o).is_empty() {
        let view = split::view_of(w, rate, n)?;
        let (claims, _) = wireskein_core::analyze_full(&view, &Value::Null);
        let holds: Vec<Value> = claims
            .iter()
            .filter(|c| c.verdict == "confirmed" || c.verdict == "likely")
            .map(|c| {
                let mut h = json!({"protocol": c.protocol, "roles": c.roles, "verdict": c.verdict, "score": c.score});
                if !c.params.is_empty() {
                    h["params"] = Value::Object(c.params.clone());
                }
                h
            })
            .collect();
        o.insert("_reanalysis".into(), json!(holds));
    }
    Ok(o)
}

/// Items written out as the pieces are read (Python's `decode._Items`): each piece's list layers in the part's
/// ticks, kept by the piece they start in; the Debug Module read over the pieces' accesses when they are all read.
pub struct Items {
    f: std::io::BufWriter<std::fs::File>,
    part: usize,
    protocols: Vec<String>,
    pub(crate) counts: Vec<Map<String, Value>>,
    pub(crate) unread: Vec<Vec<Value>>,
    dmi: BTreeMap<usize, Vec<Value>>,
}

impl Items {
    pub(crate) fn new(path: &Path, spec: &Spec, part: usize) -> Result<Items, String> {
        let protocols: Vec<&str> = spec.buses.iter().map(|b| b.protocol.as_str()).collect();
        let f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Items {
            f: std::io::BufWriter::new(f),
            part,
            protocols: protocols.iter().map(|p| p.to_string()).collect(),
            counts: vec![Map::new(); protocols.len()],
            unread: vec![vec![]; protocols.len()],
            dmi: protocols
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == "rvswd" || **b == "swio")
                .map(|(j, _)| (j, vec![]))
                .collect(),
        })
    }

    fn write(&mut self, j: usize, layer: &str, rows: &[Value]) -> Result<(), String> {
        for r in rows {
            // {"part", "bus", "layer", **item} as Python writes it (the item's keys in its own order after)
            let mut d = J::Dict(vec![
                ("part".into(), J::Int(self.part as i64)),
                ("bus".into(), J::Int(j as i64)),
                ("layer".into(), J::from(layer)),
            ]);
            match crate::order::core_j(r, crate::order::frame_added(&self.protocols[j])) {
                J::Dict(mut o) => {
                    if layer == "dm" {
                        // (Python's `_dm`: the Debug Module's accesses as it lists them)
                        let dm = crate::order::DM;
                        let at = |k: &str| dm.iter().position(|x| *x == k).unwrap_or(dm.len());
                        o.sort_by_key(|(k, _)| at(k));
                    }
                    for (k, v) in o {
                        d.set(&k, v);
                    }
                }
                x => d.set("item", x), // an item that is not an object (a text line)
            }
            writeln!(self.f, "{}", d.dumps()).map_err(|e| e.to_string())?;
        }
        let c = self.counts[j].entry(layer.to_string()).or_insert(json!(0));
        *c = json!(c.as_u64().unwrap_or(0) + rows.len() as u64);
        Ok(())
    }

    pub(crate) fn take(&mut self, p: &Piece, o: &mut Map<String, Value>) -> Result<(), String> {
        let Some(Value::Array(bs)) = o.get_mut("buses") else {
            return Ok(());
        };
        for (j, b) in bs.iter_mut().enumerate() {
            let b = b.as_object_mut().unwrap();
            let own = owned(p, b);
            b.insert("_owned".into(), json!(own));
            let dm_bus = self.dmi.contains_key(&j);
            if let Some(Value::Object(res)) = b.get_mut("result") {
                let keys: Vec<String> = res.keys().cloned().collect();
                for k in keys {
                    let Value::Array(v) = &res[&k] else { continue };
                    if k == "dm" && dm_bus {
                        continue;
                    }
                    let rows = split::join_items(&[(p, &Value::Array(v.clone()))]);
                    self.write(j, &k, &rows)?;
                    if k == "dmi" && dm_bus {
                        self.dmi.get_mut(&j).unwrap().extend(rows);
                    }
                    res.insert(k, json!([]));
                }
                res.remove("dm");
            }
            let un = split::join_items(&[(p, b.get("uninterpreted").unwrap_or(&json!([])))]);
            self.unread[j].extend(
                un.iter()
                    .map(|u| json!({"s": u.get("s"), "e": u.get("e"), "edges": u.get("edges")})),
            );
            self.write(j, "uninterpreted", &un)?;
            b.insert("uninterpreted".into(), json!([]));
        }
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), String> {
        let dmi = std::mem::take(&mut self.dmi);
        for (j, rows) in dmi {
            let mut m = Map::new();
            m.insert("dmi".into(), Value::Array(rows));
            let dm = wireskein_core::upper::riscv_dm(&m).map(|x| x.1).unwrap_or_default();
            self.write(j, "dm", &dm)?;
        }
        self.f.flush().map_err(|e| e.to_string())
    }
}

/// line_faults over the pieces (Python's `decode._join_faults`); order: the lines in the spec's order (each piece's
/// line_changes is a map sorted by name here, a dict in the spec's order in Python).
fn join_faults(per: &[(&Piece, &Map<String, Value>)], order: &[&str]) -> Map<String, Value> {
    let rank = |n: &str| order.iter().position(|x| *x == n).unwrap_or(order.len());
    let mut out = Map::new();
    let mut lines: Vec<(String, i64, Option<i64>)> = vec![];
    let mut counts: Vec<(&Piece, Map<String, Value>)> = vec![];
    let mut osc = vec![];
    for (p, b) in per {
        let r = b.get("reading").and_then(|v| v.as_object());
        let ls = r
            .and_then(|r| r.get("line_changes"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let mut names: Vec<&String> = ls.keys().collect();
        names.sort_by_key(|n| rank(n));
        for n in names {
            let v = &ls[n];
            let k = match lines.iter().position(|x| &x.0 == n) {
                Some(k) => k,
                None => {
                    lines.push((n.clone(), 0, None));
                    lines.len() - 1
                }
            };
            if let Some(l) = v["last"].as_i64() {
                let last = l + p.a;
                if p.s <= last && last < p.e {
                    lines[k].2 = Some(lines[k].2.unwrap_or(0).max(last));
                }
            }
            lines[k].1 += v["changes"].as_i64().unwrap_or(0);
        }
        counts.push((p, ls));
        for x in r
            .and_then(|r| r.get("oscillating"))
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let s = x["s"].as_i64().unwrap_or(0) + p.a;
            if p.s <= s && s < p.e {
                let mut y = x.as_object().cloned().unwrap_or_default();
                y.insert("s".into(), json!(s));
                y.insert("e".into(), json!(x["e"].as_i64().unwrap_or(0) + p.a));
                osc.push(Value::Object(y));
            }
        }
    }
    if !osc.is_empty() {
        out.insert("oscillating".into(), json!(osc));
    }
    if !lines.is_empty() {
        out.insert(
            "line_changes".into(),
            Value::Object(
                lines
                    .iter()
                    .map(|(n, c, l)| (n.clone(), json!({"changes": c, "last": l})))
                    .collect(),
            ),
        );
    }
    let end = per.iter().map(|(p, _)| p.e).max().unwrap_or(0);
    let mut stopped = vec![];
    for (name, _, last) in &lines {
        let at = last.unwrap_or(-1);
        let n_after: i64 = counts
            .iter()
            .filter(|(p, _)| p.s > at)
            .flat_map(|(_, ls)| {
                ls.iter()
                    .filter(|(m, _)| *m != name)
                    .map(|(_, w)| w["changes"].as_i64().unwrap_or(0))
            })
            .sum();
        if n_after >= STOPPED_AFTER as i64 && at < end - 1 {
            stopped.push(json!({"line": name, "tick": at, "others_after": n_after}));
        }
    }
    if !stopped.is_empty() {
        out.insert("stopped".into(), json!(stopped));
    }
    let gf = per.iter().find_map(|(_, b)| {
        b.get("reading")
            .and_then(|r| r.get("glitch_filter_ns"))
            .filter(|v| crate::nonempty(v) && *v != &json!(0))
            .cloned()
    });
    if let Some(g) = gf
        && (out.contains_key("stopped") || out.contains_key("oscillating"))
    {
        out.insert("glitch_filter_ns".into(), g);
    }
    out
}

/// Clock speeds from the pieces as one (Python's `decode._join_speeds`).
fn join_speeds(groups: &[Value], rate: f64) -> Vec<Value> {
    let mut gs: Vec<&Value> = groups.iter().collect();
    gs.sort_by(|a, b| {
        a["period_ns"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&b["period_ns"].as_f64().unwrap_or(0.0))
            .unwrap()
    });
    // [period, high, s, e, frames, fastest]
    let mut out: Vec<[f64; 6]> = vec![];
    for g in gs {
        let f = |k: &str| g[k].as_f64().unwrap_or(0.0);
        let (p, h, s, e, fr) = (f("period_ns"), f("high_ns"), f("s"), f("e"), f("frames"));
        match out.last_mut() {
            Some(x) if p <= x[5] * report::SAME_SPEED => {
                let n = x[4] + fr;
                x[0] = (x[0] * x[4] + p * fr) / n;
                x[1] = (x[1] * x[4] + h * fr) / n;
                x[2] = x[2].min(s);
                x[3] = x[3].max(e);
                x[4] = n;
            }
            _ => out.push([p, h, s, e, fr, p]),
        }
    }
    out.iter()
        .map(|x| report::speed(&[[x[0], x[1], x[2], x[3]]], rate, Some(x[4] as usize)))
        .collect()
}

/// A bus's results from the pieces as one (Python's `decode._join_bus`).
/// order: the bus's lines in its spec's order (the order its lines are told in, as Python's dicts keep it).
fn join_bus(per: &[(&Piece, &Map<String, Value>)], rate: f64, sunk: bool, order: &[&str]) -> Map<String, Value> {
    let first = per
        .iter()
        .map(|x| x.1)
        .find(|b| b.get("decoded") == Some(&json!(true)))
        .unwrap_or(per[0].1);
    let mut out: Map<String, Value> = first
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "result" | "uninterpreted" | "fit" | "_owned"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert(
        "decoded".into(),
        json!(per.iter().any(|(_, b)| b.get("decoded") == Some(&json!(true)))),
    );
    let fits: Vec<&Value> = per.iter().filter_map(|(_, b)| b.get("fit")).collect();
    if !fits.is_empty() {
        let sc = |f: &Value| f["score"].as_f64().unwrap_or(0.0);
        let mut worst = fits[0];
        for f in &fits {
            if sc(f) < sc(worst) {
                worst = f;
            }
        }
        let mut w = worst.as_object().cloned().unwrap_or_default();
        w.insert(
            "pieces".into(),
            json!(fits.iter().map(|f| f["score"].clone()).collect::<Vec<_>>()),
        );
        out.insert("fit".into(), Value::Object(w));
    }
    let mut keys: Vec<String> = vec![];
    for (_, b) in per {
        for k in b
            .get("result")
            .and_then(|v| v.as_object())
            .into_iter()
            .flat_map(|o| o.keys())
        {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    let mut res = Map::new();
    for k in keys {
        let vals: Vec<(&Piece, &Value)> = per
            .iter()
            .filter_map(|(p, b)| b["result"].get(&k).map(|v| (*p, v)))
            .collect();
        let v0 = vals[0].1;
        let joined = if v0.is_array() {
            Value::Array(split::join_items(&vals))
        } else if v0
            .as_object()
            .is_some_and(|o| o.values().any(|x| x.is_array()) && (o.contains_key("t") || o.contains_key("s")))
        {
            split::join_columns(&vals)
        } else {
            v0.clone()
        };
        res.insert(k, joined);
    }
    let proto = out["protocol"].as_str().unwrap_or("").to_string();
    if (proto == "rvswd" || proto == "swio") && res.contains_key("dmi") && !sunk {
        let mut m = Map::new();
        m.insert("dmi".into(), res["dmi"].clone());
        let dm = wireskein_core::upper::riscv_dm(&m).map(|x| x.1).unwrap_or_default();
        res.insert("dm".into(), Value::Array(dm));
    }
    out.remove("undersampled");
    out.remove("clock_speeds");
    let sp: Vec<(&Piece, Value)> = per
        .iter()
        .map(|(p, b)| (*p, b.get("clock_speeds").cloned().unwrap_or(json!([]))))
        .collect();
    let speeds = join_speeds(
        &split::join_items(&sp.iter().map(|(p, v)| (*p, v)).collect::<Vec<_>>()),
        rate,
    );
    if !speeds.is_empty() {
        out.insert("clock_speeds".into(), json!(speeds));
    }
    let clk = out["pins"].get("clk").and_then(|v| v.as_str()).map(String::from);
    if let Some(c) = &clk
        && let Some(mut us) = report::undersampled(rate, &speeds, c)
    {
        let restored = match res.get("dmi").and_then(|v| v.as_array()) {
            Some(d) if !sunk => d.iter().filter(|x| x.get("restored").is_some()).count() as i64,
            _ => per
                .iter()
                .map(|(_, b)| b.get("undersampled").and_then(|u| u["restored"].as_i64()).unwrap_or(0))
                .sum(),
        };
        us["restored"] = json!(restored);
        out.insert("undersampled".into(), us);
    }
    let un: Vec<(&Piece, Value)> = per
        .iter()
        .map(|(p, b)| (*p, b.get("uninterpreted").cloned().unwrap_or(json!([]))))
        .collect();
    let uninterpreted = split::join_items(&un.iter().map(|(p, v)| (*p, v)).collect::<Vec<_>>());
    out.remove("reading");
    let rs: Vec<&Map<String, Value>> = per
        .iter()
        .filter_map(|(_, b)| b.get("reading").and_then(|v| v.as_object()))
        .collect();
    if !rs.is_empty() {
        let fr: Option<&Vec<Value>> = if sunk {
            None // the items went to a file: the pieces' counts summed
        } else if let Some(Value::Array(d)) = res.get("dmi") {
            Some(d)
        } else {
            res.get(report::lower(&proto)).and_then(|v| v.as_array())
        };
        let rate_ok = speeds.iter().all(|g| g["rate_ok"] == json!(true));
        let mut r = match fr {
            Some(fr) => {
                let f: Vec<&Value> = fr.iter().filter(|x| x.is_object()).collect();
                let mut r = Map::new();
                r.insert("frames".into(), json!(f.len()));
                r.insert(
                    "ok".into(),
                    json!(f.iter().filter(|x| x.get("ok") != Some(&json!(false))).count()),
                );
                r.insert(
                    "unanswered".into(),
                    json!(f.iter().filter(|x| x.get("answered") == Some(&json!(false))).count()),
                );
                r.insert(
                    "restored".into(),
                    json!(f.iter().filter(|x| x.get("restored").is_some()).count()),
                );
                r.insert(
                    "repaired".into(),
                    json!(f.iter().filter(|x| x.get("repaired").is_some()).count()),
                );
                r
            }
            None => ["frames", "ok", "unanswered", "restored", "repaired"]
                .iter()
                .map(|k| {
                    (
                        k.to_string(),
                        json!(
                            rs.iter()
                                .map(|x| x.get(*k).and_then(|v| v.as_i64()).unwrap_or(0))
                                .sum::<i64>()
                        ),
                    )
                })
                .collect(),
        };
        r.insert("rate_ok".into(), json!(rate_ok));
        if sunk {
            let un_n: i64 = rs
                .iter()
                .map(|x| x.get("unread").and_then(|v| v.as_i64()).unwrap_or(0))
                .sum();
            let un_e: i64 = rs
                .iter()
                .map(|x| x.get("unread_edges").and_then(|v| v.as_i64()).unwrap_or(0))
                .sum();
            r.insert("unread".into(), json!(un_n));
            r.insert("unread_edges".into(), json!(un_e));
        } else {
            r.insert("unread".into(), json!(uninterpreted.len()));
            r.insert(
                "unread_edges".into(),
                json!(
                    uninterpreted
                        .iter()
                        .map(|u| u["edges"].as_i64().unwrap_or(0))
                        .sum::<i64>()
                ),
            );
        }
        if let Some(Value::Array(e)) = out.get("empty_selections")
            && !e.is_empty()
        {
            r.insert("empty_selections".into(), json!(e.len()));
        }
        if rs.iter().any(|x| x.contains_key("same_sample")) {
            r.insert(
                "same_sample".into(),
                json!(
                    rs.iter()
                        .map(|x| x.get("same_sample").and_then(|v| v.as_i64()).unwrap_or(0))
                        .sum::<i64>()
                ),
            );
        }
        if rs.iter().any(|x| x.contains_key("glitches")) {
            for key in ["glitches", "glitch_edges", "clock_edges"] {
                r.insert(
                    key.into(),
                    json!(
                        rs.iter()
                            .map(|x| x.get(key).and_then(|v| v.as_i64()).unwrap_or(0))
                            .sum::<i64>()
                    ),
                );
            }
            for key in ["ringing", "ringing_edges", "data_edges"] {
                let mut acc = Map::new();
                for x in &rs {
                    for (k, v) in x.get(key).and_then(|v| v.as_object()).into_iter().flatten() {
                        let a = acc.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
                        acc.insert(k.clone(), json!(a + v.as_i64().unwrap_or(0)));
                    }
                }
                r.insert(key.into(), Value::Object(acc));
            }
            report::shares(&mut r);
        }
        r.extend(join_faults(per, order));
        out.insert("reading".into(), Value::Object(r));
    }
    out.insert("uninterpreted_summary".into(), split::unread_summary(&uninterpreted));
    out.insert("uninterpreted".into(), json!(uninterpreted));
    out.insert("result".into(), Value::Object(res));
    out
}

/// A source of a part's lines read in pieces with the spec's roles, the pieces' results joined (Python's
/// `decode._part_split`).
#[allow(clippy::too_many_arguments)]
pub fn part_split(
    src: &mut dyn wireskein_io::stream::Source,
    rate: f64,
    spec: &Spec,
    head: &Map<String, Value>,
    o: Option<&Options>,
    items: Option<(&Path, &str)>,
    gf: Option<&Value>,
) -> Result<Map<String, Value>, String> {
    let whole_opts = Options {
        max_edges: usize::MAX / 4,
        ..Default::default()
    };
    let opts = o.unwrap_or(&whole_opts);
    let n = src.n();
    let watch: Vec<String> = spec
        .buses
        .iter()
        .flat_map(|b| b.pins.iter().map(|(_, p)| p.clone()))
        .chain(spec.lines.iter().cloned())
        .collect();
    let part_no = head.get("part").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut sink = match items {
        Some((path, _)) => Some(Items::new(path, spec, part_no)?),
        None => None,
    };
    let mut sp = Splitter::watching(src, opts, &watch)?;
    // the edge each clocked bus samples on, from the first of up to roles_from pieces that decodes it
    let want: Vec<usize> = (0..spec.buses.len())
        .filter(|&j| !spec.buses[j].params.contains_key("sample_edge"))
        .collect();
    let mut first: Vec<(Piece, Vec<Line>)> = vec![];
    let mut edges: BTreeMap<usize, String> = BTreeMap::new();
    if !want.is_empty() {
        for (p, w) in sp.by_ref() {
            let o0 = part(&w, rate, p.b - p.a, spec, head, true, gf)?;
            first.push((p, w));
            for &j in &want {
                let b = &o0["buses"][j];
                if let Some(e) = b["params"]["sample_edge"].as_str()
                    && b["decoded"] == json!(true)
                    && !edges.contains_key(&j)
                    && (e == "rise" || e == "fall")
                {
                    edges.insert(j, e.into());
                }
            }
            if edges.len() == want.len() || first.len() >= opts.roles_from {
                break;
            }
        }
    }
    let mut outs: Vec<(Piece, Map<String, Value>)> = vec![];
    let work = |p: Piece,
                w: Vec<Line>,
                outs: &mut Vec<(Piece, Map<String, Value>)>,
                sink: &mut Option<Items>|
     -> Result<(), String> {
        let mut o = piece(&p, &w, rate, spec, head, opts.reanalyze, &edges, n, gf)?;
        if let Some(s) = sink.as_mut() {
            s.take(&p, &mut o)?;
        }
        outs.push((p, o));
        Ok(())
    };
    for (p, w) in first {
        work(p, w, &mut outs, &mut sink)?;
    }
    for (p, w) in sp.by_ref() {
        work(p, w, &mut outs, &mut sink)?;
    }
    let noisy = std::mem::take(&mut sp.noisy);
    if let Some(s) = sink.as_mut() {
        s.close()?;
    }
    let mut out = join_part(outs, spec, n, rate, o, noisy, sink.is_some());
    if let (Some(s), Some((_, name))) = (&sink, items) {
        sunk(&mut out, s, name);
    }
    Ok(out)
}

/// The buses of a part whose items went to a file: each one's result their counts, its unread stretches summed up.
pub(crate) fn sunk(out: &mut Map<String, Value>, s: &Items, name: &str) {
    if let Some(Value::Array(bs)) = out.get_mut("buses") {
        for (j, b) in bs.iter_mut().enumerate() {
            let b = b.as_object_mut().unwrap();
            b.insert("result".into(), json!({"items": {"file": name, "counts": s.counts[j]}}));
            b.remove("uninterpreted");
            b.remove("_owned");
            b.insert("uninterpreted_summary".into(), split::unread_summary(&s.unread[j]));
        }
    }
}

/// The pieces' part documents as one (Python's `decode._join_part`).
pub(crate) fn join_part(
    outs: Vec<(Piece, Map<String, Value>)>,
    spec: &Spec,
    n: i64,
    rate: f64,
    o: Option<&Options>,
    noisy: Map<String, Value>,
    sunk: bool,
) -> Map<String, Value> {
    if outs.len() == 1 && outs[0].0.a == 0 {
        let mut o = outs.into_iter().next().unwrap().1;
        o.remove("_reanalysis");
        return o;
    }
    let o0 = &outs[0].1;
    let mut out: Map<String, Value> = o0
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "buses" | "lines" | "undeclared" | "claims" | "_reanalysis"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert("ticks".into(), json!(n));
    let buses: Vec<Value> = (0..spec.buses.len())
        .map(|j| {
            let per: Vec<(&Piece, &Map<String, Value>)> = outs
                .iter()
                .map(|(p, o)| (p, o["buses"][j].as_object().unwrap()))
                .collect();
            let order: Vec<&str> = spec.buses[j].pins.iter().map(|(_, p)| p.as_str()).collect();
            Value::Object(join_bus(&per, rate, sunk, &order))
        })
        .collect();
    out.insert("buses".into(), json!(buses));
    let mut lines = vec![];
    for (j, ln) in o0["lines"].as_array().into_iter().flatten().enumerate() {
        let mut edges = vec![];
        for (p, o) in &outs {
            for e in o["lines"][j]["edges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_i64())
            {
                let t = e + p.a;
                if p.s <= t && t < p.e {
                    edges.push(t);
                }
            }
        }
        lines.push(json!({"pin": ln["pin"], "initial": ln["initial"], "edges": edges}));
    }
    out.insert("lines".into(), json!(lines));
    let mut und: Vec<Map<String, Value>> = vec![];
    for (p, o) in &outs {
        for u in o.get("undeclared").and_then(|v| v.as_array()).into_iter().flatten() {
            let k = match und.iter().position(|x| x["pin"] == u["pin"]) {
                Some(k) => k,
                None => {
                    let mut m = Map::new();
                    m.insert("pin".into(), u["pin"].clone());
                    m.insert("edges".into(), json!(0));
                    m.insert("s".into(), Value::Null);
                    m.insert("e".into(), Value::Null);
                    m.insert("bursts".into(), json!(0));
                    und.push(m);
                    und.len() - 1
                }
            };
            let x = &mut und[k];
            let g = |v: &Value| v.as_i64().unwrap_or(0);
            x["edges"] = json!(g(&x["edges"]) + g(&u["edges"]));
            x["bursts"] = json!(g(&x["bursts"]) + g(&u["bursts"]));
            let (s, e) = (g(&u["s"]) + p.a, g(&u["e"]) + p.a);
            x["s"] = json!(x["s"].as_i64().map_or(s, |v| v.min(s)));
            x["e"] = json!(x["e"].as_i64().map_or(e, |v| v.max(e)));
        }
    }
    out.insert("undeclared".into(), json!(und));
    let mut doubtful = vec![];
    for (p, o) in &outs {
        let why = doubt_why(p, o);
        if !why.is_empty() && p.edges > 0 {
            let mut d =
                json!({"s": p.s, "e": p.e, "seconds": [p.s as f64 / rate, p.e as f64 / rate], "why": why.join("; ")});
            if let Some(h) = o.get("_reanalysis") {
                d["holds"] = h.clone();
            }
            doubtful.push(d);
        }
    }
    let mut sp = json!({"options": o.map_or(Value::Null, |o| o.doc()), "pieces": outs.iter().map(|(p, _)| p.doc()).collect::<Vec<_>>()});
    if !noisy.is_empty() {
        sp["trembling"] = Value::Object(noisy);
    }
    if !doubtful.is_empty() {
        sp["doubtful"] = json!(doubtful);
    }
    out.insert("split".into(), sp);
    for b in out
        .get_mut("buses")
        .and_then(|v| v.as_array_mut())
        .into_iter()
        .flatten()
    {
        if let Some(b) = b.as_object_mut() {
            b.remove("_owned");
        }
    }
    out
}

/// The capture as the document names it (file, sha256, id, tick, complete, lost, layout, parts).
pub fn capture_doc(path: &Path, cap: &wireskein_io::Capture) -> Result<Value, String> {
    let mut c = Map::new();
    c.insert("file".into(), json!(path.file_name().map(|s| s.to_string_lossy())));
    c.insert("sha256".into(), json!(crate::sha256_of(path)?));
    c.insert("id".into(), json!(cap.id));
    c.insert("tick_hz".into(), json!([cap.tick_hz.0, cap.tick_hz.1]));
    c.insert("complete".into(), json!(cap.complete));
    if !cap.lost.is_empty() {
        c.insert("lost".into(), json!(cap.lost));
    }
    c.insert("layout".into(), json!(cap.layout));
    c.insert("parts".into(), json!(cap.n_parts));
    Ok(Value::Object(c))
}

/// A part's keys in the document (part, tick0, start_ns ...).
fn head_of(cap: &wireskein_io::Capture, p: &wireskein_io::Part) -> Map<String, Value> {
    let mut head = Map::new();
    head.insert("part".into(), json!(p.part));
    if cap.kind == wireskein_io::Kind::WireSkein {
        if let Some(t0) = p.tick0 {
            head.insert("tick0".into(), json!(t0));
        }
    } else {
        head.insert("tick0".into(), json!(0));
    }
    head.extend(wireskein_io::wsk::part_keys(&p.meta));
    head
}

pub(crate) fn glitch_filter(meta: &Map<String, Value>) -> Option<Value> {
    meta.get("probe")
        .and_then(|p| p.get("glitch_filter_ns"))
        .filter(|v| crate::nonempty(v) && **v != json!(0) && **v != json!(false))
        .cloned()
}

/// Only these lines of a part (an error naming the pins it does not have).
fn only(p: &wireskein_io::Part, pins: Option<&[String]>) -> Result<Vec<Line>, String> {
    let Some(pins) = pins.filter(|p| !p.is_empty()) else {
        return Ok(p.lines.clone());
    };
    let names: BTreeSet<&str> = p
        .lines
        .iter()
        .map(|l| l.name.as_str())
        .chain(p.analog.iter().map(|a| a.name.as_str()))
        .chain(p.events.iter().map(|e| e.name.as_str()))
        .collect();
    let miss: Vec<&str> = pins.iter().map(|s| s.as_str()).filter(|x| !names.contains(x)).collect();
    if !miss.is_empty() {
        return Err(format!("pins not in the capture: {}", miss.join(", ")));
    }
    Ok(p.lines.iter().filter(|l| pins.contains(&l.name)).cloned().collect())
}

/// The decode document of a capture file (Python's `decode.decode_file`): every part decoded with the spec's roles;
/// a part read in pieces (split: None reads it whole), a WireSkein file's parts straight from the file a block at a
/// time when they can be. pins: only these lines read. items: (the file the items are written to, its name in the
/// document).
pub fn decode_file(
    path: &Path,
    spec: &Spec,
    split_opts: Option<&Options>,
    pins: Option<&[String]>,
    items: Option<(&Path, &str)>,
) -> Result<Value, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let kind = wireskein_io::sniff_path(path);
    if let Some((f, _)) = items {
        std::fs::write(f, b"").map_err(|e| format!("{}: {e}", f.display()))?; // written part after part
    }
    let mut parts = vec![];
    // a WireSkein file whose parts are each read a block at a time
    if split_opts.is_some() && kind == Some(wireskein_io::Kind::WireSkein) && !cfg!(target_family = "wasm") {
        let f = std::fs::File::open(path).map_err(|e| at(e.to_string()))?;
        let (cap, _) = wireskein_io::wsk::head_from(f).map_err(at)?;
        let follow = cap.layout == "timeline"
            && cap
                .parts
                .windows(2)
                .any(|w| w[0].tick0.unwrap_or(0) + w[0].ticks >= w[1].tick0.unwrap_or(0));
        let mut srcs = vec![];
        if !follow {
            for p in &cap.parts {
                match wireskein_io::stream::file_source_of(path, p.part, pins).map_err(at)? {
                    Some(s) => srcs.push(s),
                    None => break,
                }
            }
        }
        if !follow && srcs.len() == cap.parts.len() {
            let doc_cap = capture_doc(path, &cap)?;
            for (p, mut src) in cap.parts.iter().zip(srcs) {
                let head = head_of(&cap, p);
                parts.push(Value::Object(part_split(
                    &mut src,
                    cap.rate(),
                    spec,
                    &head,
                    split_opts,
                    items,
                    glitch_filter(&p.meta).as_ref(),
                )?));
            }
            return Ok(
                json!({"format": crate::FORMAT, "wireskein": env!("CARGO_PKG_VERSION"), "spec": spec.doc(),
                             "capture": doc_cap, "parts": parts}),
            );
        }
    }
    let cap = wireskein_io::read(path)?;
    let rate = cap.rate();
    for p in &cap.parts {
        let lines = only(p, pins)?;
        let head = head_of(&cap, p);
        let gf = glitch_filter(&p.meta);
        let mut out = if split_opts.is_some() || items.is_some() {
            let mut src = wireskein_io::stream::MemSource::new(&lines, p.ticks);
            part_split(&mut src, rate, spec, &head, split_opts, items, gf.as_ref())?
        } else {
            part(&lines, rate, p.ticks, spec, &head, false, gf.as_ref())?
        };
        if let Some((a, b)) = p.parts {
            out.insert("parts".into(), json!([a, b]));
        }
        parts.push(Value::Object(out));
    }
    Ok(
        json!({"format": crate::FORMAT, "wireskein": env!("CARGO_PKG_VERSION"), "spec": spec.doc(),
              "capture": capture_doc(path, &cap)?, "parts": parts}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_as_python_makes_it() {
        let s = Spec::parse(
            r#"{"buses": [{"protocol": "spi", "pins": {"clk": "C", "mosi": "D", "cs": "S0"}},
                          {"protocol": "spi", "pins": {"clk": "C", "mosi": "D", "cs": "S1"}},
                          {"protocol": "swio", "pins": {"dio": "C"}}], "lines": ["X"]}"#,
        )
        .unwrap();
        assert_eq!(s.depth, "device");
        assert_eq!(s.buses[0].pins[2], ("cs".to_string(), "S0".to_string()));
        // a pin under two protocols: read apart; the targets of one SPI bus together, sharing lines
        assert_eq!(apart(&s.buses), vec![vec![0, 1], vec![2]]);
        assert!(shares_lines(&[&s.buses[0], &s.buses[1]]) && !shares_lines(&[&s.buses[2]]));
        assert!(Spec::of(&json!({"buses": [{"protocol": "spi"}]})).is_err());
        assert!(Spec::of(&json!({"depth": "bits"})).unwrap_err().starts_with("depth"));
    }

    #[test]
    fn a_bus_on_no_reading_is_all_unread() {
        // a line that toggles in two bursts, no protocol read from it
        let l = |name: &str, e: Vec<i64>| Line {
            name: name.into(),
            initial: 0,
            edges: e,
            step: 1,
            phase: 0,
            reduction: None,
            acquisition: Default::default(),
        };
        let lines = vec![l("A", vec![10, 20, 30, 5000, 5010]), l("B", vec![100])];
        let spec = Spec::of(&json!({"buses": [{"protocol": "uart", "pins": {"data": "A"}}], "lines": ["B"]})).unwrap();
        let o = part(&lines, 1e6, 10_000, &spec, &Map::new(), false, None).unwrap();
        let b = &o["buses"][0];
        assert_eq!(b["decoded"], json!(false));
        assert_eq!(
            b["uninterpreted"],
            json!([{"s": 10, "e": 30, "edges": 3}, {"s": 5000, "e": 5010, "edges": 2}])
        );
        assert_eq!(o["lines"], json!([{"pin": "B", "initial": 0, "edges": [100]}]));
        assert_eq!(o["undeclared"], json!([]));
    }
}
