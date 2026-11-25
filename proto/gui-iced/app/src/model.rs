//! The capture as the screens see it: digital lines (their changes and summaries, built in the background), analog
//! lines, and the analysis (claims, their layers as annotation tracks and list rows), run in the background.
//!
//! (Prototype 3: a copy of prototype 1's proto/gui/egui/src/model.rs, its repaint requests taken out: the screen polls.)
#![allow(dead_code)]

use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use web_time::Instant;
use view_core::Tick;
use view_core::analog::AnalogLine;
use view_core::ann::{Item, Status, Track};
use view_core::digital::{EdgeSource, Lod, VecEdges, build_lod_shared, shift_for};
use view_core::synth;

pub struct DLine {
    pub name: String,
    pub src: Arc<dyn EdgeSource>,
    pub lod: Arc<RwLock<Lod>>,
}

pub struct Capture {
    pub title: String,
    pub path: Option<PathBuf>,
    pub tick_hz: f64,
    pub n_ticks: Tick,
    pub lines: Vec<DLine>,
    pub analog: Vec<AnalogLine>,
    pub part: usize,
    pub n_parts: usize,
    pub lost: Vec<Value>,
    pub complete: bool,
    pub total_edges: u64,
    pub opened: Instant,
    pub read_s: f64,
    /// summaries finished (lines)
    pub lod_done: Arc<AtomicUsize>,
    pub lod_s: Arc<Mutex<Option<f64>>>,
    pub cancel: Arc<AtomicBool>,
}

impl Capture {
    pub fn secs(&self, t: f64) -> f64 {
        t / self.tick_hz
    }
    pub fn mem_bytes(&self) -> usize {
        use view_core::MemSize;
        self.lines.iter().map(|l| l.src.mem_bytes() + l.lod.read().unwrap().mem_bytes()).sum::<usize>()
            + self.analog.iter().map(|a| a.mem_bytes()).sum::<usize>()
    }
    /// The summaries of every line built in the background (`threads` at a time).
    fn start_lods(&self, threads: usize, per_bucket: u32) {
        let jobs: Vec<(Arc<dyn EdgeSource>, Arc<RwLock<Lod>>)> =
            self.lines.iter().map(|l| (l.src.clone(), l.lod.clone())).collect();
        let jobs = Arc::new(Mutex::new(jobs.into_iter().enumerate().collect::<Vec<_>>()));
        let started = Instant::now();
        let n_lines = self.lines.len();
        for _ in 0..threads.max(1) {
            let jobs = jobs.clone();
            let done = self.lod_done.clone();
            let lod_s = self.lod_s.clone();
            let cancel = self.cancel.clone();
            spawn(move || {
                loop {
                    let job = jobs.lock().unwrap().pop();
                    let Some((_, (src, lod))) = job else { break };
                    let s0 = shift_for(src.n_ticks(), src.approx_edges(), per_bucket);
                    build_lod_shared(src.as_ref(), s0, &lod, &cancel);
                    if done.fetch_add(1, Ordering::SeqCst) + 1 == n_lines {
                        *lod_s.lock().unwrap() = Some(started.elapsed().as_secs_f64());
                    }
                }
            });
        }
    }
}

fn dline(name: &str, src: Arc<dyn EdgeSource>) -> DLine {
    let lod = Arc::new(RwLock::new(Lod::new(src.n_ticks(), src.initial(), 0)));
    DLine { name: name.into(), src, lod }
}

/// Work in the background (native: a thread; the web: at once, there are no threads here).
pub fn spawn(f: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(f);
    #[cfg(target_arch = "wasm32")]
    f();
}

/// A capture file opened (one part); its summaries started.
pub fn open_file(path: &PathBuf, part: usize) -> Result<Capture, String> {
    let opened = Instant::now();
    let cap = wireskein_io::read(path)?;
    let title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    open_capture(cap, title, Some(path.clone()), part, opened)
}

/// A capture file's bytes opened (the web build: an example carried in the program).
pub fn open_bytes(name: &str, bytes: &[u8], part: usize) -> Result<Capture, String> {
    let opened = Instant::now();
    let cap = wireskein_io::read_bytes(bytes)?;
    open_capture(cap, name.into(), None, part, opened)
}

fn open_capture(
    cap: wireskein_io::Capture,
    title: String,
    path: Option<PathBuf>,
    part: usize,
    opened: Instant,
) -> Result<Capture, String> {
    let read_s = opened.elapsed().as_secs_f64();
    let n_parts = cap.parts.len();
    let mut p = cap.parts.into_iter().nth(part).ok_or(format!("part {part} of {n_parts}"))?;
    let tick_hz = cap.tick_hz.0 as f64 / cap.tick_hz.1 as f64;
    let n = p.ticks;
    let mut lines = vec![];
    let mut total = 0;
    for l in std::mem::take(&mut p.lines) {
        total += l.edges.len() as u64;
        lines.push(dline(&l.name, Arc::new(VecEdges::new(l.initial, n, l.edges))));
    }
    let mut analog = vec![];
    for a in &p.analog {
        let vals: Vec<f32> = a.volts().unwrap_or_else(|| a.values.clone()).iter().map(|&v| v as f32).collect();
        let rate = a.rate_hz.0 as f64 / a.rate_hz.1 as f64;
        let t0 = a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64;
        analog.push(AnalogLine::new(&a.name, if a.volts().is_some() { "V" } else { &a.unit }, t0, tick_hz / rate, vals));
    }
    let c = Capture {
        title,
        path,
        tick_hz,
        n_ticks: n,
        lines,
        analog,
        part,
        n_parts,
        lost: cap.lost,
        complete: cap.complete,
        total_edges: total,
        opened,
        read_s,
        lod_done: Arc::new(AtomicUsize::new(0)),
        lod_s: Arc::new(Mutex::new(None)),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    c.start_lods(2, 32);
    Ok(c)
}

/// A made-up capture of `total` changes over 16 lines (made on the fly) and an analog line.
pub fn open_synth(total: u64) -> Capture {
    let opened = Instant::now();
    let (n, ls) = synth::synth_lines(total, 16);
    let lines: Vec<DLine> =
        ls.into_iter().enumerate().map(|(i, l)| dline(&format!("D{i}"), Arc::new(l) as Arc<dyn EdgeSource>)).collect();
    let n_s = 4_000_000usize;
    let dt = n as f64 / n_s as f64;
    let analog = vec![AnalogLine::new("A0", "V", 0.0, dt, synth::synth_analog(n_s, dt))];
    let c = Capture {
        title: format!("synth {total:.0e}"),
        path: None,
        tick_hz: 1e9,
        n_ticks: n,
        lines,
        analog,
        part: 0,
        n_parts: 1,
        lost: vec![],
        complete: true,
        total_edges: total,
        opened,
        read_s: opened.elapsed().as_secs_f64(),
        lod_done: Arc::new(AtomicUsize::new(0)),
        lod_s: Arc::new(Mutex::new(None)),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    c.start_lods(2, 32);
    c
}

// ---------------------------------------------------------------------------------------------------------------
// the analysis

/// A layer's rows in the track's order (row i: track item i): the document's items, or compact ones read here
/// (millions of bits as JSON objects took more than 4 GB).
pub enum Rows {
    Json(Vec<Value>),
    Bits(Vec<u8>),
    /// a byte and its ACK (0: none, 1: ACK, 2: NACK)
    Bytes(Vec<(u8, u8)>),
}

impl Layer {
    pub fn len(&self) -> usize {
        match &self.rows {
            Rows::Json(v) => v.len(),
            Rows::Bits(v) => v.len(),
            Rows::Bytes(v) => v.len(),
        }
    }
    /// Row i as the document would give it (built for the compact ones).
    pub fn value(&self, i: usize) -> std::borrow::Cow<'_, Value> {
        use std::borrow::Cow;
        match &self.rows {
            Rows::Json(v) => Cow::Borrowed(&v[i]),
            Rows::Bits(v) => Cow::Owned(serde_json::json!({"bit": v[i]})),
            Rows::Bytes(v) => {
                let (b, a) = v[i];
                let mut o = serde_json::json!({"byte": format!("0x{b:02x}")});
                if a > 0 {
                    o["ack"] = Value::Bool(a == 1);
                }
                Cow::Owned(o)
            }
        }
    }
}

pub struct Layer {
    pub name: String,
    pub rows: Rows,
    pub track: Track,
    /// the columns the rows have (scalar keys, first seen first)
    pub columns: Vec<String>,
}

pub struct Claim {
    pub protocol: String,
    pub verdict: String,
    pub score: f64,
    pub roles: Vec<(String, String)>,
    /// the line the annotations go under (its data line)
    pub anchor: Option<usize>,
    /// layers coarse first (transactions, frames, bits): at most 3 shown
    pub layers: Vec<Layer>,
    pub unread: Option<Track>,
    pub bus: Option<String>,
}

pub struct Analysis {
    pub claims: Vec<Claim>,
    pub seconds: f64,
    pub error: Option<String>,
    pub fake: bool,
}

pub fn verdict_status(v: &str) -> Status {
    match v {
        "confirmed" => Status::Ok,
        "likely" => Status::Note,
        _ => Status::Warn,
    }
}

fn status_of(it: &Value) -> Status {
    let f = |k: &str| it.get(k);
    if f("ok") == Some(&Value::Bool(false)) || f("checksum_ok") == Some(&Value::Bool(false)) || f("error").is_some_and(|e| !e.is_null()) {
        return Status::Error;
    }
    if f("complete") == Some(&Value::Bool(false))
        || f("restored").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        || f("repaired").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        return Status::Warn;
    }
    if f("addr_ack") == Some(&Value::Bool(false)) || f("ack") == Some(&Value::Bool(false)) || f("acked") == Some(&Value::Bool(false)) {
        return Status::Note;
    }
    Status::Ok
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".into()),
        _ => None,
    }
}

/// The text of an item: a bus transaction as a viewer writes it, else its fields.
pub fn item_text(it: &Value) -> (String, String) {
    if let (Some(addr), Some(rw)) = (it.get("addr"), it.get("rw").and_then(|v| v.as_str())) {
        let rw = rw.chars().next().unwrap_or('?').to_ascii_uppercase();
        let a = addr.as_i64().map(|a| format!("0x{a:02x}")).unwrap_or("?".into());
        let mut s = format!("{rw} {a}");
        if let Some(b) = it.get("bytes").and_then(|b| b.as_str()).filter(|b| !b.is_empty()) {
            let pairs: Vec<&str> = (0..b.len() / 2).map(|i| &b[2 * i..2 * i + 2]).collect();
            s += " ";
            s += &pairs.join(" ");
        }
        if it.get("addr_ack") == Some(&Value::Bool(false)) {
            s += " NACK";
        }
        let short = format!("{rw}{}", a.trim_start_matches("0x"));
        return (s, short);
    }
    if let Some(b) = it.get("byte").and_then(|b| b.as_str()) {
        let ack = match it.get("ack") {
            Some(Value::Bool(true)) => " A",
            Some(Value::Bool(false)) => " N",
            _ => "",
        };
        return (format!("{b}{ack}"), b.trim_start_matches("0x").to_string());
    }
    if let Some(b) = it.get("bit") {
        return (b.to_string(), b.to_string());
    }
    let mut parts = vec![];
    let mut short = String::new();
    if let Some(o) = it.as_object() {
        for (k, v) in o {
            if matches!(k.as_str(), "s" | "e") {
                continue;
            }
            if let Some(x) = scalar(v) {
                if short.is_empty() {
                    short = x.chars().take(8).collect();
                }
                parts.push(format!("{k}={x}"));
            }
            if parts.len() >= 6 {
                break;
            }
        }
    }
    (parts.join(" "), short)
}

/// A layer's items as rows: an array of objects, or an object of columns ({"s": [...], ...}).
fn rows_of(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) if a.first().is_some_and(|x| x.get("s").is_some_and(|s| s.is_i64() || s.is_f64())) => Some(a.clone()),
        Value::Object(o) if o.get("s").is_some_and(|x| x.is_array()) => {
            let s = o["s"].as_array().unwrap();
            let n = s.len();
            let mut rows = vec![];
            for i in 0..n {
                let mut r = serde_json::Map::new();
                for (k, vv) in o {
                    match vv {
                        Value::Array(a) if a.len() == n => {
                            r.insert(k.clone(), a[i].clone());
                        }
                        Value::String(h) if k == "hex" && h.len() >= 2 * n => {
                            r.insert("hex".into(), Value::String(h[2 * i..2 * i + 2].into()));
                        }
                        _ => {}
                    }
                }
                rows.push(Value::Object(r));
            }
            Some(rows)
        }
        _ => None,
    }
}

fn layer_of(name: &str, items: Vec<Value>) -> Layer {
    let mut columns: Vec<String> = vec![];
    for it in items.iter().take(2000) {
        if let Some(o) = it.as_object() {
            for (k, v) in o {
                if matches!(k.as_str(), "s" | "e") || scalar(v).is_none() {
                    continue;
                }
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    // each item's end: its own, or the next one's start (items without ends: a mark of at most that)
    let starts: Vec<i64> = items.iter().map(|x| x["s"].as_f64().unwrap_or(0.0) as i64).collect();
    let mut tr = vec![];
    for (i, it) in items.iter().enumerate() {
        let s = starts[i];
        let e = it.get("e").and_then(|e| e.as_f64()).map(|e| e as i64 + 1).unwrap_or_else(|| {
            let next = starts.get(i + 1).copied().unwrap_or(s + 1);
            if next > s { next } else { s + 1 }
        });
        let (full, short) = item_text(it);
        tr.push((Item { s, e, status: status_of(it), id: i as u32 }, full, short));
    }
    let track = Track::new(name, tr);
    let mut items: Vec<Option<Value>> = items.into_iter().map(Some).collect();
    let rows = track.items().iter().map(|it| items[it.id as usize].take().unwrap_or(Value::Null)).collect();
    Layer { name: name.into(), rows: Rows::Json(rows), track, columns }
}

const DATA_ROLES: [&str; 9] = ["sda", "data", "dio", "mosi", "miso", "rx", "tx", "swdio", "cmd"];

/// The analysis document read into claims (part `part`).
pub fn analysis_of(doc: &Value, part: usize, line_names: &[String]) -> Vec<Claim> {
    let mut out = vec![];
    let parts = doc["parts"].as_array().cloned().unwrap_or_default();
    let Some(p) = parts.iter().find(|p| p["part"].as_u64() == Some(part as u64)).or(parts.first()) else {
        return out;
    };
    for c in p["claims"].as_array().cloned().unwrap_or_default() {
        let roles: Vec<(String, String)> = c["roles"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), scalar(v).unwrap_or_default())).collect())
            .unwrap_or_default();
        let anchor_name = roles
            .iter()
            .find(|(r, _)| DATA_ROLES.contains(&r.as_str()))
            .or(roles.last())
            .map(|(_, n)| n.clone());
        let anchor = anchor_name.and_then(|n| line_names.iter().position(|x| *x == n));
        let mut layers: Vec<Layer> = vec![];
        // "all" gives the layers; a capture read in pieces gives each claim's final layer as its result
        if let Some(o) = c["layers"].as_object().or(c["result"].as_object()) {
            for (k, v) in o {
                if std::env::var("WSK_DUMP").is_ok() {
                    let s = v.to_string();
                    eprintln!("{} layer {k}: {}", c["protocol"], &s[..s.len().min(300)]);
                }
                if let Some(rows) = rows_of(v)
                    && !rows.is_empty()
                {
                    layers.push(layer_of(k, rows));
                }
            }
        }
        // coarse first; the three coarsest (a transaction, its frames, its bits)
        layers.sort_by_key(|l| l.len());
        let unread = c["uninterpreted"].as_array().map(|a| {
            Track::new(
                "unread",
                a.iter()
                    .map(|u| {
                        let s = u["s"].as_i64().unwrap_or(0);
                        let e = u["e"].as_i64().unwrap_or(s) + 1;
                        (Item { s, e, status: Status::Unread, id: 0 }, format!("unread {} changes", u["edges"]), "?".into())
                    })
                    .collect(),
            )
        });
        out.push(Claim {
            protocol: c["protocol"].as_str().unwrap_or("?").into(),
            verdict: c["verdict"].as_str().unwrap_or("?").into(),
            score: c["score"].as_f64().unwrap_or(0.0),
            roles,
            anchor,
            layers,
            unread,
            bus: c.get("bus").and_then(scalar),
        });
    }
    out
}

/// The analysis of a file run in the background (wireskein-analysis, the Rust engine).
pub fn start_analysis(path: PathBuf, part: usize, n_parts: usize, line_names: Vec<String>, total_edges: u64) -> Arc<Mutex<Option<Analysis>>> {
    let slot = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    spawn(move || {
        let t = Instant::now();
        let want = if n_parts > 1 { Some(part) } else { None };
        // a long capture read in pieces (whole, 12.7 million changes took 545 s and 3.5 GB)
        let opts = wireskein_analysis::split::Options::default();
        let split = (total_edges > 2_000_000 && want.is_none()).then_some(&opts);
        let r = wireskein_analysis::analyze_file(&path, &serde_json::json!({}), want, "all", None, split);
        let a = match r {
            Ok(doc) => Analysis { claims: analysis_of(&doc, part, &line_names), seconds: t.elapsed().as_secs_f64(), error: None, fake: false },
            Err(e) => Analysis { claims: vec![], seconds: t.elapsed().as_secs_f64(), error: Some(e), fake: false },
        };
        *s2.lock().unwrap() = Some(a);
    });
    slot
}

/// Made-up transactions for a made-up capture (the list and the annotations measured without an engine).
pub fn start_fake_analysis(n_items: usize, n_ticks: Tick) -> Arc<Mutex<Option<Analysis>>> {
    let slot = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    spawn(move || {
        let t = Instant::now();
        let track = synth::fake_track("transactions", n_items, n_ticks);
        let rows: Vec<Value> = track
            .items()
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let txt = track.text(i);
                let mut w = txt.split(' ');
                let rw = w.next().unwrap_or("W");
                let addr = i64::from_str_radix(w.next().unwrap_or("0x0").trim_start_matches("0x"), 16).unwrap_or(0);
                let bytes: String = w.filter(|x| x.len() == 2).collect();
                serde_json::json!({"s": it.s, "e": it.e, "addr": addr, "rw": if rw == "W" {"write"} else {"read"},
                    "addr_ack": it.status != Status::Note, "bytes": bytes, "ok": it.status != Status::Error})
            })
            .collect();
        let layer = Layer {
            name: "transactions".into(),
            rows: Rows::Json(rows),
            track,
            columns: ["addr", "rw", "addr_ack", "bytes", "ok"].iter().map(|s| s.to_string()).collect(),
        };
        let claim = Claim {
            protocol: "i2c (made up)".into(),
            verdict: "likely".into(),
            score: 0.5,
            roles: vec![("scl".into(), "D0".into()), ("sda".into(), "D1".into())],
            anchor: Some(1),
            layers: vec![layer],
            unread: None,
            bus: None,
        };
        *s2.lock().unwrap() = Some(Analysis { claims: vec![claim], seconds: t.elapsed().as_secs_f64(), error: None, fake: true });
    });
    slot
}

/// Frames and bits under a clocked claim's coarsest layer, read here from the lines (the document in "all" mode
/// gives an I2C claim its transactions only; the prototype needs 3 tiers to draw). Each sampling edge of the clock
/// (its rise) inside an item is a bit (the data line's level after it), 9 bits a frame for I2C (8 and the ACK), 8 for
/// the others. A stand-in: the engine's document should carry these layers itself.
pub fn derive_tiers(claim: &mut Claim, cap: &Capture) {
    const CLOCKS: [&str; 6] = ["scl", "clk", "sck", "swclk", "tck", "clock"];
    if claim.layers.len() != 1 {
        return;
    }
    let find = |roles: &[&str]| {
        claim
            .roles
            .iter()
            .find(|(r, _)| roles.contains(&r.as_str()))
            .and_then(|(_, n)| cap.lines.iter().position(|l| l.name == *n))
    };
    let (Some(ck), Some(dt)) = (find(&CLOCKS), find(&DATA_ROLES)) else { return };
    let (cks, dts) = (&cap.lines[ck].src, &cap.lines[dt].src);
    if cks.level_before(0).is_none() {
        return; // only lines in memory
    }
    let per = if claim.protocol == "i2c" { 9 } else { 8 };
    let top = &claim.layers[0];
    let mut frames: Vec<(Item, String, String)> = vec![];
    let mut frame_rows: Vec<(u8, u8)> = vec![];
    let mut bits: Vec<(Item, String, String)> = vec![];
    let mut bit_rows: Vec<u8> = vec![];
    let mut buf = vec![];
    for it in top.track.items() {
        buf.clear();
        cks.edges_in(it.s, it.e + 1, 1 << 16, &mut buf);
        let rises: Vec<i64> = buf.iter().copied().filter(|&t| cks.level_before(t + 1) == Some(1)).collect();
        if rises.len() < 2 || bits.len() > 20_000_000 {
            continue;
        }
        let mid = |i: usize| -> i64 {
            if i == 0 {
                rises[0] - (rises[1] - rises[0]) / 2
            } else if i >= rises.len() {
                rises[rises.len() - 1] + (rises[rises.len() - 1] - rises[rises.len() - 2]) / 2
            } else {
                (rises[i - 1] + rises[i]) / 2
            }
        };
        let vals: Vec<u8> = rises.iter().map(|&t| dts.level_before(t + 1).unwrap_or(0)).collect();
        for (i, &v) in vals.iter().enumerate() {
            let id = bits.len() as u32;
            bits.push((Item { s: mid(i), e: mid(i + 1), status: Status::Ok, id }, v.to_string(), v.to_string()));
            bit_rows.push(v);
        }
        for f in 0..rises.len() / per {
            let k = f * per;
            let byte = vals[k..k + 8].iter().fold(0u32, |a, &b| (a << 1) | b as u32) as u8;
            let ack = if per == 9 { 1 + vals[k + 8] } else { 0 };
            let id = frames.len() as u32;
            let status = if ack == 2 { Status::Note } else { Status::Ok };
            let full = format!("0x{byte:02x}{}", ["", " A", " N"][ack as usize]);
            frames.push((Item { s: mid(k), e: mid(k + per), status, id }, full, format!("{byte:02x}")));
            frame_rows.push((byte, ack));
        }
    }
    // (in time order already: the track keeps that order, so row i is item i)
    if !frames.is_empty() {
        let columns = if per == 9 { vec!["byte".into(), "ack".into()] } else { vec!["byte".into()] };
        claim.layers.push(Layer { name: "frames (GUI)".into(), track: Track::new("frames", frames), rows: Rows::Bytes(frame_rows), columns });
    }
    if !bits.is_empty() {
        claim.layers.push(Layer { name: "bits (GUI)".into(), track: Track::new("bits", bits), rows: Rows::Bits(bit_rows), columns: vec!["bit".into()] });
    }
}

/// The analysis of a capture's bytes by the core itself (the web build: analyze_file reads a path; the same claims,
/// in the document's "all" shape).
pub fn analysis_of_bytes(bytes: &[u8], part: usize, line_names: &[String]) -> Analysis {
    let t = Instant::now();
    let run = || -> Result<Vec<Claim>, String> {
        let cap = wireskein_io::read_bytes(bytes)?;
        let rate = cap.rate();
        let p = cap.parts.get(part).ok_or("no such part")?;
        let chans: Vec<wireskein_core::Channel> =
            p.lines.iter().map(|l| wireskein_core::Channel::new(&l.name, l.initial, &l.edges)).collect();
        let view = wireskein_core::View::new(rate, p.ticks, chans)?;
        let (claims, _) = wireskein_core::analyze_full(&view, &serde_json::json!({}));
        let docs: Vec<Value> = claims
            .iter()
            .map(|c| wireskein_analysis::claim_doc(c, "all", &p.lines, rate, p.ticks, None, true))
            .collect();
        let doc = serde_json::json!({"parts": [{"part": part, "claims": docs}]});
        Ok(analysis_of(&doc, part, line_names))
    };
    match run() {
        Ok(claims) => Analysis { claims, seconds: t.elapsed().as_secs_f64(), error: None, fake: false },
        Err(e) => Analysis { claims: vec![], seconds: t.elapsed().as_secs_f64(), error: Some(e), fake: false },
    }
}
