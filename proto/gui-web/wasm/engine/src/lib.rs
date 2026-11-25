//! Prototype 2 (web): what the Web Workers run, in wasm (bench/notes/2026-11-25-gui-study.ja.md §10). A throwaway.
//!
//! - `Capture`: a capture file's bytes read (wireskein-io), one part's lines, analog channels.
//! - `LodJob`: a line's summary (view-core's level 0 buckets) built a block at a time, the buckets handed to the
//!   screen's thread, which builds the levels above (`Lod::append`).
//! - `Analysis`: the analysis of a capture's bytes (wireskein-core; a long part read in pieces by
//!   wireskein-analysis' split), its claims as layers (items, texts, columns for the list), frames and bits read from
//!   the clock where the document has only the transactions (as prototype 1 did), and the search.
//!
//! Times are taken by the JS side (std's Instant does not run in wasm32-unknown-unknown).

use serde_json::{Map, Value, json};
use view_core::Tick;
use view_core::ann::{Item, Status, Track};
use view_core::digital::{EdgeSource, LodBuilder, shift_for};
use view_core::synth::{self, SynthEdges};
use wasm_bindgen::prelude::*;
use wireskein_io::Line;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(s: &str);
}

#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|p| error(&format!("wsk-web-engine: {p}"))));
}

// ---------------------------------------------------------------------------------------------------------------
// capture files

struct AnalogCh {
    name: String,
    unit: String,
    t0: f64,
    dt: f64,
    values: Vec<f32>,
}

/// One part of a capture file read from its bytes.
#[wasm_bindgen]
pub struct Capture {
    tick_hz: f64,
    n_parts: usize,
    n_ticks: Tick,
    lines: Vec<Line>,
    analog: Vec<AnalogCh>,
    complete: bool,
    lost: String,
}

#[wasm_bindgen]
impl Capture {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8], part: usize) -> Result<Capture, JsError> {
        let cap = wireskein_io::read_bytes(bytes).map_err(|e| JsError::new(&e))?;
        let n_parts = cap.parts.len();
        let tick_hz = cap.rate();
        let lost = serde_json::to_string(&cap.lost).unwrap_or_default();
        let complete = cap.complete;
        let p = cap
            .parts
            .into_iter()
            .nth(part)
            .ok_or_else(|| JsError::new(&format!("part {part} of {n_parts}")))?;
        let analog = p
            .analog
            .iter()
            .map(|a| {
                let volts = a.volts();
                let unit = if volts.is_some() { "V".to_string() } else { a.unit.clone() };
                let values = volts.unwrap_or_else(|| a.values.clone()).iter().map(|&v| v as f32).collect();
                let rate = a.rate_hz.0 as f64 / a.rate_hz.1 as f64;
                let t0 = a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64;
                AnalogCh { name: a.name.clone(), unit, t0, dt: tick_hz / rate, values }
            })
            .collect();
        Ok(Capture { tick_hz, n_parts, n_ticks: p.ticks, lines: p.lines, analog, complete, lost })
    }
    pub fn n_parts(&self) -> usize {
        self.n_parts
    }
    pub fn tick_hz(&self) -> f64 {
        self.tick_hz
    }
    pub fn n_ticks(&self) -> f64 {
        self.n_ticks as f64
    }
    pub fn complete(&self) -> bool {
        self.complete
    }
    /// The lost stretches as JSON (data lost while capturing).
    pub fn lost(&self) -> String {
        self.lost.clone()
    }
    pub fn n_lines(&self) -> usize {
        self.lines.len()
    }
    pub fn line_name(&self, i: usize) -> String {
        self.lines[i].name.clone()
    }
    pub fn line_initial(&self, i: usize) -> u8 {
        self.lines[i].initial
    }
    pub fn line_n_edges(&self, i: usize) -> f64 {
        self.lines[i].edges.len() as f64
    }
    /// A line's changes (a copy, for the screen's thread).
    pub fn line_edges(&self, i: usize) -> Vec<i64> {
        self.lines[i].edges.clone()
    }
    /// A line's summary to build: its changes moved into the job (the line is left empty here).
    pub fn lod_job(&mut self, i: usize, per_bucket: u32) -> LodJob {
        let l = &mut self.lines[i];
        let src = view_core::digital::VecEdges::new(l.initial, self.n_ticks, std::mem::take(&mut l.edges));
        LodJob::of(Box::new(src), per_bucket)
    }
    pub fn n_analog(&self) -> usize {
        self.analog.len()
    }
    pub fn analog_name(&self, i: usize) -> String {
        self.analog[i].name.clone()
    }
    pub fn analog_unit(&self, i: usize) -> String {
        self.analog[i].unit.clone()
    }
    pub fn analog_t0(&self, i: usize) -> f64 {
        self.analog[i].t0
    }
    pub fn analog_dt(&self, i: usize) -> f64 {
        self.analog[i].dt
    }
    pub fn analog_values(&self, i: usize) -> Vec<f32> {
        self.analog[i].values.clone()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// summaries

/// A line's summary built a block at a time (as view-core's build_lod_shared, its level 0 handed out).
#[wasm_bindgen]
pub struct LodJob {
    src: Box<dyn EdgeSource>,
    b: LodBuilder,
    shift0: u32,
    t: Tick,
    n: Tick,
    step: Tick,
    buf: Vec<Tick>,
    counts: Vec<u32>,
    ends: Vec<u8>,
    done: bool,
}

impl LodJob {
    fn of(src: Box<dyn EdgeSource>, per_bucket: u32) -> LodJob {
        let n = src.n_ticks();
        let shift0 = shift_for(n, src.approx_edges(), per_bucket);
        let b = LodBuilder::new(n, src.initial(), shift0);
        LodJob {
            src,
            b,
            shift0,
            t: 0,
            n,
            step: (1 as Tick) << (shift0 + 14).min(40),
            buf: vec![],
            counts: vec![],
            ends: vec![],
            done: false,
        }
    }
}

#[wasm_bindgen]
impl LodJob {
    /// A made-up line's summary (`synth_plan` gives its period and segment length).
    pub fn synth(line: u32, n_ticks: f64, period: f64, seg_len: f64, per_bucket: u32) -> LodJob {
        let src = SynthEdges { line: line as u64, n_ticks: n_ticks as Tick, period: period as Tick, seg_len: seg_len as Tick };
        LodJob::of(Box::new(src), per_bucket)
    }
    pub fn shift0(&self) -> u32 {
        self.shift0
    }
    /// `blocks` blocks read (2^14 level-0 buckets each); true when the line is done. The buckets: take_counts /
    /// take_ends.
    pub fn step(&mut self, blocks: u32) -> bool {
        if self.done {
            return true;
        }
        for _ in 0..blocks {
            if self.t >= self.n {
                break;
            }
            let t1 = (self.t + self.step).min(self.n);
            self.buf.clear();
            self.src.edges_in(self.t, t1, usize::MAX, &mut self.buf);
            for &e in &self.buf {
                self.b.push(e);
            }
            self.b.close_before(t1);
            self.t = t1;
        }
        let mut got = self.b.take_pending();
        if self.t >= self.n {
            got.extend(self.b.finish());
            self.done = true;
        }
        for (c, e) in got {
            self.counts.push(c);
            self.ends.push(e);
        }
        self.done
    }
    pub fn take_counts(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.counts)
    }
    pub fn take_ends(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.ends)
    }
    pub fn built_until(&self) -> f64 {
        self.t as f64
    }
}

/// A made-up capture of about `total` changes over `n_lines` lines: [n_ticks, then each line's period and segment
/// length] (the lines themselves are made on the fly wherever they are read).
#[wasm_bindgen]
pub fn synth_plan(total: f64, n_lines: u32) -> Vec<f64> {
    let (n, ls) = synth::synth_lines(total as u64, n_lines as usize);
    let mut v = vec![n as f64];
    for l in ls {
        v.push(l.period as f64);
        v.push(l.seg_len as f64);
    }
    v
}

/// An analog line's samples and its summary built here (view-core's AnalogLine), handed to the screen's thread
/// flat: the levels' bands one after another (min, max, mean each) and each level's length.
#[wasm_bindgen]
pub struct AnalogParts {
    values: Vec<f32>,
    levels: Vec<f32>,
    lens: Vec<u32>,
}

#[wasm_bindgen]
impl AnalogParts {
    /// A made-up line of `n` samples (view-core's synth_analog), `dt` ticks a sample.
    pub fn synth(n: u32, dt: f64) -> AnalogParts {
        AnalogParts::of(synth::synth_analog(n as usize, dt))
    }
    pub fn of(values: Vec<f32>) -> AnalogParts {
        let a = view_core::analog::AnalogLine::new("", "", 0.0, 1.0, values);
        let mut levels = vec![];
        let mut lens = vec![];
        for l in a.levels() {
            lens.push(l.len() as u32);
            for b in l {
                levels.extend_from_slice(&[b.min, b.max, b.mean]);
            }
        }
        AnalogParts { values: a.values, levels, lens }
    }
    pub fn take_values(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.values)
    }
    pub fn take_levels(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.levels)
    }
    pub fn lens(&self) -> Vec<u32> {
        self.lens.clone()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// the analysis (as prototype 1's model.rs)

enum Rows {
    Json(Vec<Value>),
    Bits(Vec<u8>),
    /// a byte and its ACK (0: none, 1: ACK, 2: NACK)
    Bytes(Vec<(u8, u8)>),
}

struct Layer {
    name: String,
    rows: Rows,
    track: Track,
    columns: Vec<String>,
}

impl Layer {
    fn len(&self) -> usize {
        match &self.rows {
            Rows::Json(v) => v.len(),
            Rows::Bits(v) => v.len(),
            Rows::Bytes(v) => v.len(),
        }
    }
    fn value(&self, i: usize) -> std::borrow::Cow<'_, Value> {
        use std::borrow::Cow;
        match &self.rows {
            Rows::Json(v) => Cow::Borrowed(&v[i]),
            Rows::Bits(v) => Cow::Owned(json!({"bit": v[i]})),
            Rows::Bytes(v) => {
                let (b, a) = v[i];
                let mut o = json!({"byte": b});
                if a > 0 {
                    o["ack"] = Value::Bool(a == 1);
                }
                Cow::Owned(o)
            }
        }
    }
}

struct Claim {
    protocol: String,
    verdict: String,
    score: f64,
    roles: Vec<(String, String)>,
    anchor: Option<usize>,
    layers: Vec<Layer>,
    unread: Option<Track>,
    bus: Option<String>,
}

fn status_of(it: &Value) -> Status {
    let f = |k: &str| it.get(k);
    if f("ok") == Some(&Value::Bool(false))
        || f("checksum_ok") == Some(&Value::Bool(false))
        || f("error").is_some_and(|e| !e.is_null())
    {
        return Status::Error;
    }
    if f("complete") == Some(&Value::Bool(false))
        || f("restored").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        || f("repaired").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        return Status::Warn;
    }
    if f("addr_ack") == Some(&Value::Bool(false))
        || f("ack") == Some(&Value::Bool(false))
        || f("acked") == Some(&Value::Bool(false))
    {
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
fn item_text(it: &Value) -> (String, String) {
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
        Value::Array(a) if a.first().is_some_and(|x| x.get("s").is_some_and(|s| s.is_i64() || s.is_f64())) => {
            Some(a.clone())
        }
        Value::Object(o) if o.get("s").is_some_and(|x| x.is_array()) => {
            let n = o["s"].as_array().unwrap().len();
            let mut rows = vec![];
            for i in 0..n {
                let mut r = Map::new();
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
const CLOCKS: [&str; 6] = ["scl", "clk", "sck", "swclk", "tck", "clock"];

fn claims_of(docs: &[Value], line_names: &[String]) -> Vec<Claim> {
    let mut out = vec![];
    for c in docs {
        let roles: Vec<(String, String)> = c["roles"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), scalar(v).unwrap_or_default())).collect())
            .unwrap_or_default();
        let anchor_name =
            roles.iter().find(|(r, _)| DATA_ROLES.contains(&r.as_str())).or(roles.last()).map(|(_, n)| n.clone());
        let anchor = anchor_name.and_then(|n| line_names.iter().position(|x| *x == n));
        let mut layers: Vec<Layer> = vec![];
        if let Some(o) = c["layers"].as_object().or(c["result"].as_object()) {
            for (k, v) in o {
                if let Some(rows) = rows_of(v)
                    && !rows.is_empty()
                {
                    layers.push(layer_of(k, rows));
                }
            }
        }
        layers.sort_by_key(|l| l.len());
        let unread = c["uninterpreted"].as_array().map(|a| {
            Track::new(
                "unread",
                a.iter()
                    .map(|u| {
                        let s = u["s"].as_i64().unwrap_or(0);
                        let e = u["e"].as_i64().unwrap_or(s) + 1;
                        (
                            Item { s, e, status: Status::Unread, id: 0 },
                            format!("unread {} changes", u["edges"]),
                            "?".into(),
                        )
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

fn level_after(l: &Line, t: Tick) -> u8 {
    l.initial ^ (l.edges.partition_point(|&e| e <= t) & 1) as u8
}

/// Frames and bits under a clocked claim's coarsest layer, read from the lines (prototype 1's derive_tiers: the
/// document in "all" mode gives an I2C claim its transactions only).
fn derive_tiers(claim: &mut Claim, lines: &[Line]) {
    if claim.layers.len() != 1 {
        return;
    }
    let find = |roles: &[&str]| {
        claim
            .roles
            .iter()
            .find(|(r, _)| roles.contains(&r.as_str()))
            .and_then(|(_, n)| lines.iter().position(|l| l.name == *n))
    };
    let (Some(ck), Some(dt)) = (find(&CLOCKS), find(&DATA_ROLES)) else { return };
    let (cks, dts) = (&lines[ck], &lines[dt]);
    let per = if claim.protocol == "i2c" { 9 } else { 8 };
    let top = &claim.layers[0];
    let mut frames: Vec<(Item, String, String)> = vec![];
    let mut frame_rows: Vec<(u8, u8)> = vec![];
    let mut bits: Vec<(Item, String, String)> = vec![];
    let mut bit_rows: Vec<u8> = vec![];
    for it in top.track.items() {
        let (a, b) = (cks.edges.partition_point(|&e| e < it.s), cks.edges.partition_point(|&e| e < it.e + 1));
        let rises: Vec<i64> = cks.edges[a..b.min(a + (1 << 16))]
            .iter()
            .copied()
            .filter(|&t| level_after(cks, t) == 1)
            .collect();
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
        let vals: Vec<u8> = rises.iter().map(|&t| level_after(dts, t)).collect();
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
    if !frames.is_empty() {
        let columns = if per == 9 { vec!["byte".into(), "ack".into()] } else { vec!["byte".into()] };
        claim.layers.push(Layer {
            name: "frames (GUI)".into(),
            track: Track::new("frames", frames),
            rows: Rows::Bytes(frame_rows),
            columns,
        });
    }
    if !bits.is_empty() {
        claim.layers.push(Layer {
            name: "bits (GUI)".into(),
            track: Track::new("bits", bits),
            rows: Rows::Bits(bit_rows),
            columns: vec!["bit".into()],
        });
    }
}

/// The claims of a part analysed whole ("all": every layer).
fn analyze_whole(p: &wireskein_io::Part, rate: f64) -> Result<Vec<Value>, String> {
    let chans: Vec<wireskein_core::Channel> =
        p.lines.iter().map(|l| wireskein_core::Channel::new(&l.name, l.initial, &l.edges)).collect();
    let view = wireskein_core::View::new(rate, p.ticks, chans)?;
    let (claims, _) = wireskein_core::analyze_full(&view, &json!({}));
    Ok(claims
        .iter()
        .map(|c| wireskein_analysis::claim_doc(c, "all", &p.lines, rate, p.ticks, None, true))
        .collect())
}

/// The search's words (prototype 1's panels.rs): all to match.
enum Word {
    Field(String, String),
    Status(Status),
    Rw(&'static str),
    Text(String),
}

fn num(s: &str) -> Option<i64> {
    if let Some(h) = s.strip_prefix("0x") { i64::from_str_radix(h, 16).ok() } else { s.parse().ok() }
}

fn words(q: &str) -> Vec<Word> {
    q.split_whitespace()
        .map(|w| {
            let lw = w.to_lowercase();
            if let Some((k, v)) = w.split_once('=') {
                return Word::Field(k.into(), v.to_lowercase());
            }
            match lw.as_str() {
                "nack" | "ナック" => Word::Status(Status::Note),
                "error" | "エラー" | "誤り" => Word::Status(Status::Error),
                "warn" | "疑わしい" => Word::Status(Status::Warn),
                "write" | "書き込み" | "書き" => Word::Rw("write"),
                "read" | "読み出し" | "読み" => Word::Rw("read"),
                _ => Word::Text(lw),
            }
        })
        .collect()
}

fn addr_text(v: &Value) -> String {
    match v.get("addr") {
        Some(Value::Number(n)) => n.as_i64().map(|i| format!("0x{i:02x}")).unwrap_or(n.to_string()),
        Some(Value::String(s)) => s.to_lowercase(),
        _ => String::new(),
    }
}

fn matches(ws: &[Word], v: &Value, text: &str, status: Status) -> bool {
    ws.iter().all(|w| match w {
        Word::Field(k, want) => match v.get(k) {
            Some(Value::Number(n)) => num(want).is_some_and(|x| n.as_i64() == Some(x)),
            Some(Value::String(s)) => s.to_lowercase().contains(want.as_str()),
            Some(Value::Bool(b)) => {
                matches!((want.as_str(), b), ("true" | "1" | "✓", true) | ("false" | "0" | "✗", false))
            }
            _ => false,
        },
        Word::Status(s) => {
            if *s == Status::Note {
                status == Status::Note || text.contains("NACK")
            } else {
                status >= *s && status != Status::Unread
            }
        }
        Word::Rw(rw) => v.get("rw").and_then(|x| x.as_str()) == Some(rw),
        Word::Text(t) => text.to_lowercase().contains(t.as_str()) || addr_text(v) == *t,
    })
}

/// The analysis of one part, its layers for the screens, the search over them.
#[wasm_bindgen]
pub struct Analysis {
    claims: Vec<Claim>,
    error: Option<String>,
    fake: bool,
    split: bool,
    hits: Vec<(usize, usize, Vec<u32>)>,
    hit_ticks: Vec<f64>,
}

#[wasm_bindgen]
impl Analysis {
    /// A capture's bytes analysed (part `part`): a part of more than 2 million changes read in pieces (as
    /// analyze's split; whole, 12.7 million took 545 s and 3.5 GB natively).
    pub fn run(bytes: &[u8], part: usize) -> Analysis {
        let mut split = false;
        let mut run = || -> Result<Vec<Claim>, String> {
            let cap = wireskein_io::read_bytes(bytes)?;
            let rate = cap.rate();
            let p = cap.parts.get(part).ok_or("no such part")?;
            let names: Vec<String> = p.lines.iter().map(|l| l.name.clone()).collect();
            let total: usize = p.lines.iter().map(|l| l.edges.len()).sum();
            let docs = if total > 2_000_000 {
                let opts = wireskein_analysis::split::Options::default();
                let mut head = Map::new();
                head.insert("part".into(), json!(p.part));
                head.insert("ticks".into(), json!(p.ticks));
                let gf = p
                    .meta
                    .get("probe")
                    .and_then(|x| x.get("glitch_filter_ns"))
                    .filter(|v| wireskein_analysis::nonempty(v) && **v != json!(0) && **v != json!(false))
                    .cloned();
                let mut src = wireskein_io::stream::MemSource::new(&p.lines, p.ticks);
                match wireskein_analysis::split::analyze(&mut src, rate, &json!({}), &opts, None, &head, gf.as_ref(), "protocol")? {
                    wireskein_analysis::split::Split::Pieces(claims, _) => {
                        split = true;
                        claims
                    }
                    wireskein_analysis::split::Split::Whole(_) => analyze_whole(p, rate)?,
                }
            } else {
                analyze_whole(p, rate)?
            };
            let mut claims = claims_of(&docs, &names);
            for c in &mut claims {
                derive_tiers(c, &p.lines);
            }
            Ok(claims)
        };
        let r = run();
        match r {
            Ok(claims) => Analysis { claims, error: None, fake: false, split, hits: vec![], hit_ticks: vec![] },
            Err(e) => Analysis { claims: vec![], error: Some(e), fake: false, split, hits: vec![], hit_ticks: vec![] },
        }
    }

    /// Made-up transactions for a made-up capture (the list and the annotations measured without an engine).
    pub fn fake(n_items: u32, n_ticks: f64) -> Analysis {
        let track = synth::fake_track("transactions", n_items as usize, n_ticks as Tick);
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
                json!({"s": it.s, "e": it.e, "addr": addr, "rw": if rw == "W" {"write"} else {"read"},
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
        Analysis { claims: vec![claim], error: None, fake: true, split: false, hits: vec![], hit_ticks: vec![] }
    }

    /// The claims and their layers as JSON (no items).
    pub fn meta(&self) -> String {
        let claims: Vec<Value> = self
            .claims
            .iter()
            .map(|c| {
                json!({
                    "protocol": c.protocol, "verdict": c.verdict, "score": c.score,
                    "roles": c.roles.iter().map(|(r, n)| json!([r, n])).collect::<Vec<_>>(),
                    "anchor": c.anchor, "bus": c.bus,
                    "layers": c.layers.iter().map(|l| json!({"name": l.name, "n": l.len(), "columns": l.columns})).collect::<Vec<_>>(),
                    "unread": c.unread.as_ref().map(|u| u.len()).unwrap_or(0),
                })
            })
            .collect();
        json!({"error": self.error, "fake": self.fake, "split": self.split, "claims": claims}).to_string()
    }

    fn layer(&self, c: usize, l: usize) -> &Layer {
        &self.claims[c].layers[l]
    }
    /// A layer's items in order of start (row i is item i): starts, ends, statuses.
    pub fn layer_s(&self, c: usize, l: usize) -> Vec<f64> {
        self.layer(c, l).track.items().iter().map(|it| it.s as f64).collect()
    }
    pub fn layer_e(&self, c: usize, l: usize) -> Vec<f64> {
        self.layer(c, l).track.items().iter().map(|it| it.e as f64).collect()
    }
    pub fn layer_status(&self, c: usize, l: usize) -> Vec<u8> {
        self.layer(c, l).track.items().iter().map(|it| it.status as u8).collect()
    }
    /// The items' full and short texts one after another, and their offsets in UTF-8 bytes.
    pub fn layer_buf(&self, c: usize, l: usize) -> Vec<u8> {
        self.layer(c, l).track.parts().1.as_bytes().to_vec()
    }
    pub fn layer_off(&self, c: usize, l: usize) -> Vec<u32> {
        self.layer(c, l).track.parts().2.to_vec()
    }
    pub fn unread_s(&self, c: usize) -> Vec<f64> {
        self.claims[c].unread.as_ref().map(|u| u.items().iter().map(|it| it.s as f64).collect()).unwrap_or_default()
    }
    pub fn unread_e(&self, c: usize) -> Vec<f64> {
        self.claims[c].unread.as_ref().map(|u| u.items().iter().map(|it| it.e as f64).collect()).unwrap_or_default()
    }

    /// A column's kind: 0 numbers, 1 booleans, 2 texts.
    pub fn column_kind(&self, c: usize, l: usize, k: usize) -> u8 {
        let ly = self.layer(c, l);
        let col = &ly.columns[k];
        match &ly.rows {
            Rows::Bits(_) => 0,
            Rows::Bytes(_) => {
                if col == "ack" {
                    1
                } else {
                    0
                }
            }
            Rows::Json(rows) => {
                let (mut n, mut b, mut other) = (0, 0, 0);
                for r in rows.iter().take(2000) {
                    match r.get(col) {
                        Some(Value::Number(_)) => n += 1,
                        Some(Value::Bool(_)) => b += 1,
                        None | Some(Value::Null) => {}
                        _ => other += 1,
                    }
                }
                if other == 0 && b == 0 && n > 0 {
                    0
                } else if other == 0 && n == 0 && b > 0 {
                    1
                } else {
                    2
                }
            }
        }
    }
    /// A number column (NaN: none).
    pub fn column_num(&self, c: usize, l: usize, k: usize) -> Vec<f64> {
        let ly = self.layer(c, l);
        let col = &ly.columns[k];
        match &ly.rows {
            Rows::Bits(v) => v.iter().map(|&b| b as f64).collect(),
            Rows::Bytes(v) => v.iter().map(|&(b, _)| b as f64).collect(),
            Rows::Json(rows) => rows.iter().map(|r| r.get(col).and_then(|x| x.as_f64()).unwrap_or(f64::NAN)).collect(),
        }
    }
    /// A boolean column (0 false, 1 true, 2 none).
    pub fn column_bool(&self, c: usize, l: usize, k: usize) -> Vec<u8> {
        let ly = self.layer(c, l);
        let col = &ly.columns[k];
        match &ly.rows {
            Rows::Bytes(v) => v.iter().map(|&(_, a)| match a {
                1 => 1,
                2 => 0,
                _ => 2,
            }).collect(),
            Rows::Json(rows) => rows
                .iter()
                .map(|r| match r.get(col) {
                    Some(Value::Bool(true)) => 1,
                    Some(Value::Bool(false)) => 0,
                    _ => 2,
                })
                .collect(),
            Rows::Bits(v) => vec![2; v.len()],
        }
    }
    /// A text column.
    pub fn column_str(&self, c: usize, l: usize, k: usize) -> Vec<String> {
        let ly = self.layer(c, l);
        let col = &ly.columns[k];
        (0..ly.len())
            .map(|i| match ly.value(i).get(col) {
                None => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) => "–".into(),
                Some(Value::Bool(b)) => if *b { "✓".into() } else { "×".into() },
                Some(x) => {
                    let s = x.to_string();
                    if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s }
                }
            })
            .collect()
    }

    /// The search: words all to match (prototype 1's). The number of hits; per layer `hit_rows`.
    pub fn search(&mut self, q: &str) -> u32 {
        self.hits.clear();
        self.hit_ticks.clear();
        let ws = words(q);
        if ws.is_empty() {
            return 0;
        }
        let mut total = 0u32;
        for (ci, c) in self.claims.iter().enumerate() {
            for (li, l) in c.layers.iter().enumerate() {
                if matches!(l.rows, Rows::Bits(_)) {
                    continue; // (bits: not searched)
                }
                let mut v = vec![];
                for i in 0..l.len() {
                    let it = l.track.item(i);
                    if matches(&ws, l.value(i).as_ref(), l.track.text(i), it.status) {
                        v.push(i as u32);
                        if li == 0 && self.hit_ticks.len() < 20_000 {
                            self.hit_ticks.push(it.s as f64);
                        }
                    }
                }
                total += v.len() as u32;
                if !v.is_empty() {
                    self.hits.push((ci, li, v));
                }
            }
        }
        total
    }
    /// The layers with hits: (claim, layer) pairs flat.
    pub fn hit_layers(&self) -> Vec<u32> {
        self.hits.iter().flat_map(|h| [h.0 as u32, h.1 as u32]).collect()
    }
    pub fn hit_rows(&self, k: usize) -> Vec<u32> {
        self.hits[k].2.clone()
    }
    /// The starts of the hits in each claim's coarsest layer (at most 20,000: the minimap's marks).
    pub fn hit_ticks(&self) -> Vec<f64> {
        self.hit_ticks.clone()
    }
}
