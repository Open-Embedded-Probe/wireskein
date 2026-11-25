//! Prototype 2b (bench/notes/2026-11-25-gui-study.ja.md §11): prototype 2's screens in a Tauri 2 shell. A throwaway.
//!
//! - The screens are web/dist as they are (built into the binary). `?engine=wasm` (the default): the Web Workers and
//!   their wasm, as in a browser; capture files served by the `wskfile` scheme (the roots given by `--files` /
//!   `--mount`, as tools/serve.mjs).
//! - `?engine=native`: what the workers did runs here, natively: a "worker" is a thread with a queue (`worker_new`,
//!   `worker_post`, `worker_end`), running wsk-web-engine (the crate the wasm is made of: wireskein-io / -core /
//!   -analysis) and answering with the same messages through a Tauri channel. How a message travels:
//!   `?ipc=json` (JSON, arrays as numbers), `raw` (one binary message: a JSON head and the arrays after it, 8-byte
//!   aligned), `chunk` (the same bytes in pieces of 1 MiB), `fetch` (a JSON notice; the page fetches the bytes from
//!   the `wskblob` scheme).
//! - `&frameipc=1`: a native scene (wsk-web-view built natively) kept beside the page's wasm one; the bench asks it for
//!   the same frames (`frame`, raw; `frame_json`) to measure what the shapes cost to bring over.
//! - `report`: what the page measured printed here (`[report] ...`, milliseconds since the process started).
//!
//! `wsk-gui-tauri [--files DIR] [--mount NAME=DIR] [--size WxH] [--log FILE] [QUERY]`, QUERY as the page's URL query
//! (`file=files/examples/mixed-1.wireskein&engine=native&bench=1&report=1&exit=1`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tauri::Manager;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use wsk_web_engine::{AnalogParts, Analysis, Capture, LodJob, synth_plan};
use wsk_web_view::Scene;

static T0: OnceLock<Instant> = OnceLock::new();
/// `--log FILE`: the reports appended there too (a Windows release build has no console)
static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

fn say(line: &str) {
    println!("{line}");
    if let Some(f) = LOG.get() {
        use std::io::Write;
        let _ = writeln!(f.lock().unwrap(), "{line}");
    }
}
fn since_start_ms() -> f64 {
    T0.get().map(|t| t.elapsed().as_secs_f64() * 1e3).unwrap_or(0.0)
}

// ---------------------------------------------------------------------------------------------------------------
// messages: a JSON head and typed arrays placed into it (at a path) on the page's side

enum Arr {
    I64(Vec<i64>),
    F64(Vec<f64>),
    F32(Vec<f32>),
    U32(Vec<u32>),
    U8(Vec<u8>),
}

impl Arr {
    fn tag(&self) -> &'static str {
        match self {
            Arr::I64(_) => "i64",
            Arr::F64(_) => "f64",
            Arr::F32(_) => "f32",
            Arr::U32(_) => "u32",
            Arr::U8(_) => "u8",
        }
    }
    fn len(&self) -> usize {
        match self {
            Arr::I64(v) => v.len(),
            Arr::F64(v) => v.len(),
            Arr::F32(v) => v.len(),
            Arr::U32(v) => v.len(),
            Arr::U8(v) => v.len(),
        }
    }
    fn bytes(&self) -> &[u8] {
        fn b<T>(v: &[T]) -> &[u8] {
            // plain numbers, little-endian on every target this runs on
            unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
        }
        match self {
            Arr::I64(v) => b(v),
            Arr::F64(v) => b(v),
            Arr::F32(v) => b(v),
            Arr::U32(v) => b(v),
            Arr::U8(v) => v,
        }
    }
    fn json(&self) -> Value {
        match self {
            Arr::I64(v) => json!(v),
            Arr::F64(v) => json!(v),
            Arr::F32(v) => json!(v),
            Arr::U32(v) => json!(v),
            Arr::U8(v) => json!(v),
        }
    }
}

struct Msg {
    head: Value,
    arrays: Vec<(Value, Arr)>,
}

impl Msg {
    fn new(head: Value) -> Msg {
        Msg { head, arrays: vec![] }
    }
    fn arr(mut self, path: Value, a: Arr) -> Msg {
        self.arrays.push((path, a));
        self
    }
    fn push(&mut self, path: Value, a: Arr) {
        self.arrays.push((path, a));
    }
    fn n_bytes(&self) -> usize {
        self.arrays.iter().map(|(_, a)| a.bytes().len()).sum()
    }
    /// `[u32 head length][head][arrays, each 8-byte aligned]`, the head's `$a` telling where each array is.
    fn encode(self) -> Vec<u8> {
        let mut head = self.head;
        // the head's length depends on the offsets: lay the arrays out after a head of a generous size
        let arrays = &self.arrays;
        let place = |base: usize| {
            let mut descr = vec![];
            let mut o = base;
            for (p, a) in arrays {
                o = o.div_ceil(8) * 8;
                descr.push(json!({"p": p, "t": a.tag(), "o": o, "n": a.len()}));
                o += a.bytes().len();
            }
            (descr, o)
        };
        let mut guess = 4 + serde_json::to_vec(&head).unwrap().len() + 64 * arrays.len() + 64;
        let (descr, total) = loop {
            guess = guess.div_ceil(8) * 8;
            let (descr, end) = place(guess);
            head["$a"] = json!(descr);
            let h = serde_json::to_vec(&head).unwrap();
            if 4 + h.len() <= guess {
                break (descr, end);
            }
            guess = 4 + h.len() + 64;
        };
        let h = serde_json::to_vec(&head).unwrap();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(h.len() as u32).to_le_bytes());
        out.extend_from_slice(&h);
        for (d, (_, a)) in descr.iter().zip(&self.arrays) {
            let o = d["o"].as_u64().unwrap() as usize;
            out.resize(o, 0);
            out.extend_from_slice(a.bytes());
        }
        out
    }
    fn to_json(self) -> String {
        let mut head = self.head;
        head["$j"] = Value::Array(
            self.arrays
                .iter()
                .map(|(p, a)| json!({"p": p, "t": a.tag(), "v": a.json()}))
                .collect(),
        );
        serde_json::to_string(&head).unwrap()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Json,
    Raw,
    Chunk,
    Fetch,
}

const CHUNK: usize = 1 << 20;

#[derive(Clone)]
struct Out {
    ch: Channel<InvokeResponseBody>,
    mode: Mode,
    blobs: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    next_blob: Arc<AtomicU64>,
}

impl Out {
    fn send(&self, m: Msg) {
        let _ = match self.mode {
            Mode::Json => self.ch.send(InvokeResponseBody::Json(m.to_json())),
            Mode::Raw => self.ch.send(InvokeResponseBody::Raw(m.encode())),
            Mode::Chunk => {
                let b = m.encode();
                let total = b.len() as u32;
                let mut r = Ok(());
                for (k, piece) in b.chunks(CHUNK).enumerate() {
                    let mut p = Vec::with_capacity(piece.len() + 8);
                    p.extend_from_slice(&total.to_le_bytes());
                    p.extend_from_slice(&((k * CHUNK) as u32).to_le_bytes());
                    p.extend_from_slice(piece);
                    r = self.ch.send(InvokeResponseBody::Raw(p));
                }
                r
            }
            Mode::Fetch => {
                let small = m.arrays.is_empty();
                if small {
                    self.ch.send(InvokeResponseBody::Json(m.to_json()))
                } else {
                    let b = m.encode();
                    let id = self.next_blob.fetch_add(1, Ordering::Relaxed);
                    let n = b.len();
                    self.blobs.lock().unwrap().insert(id, b);
                    self.ch
                        .send(InvokeResponseBody::Json(json!({"$blob": id, "n": n}).to_string()))
                }
            }
        };
    }
    fn json(&self, v: Value) {
        self.send(Msg::new(v));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// the state

struct Worker {
    tx: mpsc::Sender<Value>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Roots {
    dirs: HashMap<String, PathBuf>,
}

struct App {
    workers: Mutex<HashMap<u32, Worker>>,
    roots: Roots,
    mem_bytes: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
    next_mem: AtomicU64,
    blobs: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    next_blob: Arc<AtomicU64>,
    /// the native scene for `frameipc` (and the capture it was made for)
    scene: Arc<Mutex<Option<Scene>>>,
    exit_after_bench: bool,
}

impl App {
    /// A capture's bytes: `mem:N` (bytes the page handed over: a File), or a path under a root (`files/...`).
    fn read_src(&self, src: &str) -> Result<Arc<Vec<u8>>, String> {
        if let Some(k) = src.strip_prefix("mem:") {
            let k: u64 = k.parse().map_err(|_| format!("bad source {src}"))?;
            return self
                .mem_bytes
                .lock()
                .unwrap()
                .get(&k)
                .cloned()
                .ok_or(format!("no bytes {src}"));
        }
        let p = self.resolve(src)?;
        std::fs::read(&p)
            .map(Arc::new)
            .map_err(|e| format!("{}: {e}", p.display()))
    }
    fn resolve(&self, url_path: &str) -> Result<PathBuf, String> {
        let s = percent_decode(url_path.trim_start_matches('/'));
        let (root, rest) = s.split_once('/').ok_or(format!("no root in {s}"))?;
        let dir = self
            .roots
            .dirs
            .get(root)
            .ok_or(format!("no root {root} (--files / --mount)"))?;
        if rest.split('/').any(|c| c == "..") {
            return Err(format!("{s}: '..'"));
        }
        Ok(dir.join(rest))
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn rss() -> u64 {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64).unwrap_or(0)
}

/// This process and its descendants' resident memory (Linux: the WebKitGTK web and network processes), by name.
#[cfg(target_os = "linux")]
fn tree_rss() -> Vec<(String, u64)> {
    let me = std::process::id();
    let mut procs: Vec<(u32, u32, String, u64)> = vec![];
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some(close) = stat.rfind(')') else { continue };
            let name = stat[stat.find('(').unwrap_or(0) + 1..close].to_string();
            let f: Vec<&str> = stat[close + 2..].split(' ').collect();
            let ppid: u32 = f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            let rss_pages: u64 = f.get(21).and_then(|x| x.parse().ok()).unwrap_or(0);
            procs.push((pid, ppid, name, rss_pages * 4096));
        }
    }
    let mut keep = vec![me];
    let mut out = vec![];
    let mut k = 0;
    while k < keep.len() {
        let p = keep[k];
        for (pid, ppid, name, rss) in &procs {
            if *pid == p {
                out.push((name.clone(), *rss));
            }
            if *ppid == p {
                keep.push(*pid);
            }
        }
        k += 1;
    }
    out
}
#[cfg(not(target_os = "linux"))]
fn tree_rss() -> Vec<(String, u64)> {
    vec![("wsk-gui-tauri".into(), rss())]
}

// ---------------------------------------------------------------------------------------------------------------
// what a worker does (as web/src/workers/load.worker.ts and analysis.worker.ts)

struct Local {
    an: Option<Analysis>,
}

fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or("panic".into())
    })
}

fn run_lod(out: &Out, i: usize, mut job: LodJob, cancel: &AtomicBool, scene: Option<&Mutex<Option<Scene>>>) {
    let shift0 = job.shift0();
    out.json(json!({"type": "lod-begin", "i": i, "shift0": shift0}));
    if let Some(s) = scene
        && let Some(sc) = s.lock().unwrap().as_mut()
    {
        sc.lod_begin(i, shift0);
    }
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let done = job.step(1);
        if done || last.elapsed().as_millis() > 40 {
            let counts = job.take_counts();
            let ends = job.take_ends();
            if let Some(s) = scene
                && let Some(sc) = s.lock().unwrap().as_mut()
            {
                sc.lod_append(i, &counts, &ends, done);
            }
            out.send(
                Msg::new(json!({"type": "lod", "i": i, "last": done, "built": job.built_until()}))
                    .arr(json!(["counts"]), Arr::U32(counts))
                    .arr(json!(["ends"]), Arr::U8(ends)),
            );
            last = Instant::now();
        }
        if done {
            break;
        }
    }
}

fn analog_msg(i: usize, mut p: AnalogParts) -> (Msg, Vec<f32>, Vec<f32>, Vec<u32>) {
    let values = p.take_values();
    let levels = p.take_levels();
    let lens = p.lens();
    let m = Msg::new(json!({"type": "analog", "i": i}))
        .arr(json!(["values"]), Arr::F32(values.clone()))
        .arr(json!(["levels"]), Arr::F32(levels.clone()))
        .arr(json!(["lens"]), Arr::U32(lens.clone()));
    (m, values, levels, lens)
}

fn handle_load(app: &App, out: &Out, m: &Value, cancel: &AtomicBool, frameipc: bool) -> Result<(), String> {
    let scene = frameipc.then_some(&*app.scene);
    match m["type"].as_str().unwrap_or("") {
        "open" => {
            let part = m["part"].as_u64().unwrap_or(0) as usize;
            let t0 = Instant::now();
            let bytes = app.read_src(m["src"].as_str().unwrap_or(""))?;
            let fetch_ms = t0.elapsed().as_secs_f64() * 1e3;
            let t1 = Instant::now();
            let mut cap = catch(|| Capture::new(&bytes, part).ok())
                .map_err(|e| format!("reading: {e}"))?
                .ok_or("the file could not be read")?;
            let parse_ms = t1.elapsed().as_secs_f64() * 1e3;
            let n = cap.n_lines();
            let mut lines = vec![];
            let mut total = 0.0;
            for i in 0..n {
                lines.push(json!({"name": cap.line_name(i), "initial": cap.line_initial(i)}));
                total += cap.line_n_edges(i);
            }
            let analog: Vec<Value> = (0..cap.n_analog())
                .map(|i| json!({"name": cap.analog_name(i), "unit": cap.analog_unit(i), "t0": cap.analog_t0(i), "dt": cap.analog_dt(i)}))
                .collect();
            out.json(json!({
                "type": "meta", "tickHz": cap.tick_hz(), "nTicks": cap.n_ticks(), "nParts": cap.n_parts(), "lines": lines,
                "analog": analog, "total": total, "complete": cap.complete(), "lost": cap.lost(), "fetchMs": fetch_ms,
                "parseMs": parse_ms, "bytes": bytes.len(),
            }));
            if let Some(s) = scene {
                *s.lock().unwrap() = Some(Scene::new(cap.n_ticks()));
            }
            for i in 0..n {
                let edges = cap.line_edges(i);
                let initial = cap.line_initial(i);
                if let Some(s) = scene
                    && let Some(sc) = s.lock().unwrap().as_mut()
                {
                    sc.add_vec_line(initial, edges.clone());
                }
                out.send(
                    Msg::new(json!({"type": "edges", "i": i, "initial": initial}))
                        .arr(json!(["edges"]), Arr::I64(edges)),
                );
            }
            for i in 0..cap.n_analog() {
                let (msg, values, levels, lens) = analog_msg(i, AnalogParts::of(cap.analog_values(i)));
                if let Some(s) = scene
                    && let Some(sc) = s.lock().unwrap().as_mut()
                {
                    sc.add_analog_parts(
                        &cap.analog_name(i),
                        &cap.analog_unit(i),
                        cap.analog_t0(i),
                        cap.analog_dt(i),
                        values,
                        &levels,
                        &lens,
                    );
                }
                out.send(msg);
            }
            drop(bytes);
            let t3 = Instant::now();
            for i in 0..n {
                run_lod(out, i, cap.lod_job(i, 32), cancel, scene);
            }
            out.json(json!({"type": "lod-done", "ms": t3.elapsed().as_secs_f64() * 1e3}));
        }
        "synth-analog" => {
            let t = Instant::now();
            let (msg, ..) = analog_msg(
                0,
                AnalogParts::synth(m["n"].as_u64().unwrap_or(0) as u32, m["dt"].as_f64().unwrap_or(1.0)),
            );
            out.send(msg);
            out.json(json!({"type": "analog-ms", "ms": t.elapsed().as_secs_f64() * 1e3}));
        }
        "synth-line" => {
            let job = LodJob::synth(
                m["line"].as_u64().unwrap_or(0) as u32,
                m["nTicks"].as_f64().unwrap_or(0.0),
                m["period"].as_f64().unwrap_or(1.0),
                m["segLen"].as_f64().unwrap_or(1.0),
                32,
            );
            // the native scene's synth lines get their summaries from scene_synth
            run_lod(out, m["i"].as_u64().unwrap_or(0) as usize, job, cancel, None);
            out.json(json!({"type": "line-done", "i": m["i"]}));
        }
        _ => {}
    }
    Ok(())
}

fn send_analysis(out: &Out, a: &Analysis, ms: f64, scene: Option<&Mutex<Option<Scene>>>) {
    let meta: Value = serde_json::from_str(&a.meta()).unwrap_or(json!({}));
    let mut msg = Msg::new(json!({"type": "analysis", "ms": ms}));
    let mut layers = vec![];
    let mut unread = vec![];
    let mut sc_guard = scene.map(|s| s.lock().unwrap());
    let mut sc = sc_guard.as_mut().and_then(|g| g.as_mut());
    if let Some(sc) = sc.as_deref_mut() {
        sc.clear_analysis();
    }
    if let Some(claims) = meta["claims"].as_array() {
        for (ci, c) in claims.iter().enumerate() {
            for (li, ly) in c["layers"].as_array().into_iter().flatten().enumerate() {
                let k = layers.len();
                let s = a.layer_s(ci, li);
                let e = a.layer_e(ci, li);
                let status = a.layer_status(ci, li);
                let buf = a.layer_buf(ci, li);
                let off = a.layer_off(ci, li);
                if let Some(sc) = sc.as_deref_mut() {
                    sc.set_track(
                        ci as u32,
                        li as u32,
                        ly["name"].as_str().unwrap_or(""),
                        &s,
                        &e,
                        &status,
                        buf.clone(),
                        off.clone(),
                    );
                }
                let mut cols = vec![];
                for (kk, name) in ly["columns"].as_array().into_iter().flatten().enumerate() {
                    let kind = a.column_kind(ci, li, kk);
                    match kind {
                        0 => {
                            cols.push(json!({"name": name, "kind": 0}));
                            msg.push(
                                json!(["layers", k, "columns", kk, "data"]),
                                Arr::F64(a.column_num(ci, li, kk)),
                            );
                        }
                        1 => {
                            cols.push(json!({"name": name, "kind": 1}));
                            msg.push(
                                json!(["layers", k, "columns", kk, "data"]),
                                Arr::U8(a.column_bool(ci, li, kk)),
                            );
                        }
                        _ => cols.push(json!({"name": name, "kind": kind, "data": a.column_str(ci, li, kk)})),
                    }
                }
                layers.push(json!({"c": ci, "l": li, "name": ly["name"], "columns": cols}));
                msg.push(json!(["layers", k, "s"]), Arr::F64(s));
                msg.push(json!(["layers", k, "e"]), Arr::F64(e));
                msg.push(json!(["layers", k, "status"]), Arr::U8(status));
                msg.push(json!(["layers", k, "buf"]), Arr::U8(buf));
                msg.push(json!(["layers", k, "off"]), Arr::U32(off));
            }
            if c["unread"].as_u64().unwrap_or(0) > 0 {
                let k = unread.len();
                let s = a.unread_s(ci);
                let e = a.unread_e(ci);
                if let Some(sc) = sc.as_deref_mut() {
                    sc.set_unread(ci as u32, &s, &e);
                }
                unread.push(json!({"c": ci}));
                msg.push(json!(["unread", k, "s"]), Arr::F64(s));
                msg.push(json!(["unread", k, "e"]), Arr::F64(e));
            }
        }
    }
    drop(sc_guard);
    msg.head["meta"] = meta;
    msg.head["layers"] = json!(layers);
    msg.head["unread"] = json!(unread);
    msg.head["bytes"] = json!(msg.n_bytes());
    out.send(msg);
}

fn handle_analysis(app: &App, out: &Out, m: &Value, local: &mut Local, frameipc: bool) -> Result<(), String> {
    let scene = frameipc.then_some(&*app.scene);
    match m["type"].as_str().unwrap_or("") {
        "analyze" => {
            let bytes = app.read_src(m["src"].as_str().unwrap_or(""))?;
            let part = m["part"].as_u64().unwrap_or(0) as usize;
            let t = Instant::now();
            local.an = None;
            let an = catch(|| Analysis::run(&bytes, part)).map_err(|e| format!("analysis: {e}"))?;
            drop(bytes);
            send_analysis(out, &an, t.elapsed().as_secs_f64() * 1e3, scene);
            local.an = Some(an);
        }
        "fake" => {
            let t = Instant::now();
            local.an = None;
            let an = Analysis::fake(
                m["n"].as_f64().unwrap_or(0.0) as u32,
                m["nTicks"].as_f64().unwrap_or(0.0),
            );
            send_analysis(out, &an, t.elapsed().as_secs_f64() * 1e3, scene);
            local.an = Some(an);
        }
        "search" => {
            let Some(an) = local.an.as_mut() else { return Ok(()) };
            let t = Instant::now();
            let total = an.search(m["q"].as_str().unwrap_or(""));
            let pairs = an.hit_layers();
            let mut msg = Msg::new(json!({"type": "search", "id": m["id"], "q": m["q"], "total": total}));
            let mut layers = vec![];
            for k in 0..pairs.len() / 2 {
                layers.push(json!({"c": pairs[2 * k], "l": pairs[2 * k + 1]}));
                msg.push(json!(["layers", k, "rows"]), Arr::U32(an.hit_rows(k)));
            }
            msg.push(json!(["ticks"]), Arr::F64(an.hit_ticks()));
            msg.head["layers"] = json!(layers);
            msg.head["ms"] = json!(t.elapsed().as_secs_f64() * 1e3);
            out.send(msg);
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// commands

#[tauri::command]
fn worker_new(
    app: tauri::AppHandle,
    id: u32,
    kind: String,
    mode: String,
    frameipc: bool,
    on: Channel<InvokeResponseBody>,
) {
    let st = app.state::<App>();
    let mode = match mode.as_str() {
        "json" => Mode::Json,
        "chunk" => Mode::Chunk,
        "fetch" => Mode::Fetch,
        _ => Mode::Raw,
    };
    let out = Out {
        ch: on,
        mode,
        blobs: st.blobs.clone(),
        next_blob: st.next_blob.clone(),
    };
    let (tx, rx) = mpsc::channel::<Value>();
    let cancel = Arc::new(AtomicBool::new(false));
    st.workers.lock().unwrap().insert(
        id,
        Worker {
            tx,
            cancel: cancel.clone(),
        },
    );
    let h = app.clone();
    std::thread::Builder::new()
        .name(format!("{kind}-{id}"))
        .spawn(move || {
            let app = h.state::<App>();
            let mut local = Local { an: None };
            // the memory, as the wasm workers tell theirs (here: the whole process)
            if kind == "analysis" {
                let (o, c) = (out.clone(), cancel.clone());
                std::thread::spawn(move || {
                    while !c.load(Ordering::Relaxed) {
                        o.json(json!({"type": "mem", "bytes": rss()}));
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                });
            }
            for m in rx {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let r = if kind == "load" {
                    handle_load(&app, &out, &m, &cancel, frameipc)
                } else {
                    handle_analysis(&app, &out, &m, &mut local, frameipc)
                };
                if let Err(e) = r {
                    out.json(json!({"type": "error", "error": e}));
                }
            }
        })
        .expect("a worker's thread");
}

#[tauri::command]
fn worker_post(app: tauri::AppHandle, id: u32, msg: Value) {
    if let Some(w) = app.state::<App>().workers.lock().unwrap().get(&id) {
        let _ = w.tx.send(msg);
    }
}

#[tauri::command]
fn worker_end(app: tauri::AppHandle, id: u32) {
    if let Some(w) = app.state::<App>().workers.lock().unwrap().remove(&id) {
        w.cancel.store(true, Ordering::Relaxed);
    }
}

/// A File's bytes handed over by the page (a raw body): a key for `worker_post`'s `src`.
#[tauri::command]
fn put_bytes(app: tauri::AppHandle, request: Request<'_>) -> Result<String, String> {
    let InvokeBody::Raw(b) = request.body() else {
        return Err("a raw body expected".into());
    };
    let st = app.state::<App>();
    let k = st.next_mem.fetch_add(1, Ordering::Relaxed);
    let mut m = st.mem_bytes.lock().unwrap();
    m.clear();
    m.insert(k, Arc::new(b.clone()));
    Ok(format!("mem:{k}"))
}

/// The native scene made for a made-up capture (its lines' summaries and the analog line built here, two threads).
#[tauri::command]
fn scene_synth(app: tauri::AppHandle, total: f64) {
    let scene = app.state::<App>().scene.clone();
    std::thread::spawn(move || {
        let plan = synth_plan(total, 16);
        let n = plan[0];
        let mut sc = Scene::new(n);
        for i in 0..16 {
            sc.add_synth_line(i as u32, plan[1 + 2 * i], plan[2 + 2 * i]);
        }
        let mut a = AnalogParts::synth(4_000_000, n / 4e6);
        let (v, l, ln) = (a.take_values(), a.take_levels(), a.lens());
        sc.add_analog_parts("A0", "V", 0.0, n / 4e6, v, &l, &ln);
        *scene.lock().unwrap() = Some(sc);
        let next = Arc::new(AtomicU64::new(0));
        let hs: Vec<_> = (0..2)
            .map(|_| {
                let (scene, next, plan) = (scene.clone(), next.clone(), plan.clone());
                std::thread::spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed) as usize;
                        if i >= 16 {
                            break;
                        }
                        let mut job = LodJob::synth(i as u32, n, plan[1 + 2 * i], plan[2 + 2 * i], 32);
                        if let Some(sc) = scene.lock().unwrap().as_mut() {
                            sc.lod_begin(i, job.shift0());
                        }
                        loop {
                            let done = job.step(16);
                            let (c, e) = (job.take_counts(), job.take_ends());
                            if let Some(sc) = scene.lock().unwrap().as_mut() {
                                sc.lod_append(i, &c, &e, done);
                            }
                            if done {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();
        for h in hs {
            let _ = h.join();
        }
    });
}

fn frame_of(app: &App, f: &[f64]) -> Option<(u32, f64, Vec<f32>, Vec<f32>, String)> {
    // [12 vp][n lanes][lanes][n sel][sel][grid...]
    let vp = f.get(..12)?;
    let nl = *f.get(12)? as usize;
    let lanes = f.get(13..13 + nl)?;
    let ns = *f.get(13 + nl)? as usize;
    let sel = f.get(14 + nl..14 + nl + ns)?;
    let grid = f.get(14 + nl + ns..)?;
    let mut g = app.scene.lock().unwrap();
    let sc = g.as_mut()?;
    let t = Instant::now();
    let ni = sc.frame(vp, lanes, sel, grid);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let inst = unsafe { std::slice::from_raw_parts(sc.inst_ptr(), ni as usize * 8) }.to_vec();
    let nlab = sc.n_labels() as usize;
    let labels = unsafe { std::slice::from_raw_parts(sc.labels_ptr(), nlab * 3) }.to_vec();
    Some((ni, ms, inst, labels, sc.label_text()))
}

/// A frame made by the native scene, the shapes sent back raw: `[u32 n][u32 n labels][f32 ms][u32 text bytes]`,
/// the shapes (8 f32 each), the labels (3 f32 each), the text.
#[tauri::command]
fn frame(app: tauri::AppHandle, request: Request<'_>) -> Response {
    let InvokeBody::Raw(b) = request.body() else {
        return Response::new(vec![]);
    };
    let f: Vec<f64> = b
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let Some((ni, ms, inst, labels, text)) = frame_of(&app.state::<App>(), &f) else {
        return Response::new(vec![]);
    };
    let mut out = Vec::with_capacity(16 + inst.len() * 4 + labels.len() * 4 + text.len());
    out.extend_from_slice(&ni.to_le_bytes());
    out.extend_from_slice(&((labels.len() / 3) as u32).to_le_bytes());
    out.extend_from_slice(&(ms as f32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    for x in inst.iter().chain(&labels) {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.extend_from_slice(text.as_bytes());
    Response::new(out)
}

/// The same as `frame`, as JSON both ways.
#[tauri::command]
fn frame_json(app: tauri::AppHandle, f: Vec<f64>) -> Value {
    match frame_of(&app.state::<App>(), &f) {
        Some((ni, ms, inst, labels, text)) => json!({"n": ni, "ms": ms, "inst": inst, "labels": labels, "text": text}),
        None => json!(null),
    }
}

#[tauri::command]
fn mem() -> Value {
    let t = tree_rss();
    let total: u64 = t.iter().map(|x| x.1).sum();
    json!({"self": rss(), "total": total, "procs": t.iter().map(|(n, r)| json!([n, r])).collect::<Vec<_>>()})
}

#[tauri::command]
fn report(app: tauri::AppHandle, what: String, data: Value) {
    let ms = since_start_ms();
    let m = tree_rss();
    let total: u64 = m.iter().map(|x| x.1).sum();
    say(&format!(
        "[report {ms:.0} ms, rss {:.0} MB] {what} {data}",
        total as f64 / 1e6
    ));
    if what == "bench" && app.state::<App>().exit_after_bench {
        let procs: Vec<String> = m.iter().map(|(n, r)| format!("{n} {:.0}", *r as f64 / 1e6)).collect();
        say(&format!("[memory MB] {}", procs.join(", ")));
        app.exit(0);
    }
}

// ---------------------------------------------------------------------------------------------------------------

fn main() {
    T0.get_or_init(Instant::now);
    let mut roots = Roots::default();
    let mut query = String::new();
    let (mut w, mut h) = (1600.0, 960.0);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--files" => {
                roots
                    .dirs
                    .insert("files".into(), args.next().expect("--files DIR").into());
            }
            "--mount" => {
                let v = args.next().expect("--mount NAME=DIR");
                let (k, d) = v.split_once('=').expect("--mount NAME=DIR");
                roots.dirs.insert(k.into(), d.into());
            }
            "--log" => {
                let p = args.next().expect("--log FILE");
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .expect("--log FILE");
                let _ = LOG.set(Mutex::new(f));
            }
            "--size" => {
                let v = args.next().expect("--size WxH");
                let (a, b) = v.split_once('x').expect("--size WxH");
                (w, h) = (a.parse().expect("width"), b.parse().expect("height"));
            }
            q => query = q.trim_start_matches('?').to_string(),
        }
    }
    if !roots.dirs.contains_key("files") {
        // tests/data above the working directory (as serve.mjs' --files ../../../tests/data from web/)
        let mut d = std::env::current_dir().unwrap_or_default();
        loop {
            if d.join("tests/data").is_dir() {
                roots.dirs.insert("files".into(), d.join("tests/data"));
                break;
            }
            if !d.pop() {
                break;
            }
        }
    }
    let exit_after_bench = query.split('&').any(|kv| kv == "exit=1");
    let app_state = App {
        workers: Mutex::new(HashMap::new()),
        roots,
        mem_bytes: Mutex::new(HashMap::new()),
        next_mem: AtomicU64::new(0),
        blobs: Arc::new(Mutex::new(HashMap::new())),
        next_blob: Arc::new(AtomicU64::new(0)),
        scene: Arc::new(Mutex::new(None)),
        exit_after_bench,
    };
    let blobs = app_state.blobs.clone();
    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            worker_new,
            worker_post,
            worker_end,
            put_bytes,
            scene_synth,
            frame,
            frame_json,
            mem,
            report
        ])
        // capture files for the wasm workers (the roots of --files / --mount)
        .register_asynchronous_uri_scheme_protocol("wskfile", |ctx, req, responder| {
            let app = ctx.app_handle().clone();
            let path = req.uri().path().to_string();
            std::thread::spawn(move || {
                let st = app.state::<App>();
                let r = st
                    .resolve(&path)
                    .and_then(|p| std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display())));
                let b = tauri::http::Response::builder().header("Access-Control-Allow-Origin", "*");
                responder.respond(match r {
                    Ok(bytes) => b
                        .header("Content-Type", "application/octet-stream")
                        .body(bytes)
                        .unwrap(),
                    Err(e) => b.status(404).body(e.into_bytes()).unwrap(),
                });
            });
        })
        // the messages of `?ipc=fetch`: each fetched once
        .register_asynchronous_uri_scheme_protocol("wskblob", move |_ctx, req, responder| {
            let id: u64 = req.uri().path().trim_start_matches('/').parse().unwrap_or(u64::MAX);
            let b = blobs.lock().unwrap().remove(&id);
            let r = tauri::http::Response::builder().header("Access-Control-Allow-Origin", "*");
            responder.respond(match b {
                Some(bytes) => r
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes)
                    .unwrap(),
                None => r.status(404).body(vec![]).unwrap(),
            });
        })
        .setup(move |app| {
            let url = if query.is_empty() {
                "index.html".to_string()
            } else {
                format!("index.html?{query}")
            };
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App(url.into()))
                .title("WireSkein 試作 2b（Tauri）")
                .inner_size(w, h)
                .build()?;
            say(&format!("[start {:.0} ms] window made", since_start_ms()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the Tauri app");
}
