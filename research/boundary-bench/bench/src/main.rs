//! Core/plugin boundary benchmark: the same UART hypothesis sweep through
//! different boundaries and call granularities. Throwaway.
//!
//!   cargo run --release -p bench -- <edges.i64> <meta.json>
//!   (internally re-executes itself with --serve for the IPC variants)

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

use wsk::*;

struct HostCtx {
    edges: Vec<i64>,
    initial: u8,
    n: i64,
}

fn load(edges_path: &str, meta_path: &str) -> (HostCtx, f64, Vec<f64>) {
    let mut raw = vec![];
    std::fs::File::open(edges_path).unwrap().read_to_end(&mut raw).unwrap();
    let edges: Vec<i64> = raw.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
    let meta = std::fs::read_to_string(meta_path).unwrap();
    let num = |k: &str| -> f64 {
        let i = meta.find(&format!("\"{k}\":")).unwrap() + k.len() + 3;
        meta[i..].trim_start().split(|c| c == ',' || c == '}').next().unwrap().trim().parse().unwrap()
    };
    let ui = meta.find("\"units\":").unwrap();
    let ue = meta[ui..].find(']').unwrap() + ui;
    let units = meta[meta[ui..].find('[').unwrap() + ui + 1..ue].split(',').map(|s| s.trim().parse().unwrap()).collect();
    (HostCtx { edges, initial: num("initial") as u8, n: num("n_samples") as i64 }, num("rate"), units)
}

// ---------------- host API for the cdylib plugin ----------------
#[repr(C)]
struct HostApi {
    ctx: *const core::ffi::c_void,
    edges: extern "C" fn(*const core::ffi::c_void, *mut usize) -> *const i64,
    starts: extern "C" fn(*const core::ffi::c_void, u8, *mut f64, usize) -> usize,
    sample_grid: extern "C" fn(*const core::ffi::c_void, *const f64, usize, f64, u8, *mut u8),
    chain: extern "C" fn(*const f64, *const f64, usize, *mut u32) -> usize,
    in_windows: extern "C" fn(*const core::ffi::c_void, *const f64, *const f64, usize, *mut i32),
}

fn ctx_of<'a>(c: *const core::ffi::c_void) -> &'a HostCtx {
    unsafe { &*(c as *const HostCtx) }
}
extern "C" fn api_edges(c: *const core::ffi::c_void, len: *mut usize) -> *const i64 {
    let h = ctx_of(c);
    unsafe { *len = h.edges.len() };
    h.edges.as_ptr()
}
extern "C" fn api_starts(c: *const core::ffi::c_void, idle: u8, out: *mut f64, cap: usize) -> usize {
    let h = ctx_of(c);
    let mut v = vec![];
    k_starts(&h.edges, h.initial, idle, &mut v);
    let n = v.len().min(cap);
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n
}
extern "C" fn api_grid(c: *const core::ffi::c_void, s: *const f64, n: usize, t: f64, idle: u8, out: *mut u8) {
    let h = ctx_of(c);
    let starts = unsafe { std::slice::from_raw_parts(s, n) };
    let mut v = vec![];
    k_sample_grid(&h.edges, h.initial, h.n, starts, t, 13, idle, &mut v);
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len()) };
}
extern "C" fn api_chain(s: *const f64, b: *const f64, n: usize, out: *mut u32) -> usize {
    let (s, b) = unsafe { (std::slice::from_raw_parts(s, n), std::slice::from_raw_parts(b, n)) };
    let mut v = vec![];
    k_chain(s, b, &mut v);
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len()) };
    v.len()
}
extern "C" fn api_inwin(c: *const core::ffi::c_void, ws: *const f64, we: *const f64, nw: usize, out: *mut i32) {
    let h = ctx_of(c);
    let (ws, we) = unsafe { (std::slice::from_raw_parts(ws, nw), std::slice::from_raw_parts(we, nw)) };
    let mut v = vec![];
    k_in_windows(ws, we, &h.edges, &mut v);
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, v.len()) };
}

// ---------------- wasm host ----------------
struct WasmHost {
    ctx: HostCtx,
    calls: u64,
    bytes: u64,
}

fn rd_f64(mem: &[u8], ptr: u32, n: usize) -> Vec<f64> {
    (0..n).map(|i| f64::from_le_bytes(mem[ptr as usize + 8 * i..ptr as usize + 8 * i + 8].try_into().unwrap())).collect()
}

fn wasm_setup(path: &str, ctx: HostCtx) -> anyhow_lite::Result<(wasmtime::Store<WasmHost>, wasmtime::Instance)> {
    use wasmtime::*;
    let engine = Engine::default();
    let module = Module::from_file(&engine, path).map_err(anyhow_lite::e)?;
    let mut store = Store::new(&engine, WasmHost { ctx, calls: 0, bytes: 0 });
    let mut linker: Linker<WasmHost> = Linker::new(&engine);
    linker.func_wrap("host", "h_starts", |mut c: Caller<'_, WasmHost>, idle: u32, out: u32, cap: u32| -> u32 {
        let mem = c.get_export("memory").unwrap().into_memory().unwrap();
        let (data, h) = mem.data_and_store_mut(&mut c);
        let mut v = vec![];
        k_starts(&h.ctx.edges, h.ctx.initial, idle as u8, &mut v);
        let n = v.len().min(cap as usize);
        for (i, x) in v[..n].iter().enumerate() {
            data[out as usize + 8 * i..out as usize + 8 * i + 8].copy_from_slice(&x.to_le_bytes());
        }
        h.calls += 1;
        h.bytes += 8 * n as u64;
        n as u32
    }).map_err(anyhow_lite::e)?;
    linker.func_wrap("host", "h_sample_grid", |mut c: Caller<'_, WasmHost>, s: u32, n: u32, t: f64, idle: u32, out: u32| {
        let mem = c.get_export("memory").unwrap().into_memory().unwrap();
        let (data, h) = mem.data_and_store_mut(&mut c);
        let starts = rd_f64(data, s, n as usize);
        let mut v = vec![];
        k_sample_grid(&h.ctx.edges, h.ctx.initial, h.ctx.n, &starts, t, 13, idle as u8, &mut v);
        data[out as usize..out as usize + v.len()].copy_from_slice(&v);
        h.calls += 1;
        h.bytes += 8 * n as u64 + v.len() as u64;
    }).map_err(anyhow_lite::e)?;
    linker.func_wrap("host", "h_chain", |mut c: Caller<'_, WasmHost>, s: u32, b: u32, n: u32, out: u32| -> u32 {
        let mem = c.get_export("memory").unwrap().into_memory().unwrap();
        let (data, h) = mem.data_and_store_mut(&mut c);
        let (sv, bv) = (rd_f64(data, s, n as usize), rd_f64(data, b, n as usize));
        let mut v = vec![];
        k_chain(&sv, &bv, &mut v);
        for (i, x) in v.iter().enumerate() {
            data[out as usize + 4 * i..out as usize + 4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        h.calls += 1;
        h.bytes += 16 * n as u64 + 4 * v.len() as u64;
        v.len() as u32
    }).map_err(anyhow_lite::e)?;
    linker.func_wrap("host", "h_in_windows", |mut c: Caller<'_, WasmHost>, ws: u32, we: u32, nw: u32, out: u32| {
        let mem = c.get_export("memory").unwrap().into_memory().unwrap();
        let (data, h) = mem.data_and_store_mut(&mut c);
        let (a, b) = (rd_f64(data, ws, nw as usize), rd_f64(data, we, nw as usize));
        let mut v = vec![];
        k_in_windows(&a, &b, &h.ctx.edges, &mut v);
        for (i, x) in v.iter().enumerate() {
            data[out as usize + 4 * i..out as usize + 4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        h.calls += 1;
        h.bytes += 16 * nw as u64 + 4 * v.len() as u64;
    }).map_err(anyhow_lite::e)?;
    let inst = linker.instantiate(&mut store, &module).map_err(anyhow_lite::e)?;
    Ok((store, inst))
}

mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
    pub fn e<E: std::fmt::Display>(x: E) -> String {
        x.to_string()
    }
}

// ---------------- IPC server ----------------
fn serve(edges_path: &str, meta_path: &str, json: bool) {
    let (ctx, _, _) = load(edges_path, meta_path);
    let mut s = Scratch::default();
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    if json {
        for line in stdin.lock().lines() {
            let line = line.unwrap();
            if line.is_empty() {
                break;
            }
            // {"t":..,"idle":..,"db":..,"par":..,"sb":..}
            let v: Vec<f64> = line.trim_matches(|c| c == '{' || c == '}').split(',')
                .map(|kv| kv.split(':').nth(1).unwrap().parse().unwrap()).collect();
            let p = UartParams { t: v[0], idle: v[1] as u8, data_bits: v[2] as u8, parity: v[3] as u8, stop_bits: v[4] as u8 };
            let m = uart_eval(&ctx.edges, ctx.initial, ctx.n, &p, &mut s);
            writeln!(out, "{{\"frames\":{},\"frames_ok\":{},\"explained\":{},\"edges\":{}}}", m.frames, m.frames_ok, m.explained_edges, m.n_edges).unwrap();
            out.flush().unwrap();
        }
    } else {
        let mut inp = stdin.lock();
        loop {
            let mut hdr = [0u8; 4];
            if inp.read_exact(&mut hdr).is_err() {
                break;
            }
            let n = u32::from_le_bytes(hdr) as usize;
            if n == 0 {
                break;
            }
            let mut buf = vec![0u8; 16 * n];
            inp.read_exact(&mut buf).unwrap();
            for r in buf.chunks_exact(16) {
                let p = UartParams { t: f64::from_le_bytes(r[..8].try_into().unwrap()), idle: r[8], data_bits: r[9], parity: r[10], stop_bits: r[11] };
                let m = uart_eval(&ctx.edges, ctx.initial, ctx.n, &p, &mut s);
                for x in [m.frames, m.frames_ok, m.explained_edges, m.n_edges] {
                    out.write_all(&x.to_le_bytes()).unwrap();
                }
            }
            out.flush().unwrap();
        }
    }
}

fn enc(p: &UartParams) -> [u8; 16] {
    let mut r = [0u8; 16];
    r[..8].copy_from_slice(&p.t.to_le_bytes());
    r[8..12].copy_from_slice(&[p.idle, p.data_bits, p.parity, p.stop_bits]);
    r
}

fn report(name: &str, n: usize, secs: f64, ok: bool, extra: &str) {
    println!("{name:28} {:>10.2} us/hyp {:>9.3} s  match={ok} {extra}", secs / n as f64 * 1e6, secs);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--serve") {
        serve(&args[2], &args[3], args.get(4).map(|s| s == "json").unwrap_or(false));
        return;
    }
    let (edges_path, meta_path) = (&args[1], &args[2]);
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    let (ctx, rate, units) = load(edges_path, meta_path);
    let hyps = hypotheses(rate, &units);
    let reps = 5;
    println!("edges={} hypotheses={} reps={}", ctx.edges.len(), hyps.len(), reps);

    // native, per hypothesis (grid cache shared across formats as in Python)
    let mut s = Scratch::default();
    let t0 = Instant::now();
    let mut base = vec![];
    for _ in 0..reps {
        base.clear();
        for p in &hyps {
            base.push(uart_eval(&ctx.edges, ctx.initial, ctx.n, p, &mut s));
        }
    }
    report("native per-hyp", hyps.len() * reps, t0.elapsed().as_secs_f64(), true, "");
    let best = base.iter().zip(&hyps).max_by_key(|(m, _)| m.explained_edges).unwrap();
    println!("  best: t={:.2} idle={} db={} par={} sb={} -> {:?}", best.1.t, best.1.idle, best.1.data_bits, best.1.parity, best.1.stop_bits, best.0);

    // native batched
    let t0 = Instant::now();
    let mut out = vec![];
    for _ in 0..reps {
        uart_eval_grid(&ctx.edges, ctx.initial, ctx.n, &hyps, &mut out);
    }
    report("native grid (1 call/channel)", hyps.len() * reps, t0.elapsed().as_secs_f64(), out == base, "");

    // cdylib plugin, kernels via host fn pointers
    unsafe {
        let lib = libloading::Library::new(target.join("release/libplugin_cdylib.so")).unwrap();
        let f: libloading::Symbol<extern "C" fn(&HostApi, &UartParams, &mut UartMetrics)> = lib.get(b"plugin_uart").unwrap();
        let api = HostApi { ctx: &ctx as *const HostCtx as *const _, edges: api_edges, starts: api_starts,
                            sample_grid: api_grid, chain: api_chain, in_windows: api_inwin };
        let t0 = Instant::now();
        let mut res = vec![UartMetrics::default(); hyps.len()];
        for _ in 0..reps {
            for (p, r) in hyps.iter().zip(res.iter_mut()) {
                f(&api, p, r);
            }
        }
        report("cdylib glue + host kernels", hyps.len() * reps, t0.elapsed().as_secs_f64(), res == base, "");
    }

    // wasm
    let wasm_path = target.join("wasm32-unknown-unknown/release/plugin_wasm.wasm");
    let t_inst = Instant::now();
    let (mut store, inst) = wasm_setup(wasm_path.to_str().unwrap(), HostCtx { edges: ctx.edges.clone(), initial: ctx.initial, n: ctx.n }).unwrap();
    let mem = inst.get_memory(&mut store, "memory").unwrap();
    let alloc = inst.get_typed_func::<u32, u32>(&mut store, "alloc").unwrap();
    let set_edges = inst.get_typed_func::<(u32, u32, u32, i64), ()>(&mut store, "set_edges").unwrap();
    let eptr = alloc.call(&mut store, (ctx.edges.len() * 8) as u32).unwrap();
    let bytes: Vec<u8> = ctx.edges.iter().flat_map(|e| e.to_le_bytes()).collect();
    mem.write(&mut store, eptr as usize, &bytes).unwrap();
    set_edges.call(&mut store, (eptr, ctx.edges.len() as u32, ctx.initial as u32, ctx.n)).unwrap();
    let outp = alloc.call(&mut store, 16).unwrap();
    println!("  wasm instantiate + edge copy: {:.1} ms", t_inst.elapsed().as_secs_f64() * 1e3);
    for (name, export) in [("wasm compute-in-plugin", "uart_local"), ("wasm glue + host kernels", "uart_kernels")] {
        let f = inst.get_typed_func::<(f64, u32, u32, u32, u32, u32), ()>(&mut store, export).unwrap();
        store.data_mut().calls = 0;
        store.data_mut().bytes = 0;
        let t0 = Instant::now();
        let mut res = vec![];
        for _ in 0..reps {
            res.clear();
            for p in &hyps {
                f.call(&mut store, (p.t, p.idle as u32, p.data_bits as u32, p.parity as u32, p.stop_bits as u32, outp)).unwrap();
                let mut b = [0u8; 16];
                mem.read(&store, outp as usize, &mut b).unwrap();
                let u = |i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
                res.push(UartMetrics { frames: u(0), frames_ok: u(1), explained_edges: u(2), n_edges: u(3) });
            }
        }
        let d = store.data();
        let extra = if d.calls > 0 { format!("host calls/hyp={:.1} copied bytes/hyp={:.0}", d.calls as f64 / (hyps.len() * reps) as f64, d.bytes as f64 / (hyps.len() * reps) as f64) } else { String::new() };
        report(name, hyps.len() * reps, t0.elapsed().as_secs_f64(), res == base, &extra);
    }

    // IPC: binary per call, binary batched, JSON per call
    let exe = std::env::current_exe().unwrap();
    for (name, json, batch) in [("ipc binary per-hyp", false, false), ("ipc binary batched", false, true), ("ipc json per-hyp", true, false)] {
        let mut child = Command::new(&exe).args(["--serve", edges_path, meta_path, if json { "json" } else { "bin" }])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut w = child.stdin.take().unwrap();
        let mut r = BufReader::new(child.stdout.take().unwrap());
        let t0 = Instant::now();
        let mut res = vec![];
        for _ in 0..reps {
            res.clear();
            if batch {
                let mut msg = (hyps.len() as u32).to_le_bytes().to_vec();
                for p in &hyps {
                    msg.extend_from_slice(&enc(p));
                }
                w.write_all(&msg).unwrap();
                w.flush().unwrap();
                let mut b = vec![0u8; 16 * hyps.len()];
                r.read_exact(&mut b).unwrap();
                for c in b.chunks_exact(16) {
                    let u = |i: usize| u32::from_le_bytes(c[4 * i..4 * i + 4].try_into().unwrap());
                    res.push(UartMetrics { frames: u(0), frames_ok: u(1), explained_edges: u(2), n_edges: u(3) });
                }
            } else {
                for p in &hyps {
                    if json {
                        writeln!(w, "{{\"t\":{},\"idle\":{},\"db\":{},\"par\":{},\"sb\":{}}}", p.t, p.idle, p.data_bits, p.parity, p.stop_bits).unwrap();
                        w.flush().unwrap();
                        let mut line = String::new();
                        r.read_line(&mut line).unwrap();
                        let v: Vec<u32> = line.trim().trim_matches(|c| c == '{' || c == '}').split(',')
                            .map(|kv| kv.split(':').nth(1).unwrap().parse().unwrap()).collect();
                        res.push(UartMetrics { frames: v[0], frames_ok: v[1], explained_edges: v[2], n_edges: v[3] });
                    } else {
                        let mut msg = 1u32.to_le_bytes().to_vec();
                        msg.extend_from_slice(&enc(p));
                        w.write_all(&msg).unwrap();
                        w.flush().unwrap();
                        let mut b = [0u8; 16];
                        r.read_exact(&mut b).unwrap();
                        let u = |i: usize| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
                        res.push(UartMetrics { frames: u(0), frames_ok: u(1), explained_edges: u(2), n_edges: u(3) });
                    }
                }
            }
        }
        report(name, hyps.len() * reps, t0.elapsed().as_secs_f64(), res == base, "");
        if json {
            writeln!(w).unwrap();
        } else {
            w.write_all(&0u32.to_le_bytes()).unwrap();
        }
        drop(w);
        child.wait().unwrap();
    }
}
