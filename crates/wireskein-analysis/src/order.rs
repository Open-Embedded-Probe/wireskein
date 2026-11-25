//! The documents' keys in the order Python writes them (`decode.analyze_file`, `decode_file`, `export.export`): the
//! documents are built here as serde_json's values, whose maps keep their keys sorted, and turned into `J` (dicts in
//! their keys' order) on the way out. The dicts this crate makes are put in Python's order kind by kind; what the core
//! wrote (a claim's layers, items, params, evidence) keeps the order it has (the core writes it through serde_json:
//! sorted, as Python reads it from the core); the keys this crate adds to the core's items (a frame's clock) go after
//! them, as Python adds them. Not wireskein-core's: its output is left as it is.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use wireskein_io::j::J;
use wireskein_io::write::j_of;

/// The orders the core writes its dicts' keys in (not always sorted: some of its layers are written by hand), by
/// their sorted keys: learned from each claim's layers as they are read (`layers`), shared by every document.
static CORE_ORDERS: Mutex<Option<HashMap<Vec<String>, Vec<String>>>> = Mutex::new(None);

/// A claim's layers as the core wrote them, read into serde_json's map; the order of each dict's keys learned.
pub fn layers(raw: &serde_json::value::RawValue) -> Map<String, Value> {
    if let Ok(j) = J::parse(raw.get()) {
        let mut seen: Vec<(Vec<String>, Vec<String>)> = vec![];
        learn(&j, &mut seen);
        if let Ok(mut t) = CORE_ORDERS.lock() {
            let t = t.get_or_insert_with(HashMap::new);
            for (k, v) in seen {
                t.entry(k).or_insert(v);
            }
        }
    }
    serde_json::from_str(raw.get()).unwrap_or_default()
}

fn learn(j: &J, out: &mut Vec<(Vec<String>, Vec<String>)>) {
    match j {
        J::Dict(kv) => {
            let order: Vec<String> = kv.iter().map(|(k, _)| k.clone()).collect();
            let mut key = order.clone();
            key.sort();
            if key != order && !out.iter().any(|(k, _)| *k == key) {
                out.push((key, order));
            }
            for (_, v) in kv {
                learn(v, out);
            }
        }
        // (a list's items are alike: its first few tell their orders)
        J::List(v) => v.iter().take(8).for_each(|x| learn(x, out)),
        _ => {}
    }
}

/// The keys analyze adds to a clocked bus's frames (`decode.frame_clocks`, after the core's): its clock's period
/// (where the core gives none: an RVSWD frame has its own) and high.
pub fn frame_added(protocol: &str) -> &'static [&'static str] {
    if protocol == "rvswd" {
        &FRAME_ADDED[1..]
    } else {
        FRAME_ADDED
    }
}

/// A core dict's keys in the order the core writes them (sorted where it was never seen otherwise); the keys analyze
/// adds (`added`, where the core's own order has them not) after the core's, in that order.
fn core_order(kv: Vec<(String, J)>, added: &[&str]) -> Vec<(String, J)> {
    let t = CORE_ORDERS.lock().ok();
    let t = t.as_ref().and_then(|t| t.as_ref());
    let keys: Vec<String> = kv.iter().map(|(k, _)| k.clone()).collect(); // (sorted: serde_json's)
    let order = t.and_then(|t| t.get(&keys)).cloned().or_else(|| {
        let base: Vec<String> = keys.iter().filter(|k| !added.contains(&k.as_str())).cloned().collect();
        let mut o = t.and_then(|t| t.get(&base)).cloned().unwrap_or(base);
        o.extend(
            added
                .iter()
                .filter(|a| keys.iter().any(|k| k == *a))
                .map(|a| a.to_string()),
        );
        Some(o)
    });
    let Some(order) = order else { return kv };
    let at = |k: &str| order.iter().position(|x| x == k).unwrap_or(order.len());
    let mut v = kv;
    v.sort_by_key(|(k, _)| at(k));
    v
}

/// A value the core wrote, each dict in the core's order (`added`: what analyze adds to its dicts).
pub fn core_j(v: &Value, added: &[&str]) -> J {
    match v {
        Value::Array(a) => J::List(a.iter().map(|x| core_j(x, added)).collect()),
        Value::Object(o) => J::Dict(core_order(
            o.iter().map(|(k, x)| (k.clone(), core_j(x, added))).collect(),
            added,
        )),
        x => j_of(x),
    }
}

const DOC: &[&str] = &[
    "format",
    "wireskein",
    "hints",
    "spec",
    "capture",
    "parts",
    "analysis_seconds",
];
const CAPTURE: &[&str] = &["file", "sha256", "id", "tick_hz", "complete", "lost", "layout", "parts"];
const PART: &[&str] = &[
    "part",
    "ticks",
    "parts",
    "tick0",
    "start_ns",
    "start_uncertainty_ns",
    "boot_id",
    "buses",
    "lines",
    "undeclared",
    "claims",
    "split",
    "window",
    "relations",
    "segment",
    "events",
    "skipped_channels",
];
const CLAIM: &[&str] = &[
    "protocol",
    "roles",
    "verdict",
    "score",
    "margin",
    "params",
    "bus",
    "result",
    "devices",
    "layers",
    "evidence",
    "runner_up",
    "reading",
    "clock_speeds",
    "undersampled",
    "fit_pieces",
    "uninterpreted",
    "uninterpreted_summary",
];
/// a bus of a decode document read whole (`decode._bus`, `_shared`)
const BUS: &[&str] = &[
    "protocol",
    "pins",
    "decoded",
    "params",
    "fit",
    "result",
    "devices",
    "roles_unknown",
    "uninterpreted",
    "empty_selections",
    "idle_ticks",
    "clock_speeds",
    "undersampled",
    "reading",
    "shared",
    "uninterpreted_summary",
];
/// a bus joined from the pieces of a long part (`decode._join_bus`)
const BUS_JOINED: &[&str] = &[
    "protocol",
    "pins",
    "decoded",
    "params",
    "devices",
    "roles_unknown",
    "empty_selections",
    "idle_ticks",
    "shared",
    "fit",
    "result",
    "clock_speeds",
    "undersampled",
    "uninterpreted",
    "reading",
    "uninterpreted_summary",
];
const FIT: &[&str] = &["score", "claimed", "verdict", "pieces"];
/// a device pack's match beside a claim or bus at depth device (`export.devices_of`)
const DEVICE: &[&str] = &["device", "level", "evidence", "messages"];
/// its evidence (`devices._i2c_command_table`, `_i2c_register_map`, `match_spi`, the candidates of an address,
/// `_merge_indistinguishable`'s packs)
const DEVICE_EVIDENCE: &[&str] = &[
    "known_commands",
    "unknown_commands",
    "crc_passed",
    "crc_failed",
    "length_consistent",
    "identity",
    "command_pin",
    "identity_hits",
    "identity_misses",
    "accesses",
    "address",
    "chips",
    "pack",
    "indistinguishable",
];
/// its messages (the same, a transaction's or an SPI frame's)
const DEVICE_MESSAGE: &[&str] = &[
    "s",
    "frame",
    "command",
    "known",
    "response_to",
    "bytes",
    "crc_ok",
    "write",
    "read",
    "value",
    "length_ok",
    "address",
    "jedec_id",
    "identity",
];
/// `decode.reading` (with `noise`, `_shares`, `line_faults`)
const READING: &[&str] = &[
    "frames",
    "ok",
    "unanswered",
    "restored",
    "repaired",
    "rate_ok",
    "unread",
    "unread_edges",
    "empty_selections",
    "glitches",
    "clock_edges",
    "ringing",
    "data_edges",
    "glitch_edges",
    "ringing_edges",
    "glitch_share",
    "ringing_share",
    "noisy",
    "line_changes",
    "stopped",
    "oscillating",
    "glitch_filter_ns",
    "same_sample",
    "shared",
    "shared_edges",
];
/// a joined bus's reading (`decode._join_bus`)
const READING_JOINED: &[&str] = &[
    "frames",
    "ok",
    "unanswered",
    "restored",
    "repaired",
    "rate_ok",
    "unread",
    "unread_edges",
    "empty_selections",
    "same_sample",
    "glitches",
    "glitch_edges",
    "clock_edges",
    "ringing",
    "ringing_edges",
    "data_edges",
    "glitch_share",
    "ringing_share",
    "noisy",
    "line_changes",
    "stopped",
    "oscillating",
    "glitch_filter_ns",
    "shared",
    "shared_edges",
];
const SPEED: &[&str] = &[
    "period_ns",
    "freq_hz",
    "high_ns",
    "low_ns",
    "frames",
    "s",
    "e",
    "rate_ok",
];
const UNDERSAMPLED: &[&str] = &[
    "clock",
    "rate_hz",
    "frames",
    "period_ns",
    "high_ns",
    "low_ns",
    "least_rate_hz",
    "best_rate_hz",
    "restored",
];
const SPAN: &[&str] = &[
    "line",
    "pin",
    "s",
    "e",
    "edges",
    "tick",
    "others_after",
    "changes",
    "bursts",
    "bus",
];
const LINE: &[&str] = &["pin", "initial", "edges"];
const CHANGES: &[&str] = &["changes", "last"];
const SPLIT: &[&str] = &[
    "options",
    "pieces",
    "trembling",
    "doubtful",
    "roles_from",
    "no_roles",
    "not_carried",
];
const OPTIONS: &[&str] = &[
    "max_edges",
    "search_from",
    "gap_min",
    "similar",
    "min_edges",
    "overlap_edges",
    "noise_max",
    "noise_share",
    "workers",
    "roles_from",
    "reanalyze",
    "cuts",
    "watch",
    "ignore",
];
const PIECE: &[&str] = &["s", "e", "edges", "cut", "ratio", "read"];
const DOUBTFUL: &[&str] = &["s", "e", "seconds", "why", "holds"];
const NOT_CARRIED: &[&str] = &["protocol", "roles", "verdict"];
const HOLDS: &[&str] = &["protocol", "roles", "verdict", "score", "params"];
const UNDECLARED: &[&str] = &["pin", "edges", "s", "e", "bursts"];
const SUMMARY: &[&str] = &["count", "edges", "sizes", "largest"];
const ROLES_UNKNOWN: &[&str] = &["given", "roles"];
const SEGMENT: &[&str] = &["path", "raw", "issues"];
const EVENT: &[&str] = &["s", "pin", "kind", "text"];
const ITEMS: &[&str] = &["file", "counts"];
/// the keys analyze adds to a clocked bus's frames (`decode.frame_clocks`), after the core's
const FRAME_ADDED: &[&str] = &["period_ns", "high_ns"];
/// a Debug Module access (RVSWD's, SWIO's dm layer), as Python's upper stage and items file write it
pub const DM: &[&str] = &["s", "e", "op", "addr", "value", "size"];

#[derive(Clone, Copy, PartialEq)]
enum K {
    /// a dict of this crate's, its keys in this order (any other after them, as they are)
    Own(&'static [&'static str]),
    /// a dict keyed by lines: in the order of the claim's or bus's lines
    Lines(&'static [&'static str]),
    /// what the core wrote, as it is
    Core,
    /// a layer's items (dict of lists or a list): the core's keys first, the frame's clock after
    Layers,
}

struct Cx<'a> {
    joined: bool,
    /// the claim's or bus's protocol
    protocol: String,
    lines: Vec<String>,
    /// a decode spec's buses' pins (role names), in the spec's order
    spec_pins: &'a [Vec<String>],
}

fn ordered(keys: Vec<(String, J)>, order: &[&str]) -> Vec<(String, J)> {
    let at = |k: &str| order.iter().position(|x| *x == k).unwrap_or(order.len());
    let mut v = keys;
    v.sort_by_key(|(k, _)| at(k)); // (stable: the others keep their order)
    v
}

fn child(parent: &'static [&'static str], key: &str, cx: &Cx) -> K {
    match (parent, key) {
        (_, "capture") if parent == DOC => K::Own(CAPTURE),
        (_, "parts") if parent == DOC => K::Own(PART),
        (_, "hints" | "spec") if parent == DOC => K::Core,
        (_, "claims") if parent == PART => K::Own(CLAIM),
        (_, "buses") if parent == PART => K::Own(if cx.joined { BUS_JOINED } else { BUS }),
        (_, "lines") if parent == PART => K::Own(LINE),
        (_, "undeclared") if parent == PART => K::Own(UNDECLARED),
        (_, "split") if parent == PART => K::Own(SPLIT),
        (_, "segment") if parent == PART => K::Own(SEGMENT),
        (_, "events") if parent == PART => K::Own(EVENT),
        (_, "options") if parent == SPLIT => K::Own(OPTIONS),
        (_, "pieces") if parent == SPLIT => K::Own(PIECE),
        (_, "doubtful") if parent == SPLIT => K::Own(DOUBTFUL),
        (_, "not_carried") if parent == SPLIT => K::Own(NOT_CARRIED),
        (_, "holds") if parent == DOUBTFUL => K::Own(HOLDS),
        (_, "devices") if parent == CLAIM || parent == BUS || parent == BUS_JOINED => K::Own(DEVICE),
        (_, "evidence") if parent == DEVICE => K::Own(DEVICE_EVIDENCE),
        (_, "messages") if parent == DEVICE => K::Own(DEVICE_MESSAGE),
        (_, "fit") => K::Own(FIT),
        (_, "reading") => K::Own(if cx.joined { READING_JOINED } else { READING }),
        (_, "clock_speeds") => K::Own(SPEED),
        (_, "undersampled") => K::Own(UNDERSAMPLED),
        (_, "uninterpreted" | "shared" | "empty_selections" | "stopped" | "oscillating") => K::Own(SPAN),
        (_, "uninterpreted_summary") => K::Own(SUMMARY),
        (_, "roles_unknown") => K::Own(ROLES_UNKNOWN),
        (_, "ringing" | "data_edges" | "ringing_edges" | "ringing_share")
            if parent == READING || parent == READING_JOINED =>
        {
            K::Lines(&[])
        }
        (_, "line_changes") => K::Lines(CHANGES),
        (_, "result" | "layers") => K::Layers,
        _ => K::Core,
    }
}

fn walk(v: &Value, k: K, cx: &Cx) -> J {
    match (v, k) {
        (Value::Array(a), _) => J::List(a.iter().map(|x| walk(x, k, cx)).collect()),
        (Value::Object(o), K::Own(order)) => {
            let mut lines = cx.lines.clone();
            let mut protocol = cx.protocol.clone();
            if order == CLAIM || order == BUS || order == BUS_JOINED {
                let who = o.get("roles").or_else(|| o.get("pins"));
                let mut rp: Vec<(String, String)> = who
                    .and_then(|r| r.as_object())
                    .map(|r| {
                        r.iter()
                            .filter_map(|(k, p)| p.as_str().map(|p| (k.clone(), p.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(roles) = spec_order(cx.spec_pins, rp.iter().map(|(k, _)| k.as_str())) {
                    let at = |k: &str| roles.iter().position(|x| x == k).unwrap_or(roles.len());
                    rp.sort_by_key(|(k, _)| at(k));
                }
                lines = rp.into_iter().map(|(_, p)| p).collect();
                protocol = o.get("protocol").and_then(|p| p.as_str()).unwrap_or("").to_string();
            }
            let sub = Cx {
                joined: cx.joined || (order == PART && o.contains_key("split")),
                lines,
                protocol,
                spec_pins: cx.spec_pins,
            };
            let kv: Vec<(String, J)> = o
                .iter()
                .map(|(key, x)| (key.clone(), walk(x, child(order, key, &sub), &sub)))
                .collect();
            J::Dict(ordered(kv, order))
        }
        (Value::Object(o), K::Lines(inner)) => {
            let at = |p: &str| cx.lines.iter().position(|x| x == p).unwrap_or(cx.lines.len());
            let mut kv: Vec<(String, J)> = o
                .iter()
                .map(|(key, x)| (key.clone(), walk(x, K::Own(inner), cx)))
                .collect();
            kv.sort_by_key(|(key, _)| at(key));
            J::Dict(kv)
        }
        (Value::Object(o), K::Layers) => {
            // the layers (their names in the core's order), each a list of items or columns
            let added = frame_added(&cx.protocol);
            J::Dict(core_order(
                o.iter().map(|(key, x)| (key.clone(), layer(key, x, added))).collect(),
                &[],
            ))
        }
        _ => core_j(v, &[]),
    }
}

/// A layer: its items as the core wrote them (the frame's clock analyze adds after), or a file's items.
fn layer(name: &str, v: &Value, added: &[&str]) -> J {
    match v {
        Value::Object(o) if o.contains_key("file") && o.contains_key("counts") => J::Dict(ordered(
            o.iter().map(|(k, x)| (k.clone(), core_j(x, &[]))).collect(),
            ITEMS,
        )),
        // the Debug Module's accesses as Python's upper stage lists them
        Value::Array(a) if name == "dm" => J::List(
            a.iter()
                .map(|x| match core_j(x, &[]) {
                    J::Dict(kv) => J::Dict(ordered(kv, DM)),
                    y => y,
                })
                .collect(),
        ),
        x => core_j(x, added),
    }
}

/// An analysis document (`analyze_file`'s) in Python's order; `hints` as the caller gave them (their own order).
pub fn analysis(doc: &Value, hints: Option<&J>) -> J {
    let cx = Cx {
        joined: false,
        protocol: String::new(),
        lines: vec![],
        spec_pins: &[],
    };
    let mut out = walk(doc, K::Own(DOC), &cx);
    if let Some(h) = hints {
        out.set("hints", h.clone());
    }
    out
}

/// A decode document (`decode_file`'s) in Python's order: each bus's pins in its spec's order; `spec` the
/// normalized spec as the document gives it, in its own order.
pub fn decoding(doc: &Value, spec: Option<&J>, spec_pins: &[Vec<String>]) -> J {
    let cx = Cx {
        joined: false,
        protocol: String::new(),
        lines: vec![],
        spec_pins,
    };
    let mut out = walk(doc, K::Own(DOC), &cx);
    if let Some(s) = spec {
        out.set("spec", s.clone());
    }
    // each bus's pins as its spec named them
    if let Some(J::List(parts)) = out.get_mut("parts") {
        for p in parts {
            if let Some(J::List(buses)) = p.get_mut("buses") {
                for b in buses.iter_mut() {
                    if let Some(J::Dict(pins)) = b.get_mut("pins")
                        && let Some(order) = spec_order(spec_pins, pins.iter().map(|(k, _)| k.as_str()))
                    {
                        let at = |k: &str| order.iter().position(|x| x == k).unwrap_or(order.len());
                        pins.sort_by_key(|(k, _)| at(k));
                    }
                }
            }
        }
    }
    out
}

/// The roles of a spec's bus with these roles, in the spec's order.
fn spec_order<'a, 'b>(spec_pins: &'a [Vec<String>], roles: impl Iterator<Item = &'b str>) -> Option<&'a Vec<String>> {
    let mut want: Vec<&str> = roles.collect();
    want.sort();
    spec_pins.iter().find(|s| {
        let mut have: Vec<&str> = s.iter().map(|x| x.as_str()).collect();
        have.sort();
        have == want
    })
}
