//! Zero-copy data sharing between a Rust core and QuickJS plugins (rquickjs).
//!
//! The core keeps the capture as a memory-mapped dense u16 sample file
//! (Arc<Mmap>) and per-channel edge lists (Arc<[u32]>). JS plugins see them as
//! typed arrays backed by that memory (no copy); results computed by Rust
//! kernels are handed over by moving the Vec into an ArrayBuffer (no copy).
//! Throwaway benchmark.
//!
//!   cargo run --release -- <dir> <capture-id> <plugins.js>

use std::ffi::c_void;
use std::fs::File;
use std::sync::Arc;
use std::time::Instant;

use memmap2::{Mmap, MmapOptions};
use rquickjs::{qjs, ArrayBuffer, Context, Ctx, Function, Object, Runtime, TypedArray, Value};

// ---------------------------------------------------------------- core data

struct Capture {
    samples: Arc<Mmap>, // dense u16, bit k = channel k
    n: usize,
    edges: Vec<Arc<[u32]>>,
    initial: Vec<u8>,
}

fn samples_u16(m: &Mmap) -> &[u16] {
    // the mapping is page aligned, so the cast to u16 is aligned
    unsafe { std::slice::from_raw_parts(m.as_ptr() as *const u16, m.len() / 2) }
}

fn load(dir: &str, id: &str) -> (Capture, serde_like::Meta) {
    let meta = serde_like::Meta::read(&format!("{dir}/{id}.json"));
    let f = File::open(format!("{dir}/{id}.u16")).unwrap();
    // private copy-on-write mapping: a stray JS write cannot modify the file
    let m = unsafe { MmapOptions::new().map_copy(&f).unwrap() };
    let m: Mmap = m.make_read_only().unwrap();
    let mut edges = vec![];
    for k in 0..meta.channels {
        let raw = std::fs::read(format!("{dir}/{id}.ch{k}.u32")).unwrap();
        let v: Vec<u32> = raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        edges.push(Arc::from(v.into_boxed_slice()));
    }
    let n = m.len() / 2;
    (Capture { samples: Arc::new(m), n, edges, initial: meta.initial.clone() }, meta)
}

/// Minimal JSON field extraction (no serde, to keep the crate small).
mod serde_like {
    pub struct Meta {
        pub channels: usize,
        pub initial: Vec<u8>,
        pub rate: f64,
        pub raw: String,
    }
    impl Meta {
        pub fn read(path: &str) -> Meta {
            let raw = std::fs::read_to_string(path).unwrap();
            let arr = |key: &str| -> String {
                let i = raw.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
                let j = raw[i..].find(']').unwrap() + i;
                raw[raw[i..].find('[').unwrap() + i + 1..j].to_string()
            };
            let channels = arr("channels").split(',').count();
            let initial = arr("initial").split(',').map(|s| s.trim().parse().unwrap()).collect();
            let i = raw.find("\"rate\":").unwrap() + 7;
            let rate = raw[i..].split(|c| c == ',' || c == '}').next().unwrap().trim().parse().unwrap();
            Meta { channels, initial, rate, raw }
        }
    }
}

// ---------------------------------------------------------------- zero-copy views

unsafe extern "C" fn free_holder<T>(_rt: *mut qjs::JSRuntime, opaque: *mut c_void, _ptr: *mut c_void) {
    drop(Box::from_raw(opaque as *mut T)); // releases the Arc when JS collects the buffer
}

/// ArrayBuffer over memory owned by an Arc (mmap or [u32]); the JS object keeps a clone alive.
fn external_buffer<'js, H: 'static>(ctx: &Ctx<'js>, holder: H, ptr: *const u8, len: usize) -> ArrayBuffer<'js> {
    let boxed = Box::into_raw(Box::new(holder));
    unsafe {
        let v = qjs::JS_NewArrayBuffer(ctx.as_raw().as_ptr(), ptr as *mut u8, len as _, Some(free_holder::<H>),
                                       boxed as *mut c_void, false);
        ArrayBuffer::from_value(Value::from_raw(ctx.clone(), v)).unwrap()
    }
}

fn samples_view<'js>(ctx: &Ctx<'js>, cap: &Capture) -> TypedArray<'js, u16> {
    let ab = external_buffer(ctx, cap.samples.clone(), cap.samples.as_ptr(), cap.samples.len());
    TypedArray::<u16>::from_arraybuffer(ab).unwrap()
}

fn edges_view<'js>(ctx: &Ctx<'js>, e: &Arc<[u32]>) -> TypedArray<'js, u32> {
    let ab = external_buffer(ctx, e.clone(), e.as_ptr() as *const u8, e.len() * 4);
    TypedArray::<u32>::from_arraybuffer(ab).unwrap()
}

// ---------------------------------------------------------------- kernels (core side)

/// Levels of `data_bit` sampled at the rising (or falling) edges of the clock channel.
fn sync_bits(cap: &Capture, clk: usize, data_bit: usize, rise: bool) -> (Vec<u32>, Vec<u8>) {
    let s = samples_u16(&cap.samples);
    let init = cap.initial[clk];
    let mut t = vec![];
    let mut bits = vec![];
    for (i, &e) in cap.edges[clk].iter().enumerate() {
        let after = init ^ (((i + 1) & 1) as u8);
        if (after == 1) == rise {
            t.push(e);
            bits.push(((s[e as usize] >> data_bit) & 1) as u8);
        }
    }
    (t, bits)
}

/// START/STOP delimited frames: data edges while the clock is high.
fn frames_startstop(cap: &Capture, clk: usize, data: usize, t: &[u32]) -> Vec<u32> {
    let s = samples_u16(&cap.samples);
    let init = cap.initial[data];
    let mut bounds = vec![];
    let mut open: Option<u32> = None;
    for (i, &e) in cap.edges[data].iter().enumerate() {
        if (s[e as usize] >> clk) & 1 == 0 {
            continue;
        }
        let after = init ^ (((i + 1) & 1) as u8);
        let p = t.partition_point(|&x| x < e) as u32;
        if after == 0 {
            if let Some(o) = open {
                bounds.extend_from_slice(&[o, p]);
            }
            open = Some(p);
        } else {
            if let Some(o) = open {
                bounds.extend_from_slice(&[o, p]);
            }
            open = None;
        }
    }
    bounds
}

/// Native I2C on the same frames, for comparison with the JS plugin.
fn i2c_native(bits: &[u8], bounds: &[u32]) -> (usize, usize) {
    let (mut txs, mut nbytes) = (0, 0);
    for f in bounds.chunks_exact(2) {
        let (a, b) = (f[0] as usize, f[1] as usize);
        let nw = (b - a) / 9;
        if nw > 0 {
            txs += 1;
            nbytes += nw;
        }
        let mut _acc = 0u32;
        for w in 0..nw {
            let mut v = 0u32;
            for k in 0..9 {
                v = (v << 1) | bits[a + w * 9 + k] as u32;
            }
            _acc ^= v;
        }
    }
    (txs, nbytes)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (dir, id, plugins) = (&args[1], &args[2], &args[3]);
    let t0 = Instant::now();
    let (cap, meta) = load(dir, id);
    println!("== {id}: {} samples ({:.1} MB mapped), {} channels, load {:.1} ms",
             cap.n, cap.samples.len() as f64 / 1e6, cap.edges.len(), ms(t0));
    let busiest = (0..cap.edges.len()).max_by_key(|&k| cap.edges[k].len()).unwrap();

    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        let src = std::fs::read_to_string(plugins).unwrap();
        ctx.eval::<(), _>(src + "\nglobalThis.WS = WS;").unwrap();
        let g = ctx.globals();

        // 1. handing the whole sample buffer to JS: zero-copy view vs copy
        let reps = 20;
        let t = Instant::now();
        for _ in 0..reps {
            let v = samples_view(&ctx, &cap);
            std::hint::black_box(v.len());
        }
        let view_ms = ms(t) / reps as f64;
        let t = Instant::now();
        for _ in 0..reps {
            let v = TypedArray::<u16>::new_copy(ctx.clone(), samples_u16(&cap.samples)).unwrap();
            std::hint::black_box(v.len());
        }
        let copy_ms = ms(t) / reps as f64;
        println!("  hand {:.1} MB of samples to JS: zero-copy view {:.4} ms, copy {:.2} ms",
                 cap.samples.len() as f64 / 1e6, view_ms, copy_ms);

        // 2. who scans raw samples: JS over the view vs Rust
        g.set("samples", samples_view(&ctx, &cap)).unwrap();
        let scan: Function = ctx.eval(format!(
            "(function(){{ var s = samples, n = s.length, b = {busiest}, c = 0, p = s[0] >> b & 1;
               for (var i = 1; i < n; i++) {{ var v = s[i] >> b & 1; if (v !== p) {{ c++; p = v; }} }} return c; }})")).unwrap();
        let t = Instant::now();
        let js_edges: u32 = scan.call(()).unwrap();
        let js_scan = ms(t);
        let t = Instant::now();
        let s = samples_u16(&cap.samples);
        let mut c = 0u32;
        let mut p = s[0] >> busiest & 1;
        for &v in &s[1..] {
            let v = v >> busiest & 1;
            if v != p {
                c += 1;
                p = v;
            }
        }
        let rs_scan = ms(t);
        println!("  scan all samples for edges of ch{busiest}: JS {js_scan:.1} ms ({js_edges}), Rust {rs_scan:.1} ms ({c}) -> JS/Rust x{:.0}",
                 js_scan / rs_scan.max(1e-6));

        // 3. edge lists as zero-copy views; JS walks them
        let t = Instant::now();
        let ev = edges_view(&ctx, &cap.edges[busiest]);
        g.set("edges", ev).unwrap();
        let sum: Function = ctx.eval("(function(){ var e = edges, s = 0; for (var i = 1; i < e.length; i++) s += e[i] - e[i-1]; return s; })").unwrap();
        let _: f64 = sum.call(()).unwrap();
        println!("  walk {} edges of ch{busiest} in JS over a zero-copy view: {:.2} ms", cap.edges[busiest].len(), ms(t));

        // 4. I2C: Rust kernels -> JS plugin (Vecs moved into ArrayBuffers, no copy)
        let buses = &meta.raw;
        let find = |role: &str| -> Option<usize> {
            let i = buses.find(&format!("\"{role}\": \"D"))? + role.len() + 6;
            buses[i..].split('"').next()?.parse().ok()
        };
        let (clk, dat) = match (find("scl"), find("sda")) {
            (Some(a), Some(b)) => (a, b),
            _ => (find("clk").unwrap_or(0), find("dio").unwrap_or(1)),
        };
        let t = Instant::now();
        let (tt, bits) = sync_bits(&cap, clk, dat, true);
        let k_sync = ms(t);
        let t = Instant::now();
        let bounds = frames_startstop(&cap, clk, dat, &tt);
        let k_frames = ms(t);
        let n_bits = bits.len();
        let t = Instant::now();
        let (ntx, nb) = i2c_native(&bits, &bounds);
        let native_ms = ms(t);
        let t = Instant::now();
        let s_obj = Object::new(ctx.clone()).unwrap();
        s_obj.set("bits", TypedArray::<u8>::new(ctx.clone(), bits).unwrap()).unwrap();
        s_obj.set("t", TypedArray::<u32>::new(ctx.clone(), tt).unwrap()).unwrap();
        s_obj.set("bounds", TypedArray::<u32>::new(ctx.clone(), bounds).unwrap()).unwrap();
        s_obj.set("sample_edge", "rise").unwrap();
        s_obj.set("n_data", 1).unwrap();
        s_obj.set("clock", format!("D{clk}")).unwrap();
        s_obj.set("data", vec![format!("D{dat}")]).unwrap();
        s_obj.set("clk_score", 1.0).unwrap();
        s_obj.set("pair_score", 1.0).unwrap();
        let handoff = ms(t);
        let ws: Object = g.get("WS").unwrap();
        let i2c: Function = ws.get("i2c").unwrap();
        let t = Instant::now();
        let out: Object = i2c.call((s_obj,)).unwrap();
        let js_ms = ms(t);
        let nodes: rquickjs::Array = out.get("nodes").unwrap();
        let ntx_js = if nodes.len() > 0 {
            let n0: Object = nodes.get(0).unwrap();
            let txs: rquickjs::Array = n0.get("output").unwrap();
            // first few transactions, printed for a check against the reference
            let mut shown = vec![];
            for i in 0..txs.len().min(3) {
                let x: Object = txs.get(i).unwrap();
                let addr: i32 = x.get("addr").unwrap();
                let rw: String = x.get("rw").unwrap();
                let b: Vec<i32> = x.get("bytes").unwrap();
                shown.push(format!("{addr:#x} {rw} {b:02x?}"));
            }
            println!("  JS i2c first transactions: {shown:?}");
            txs.len()
        } else {
            0
        };
        println!("  I2C on ch{clk}/ch{dat}: kernels sync_bits {k_sync:.2} ms + frames {k_frames:.2} ms ({n_bits} bits); \
                  hand-off to JS {handoff:.3} ms; JS plugin {js_ms:.2} ms ({ntx_js} transactions); native {native_ms:.3} ms ({ntx} tx, {nb} words)");

        // 5. call overheads
        let f = Function::new(ctx.clone(), |x: i32| x + 1).unwrap();
        g.set("hostInc", f).unwrap();
        let loop_fn: Function = ctx.eval("(function(n){ var x = 0; for (var i = 0; i < n; i++) x = hostInc(x); return x; })").unwrap();
        let n = 1_000_000;
        let t = Instant::now();
        let _: i32 = loop_fn.call((n,)).unwrap();
        let per_host = ms(t) * 1e3 / n as f64;
        let js_inc: Function = ctx.eval("(function(x){ return x + 1; })").unwrap();
        let t = Instant::now();
        let mut x = 0i32;
        for _ in 0..100_000 {
            x = js_inc.call((x,)).unwrap();
        }
        let per_js = ms(t) * 1e3 / 100_000.0;
        println!("  call overhead: JS -> Rust host fn {per_host:.3} us/call; Rust -> JS fn {per_js:.3} us/call");
        let _ = meta.rate;
    });
}
