//! WASM plugin. Two variants:
//! - uart_kernels: the glue runs in wasm, numeric kernels are host imports that
//!   read/write this module's linear memory (per-kernel-call boundary).
//! - uart_local: everything runs in wasm on a local copy of the edges.

use wsk::{frame_len, uart_eval, uart_glue, Scratch, UartMetrics, UartParams};

#[link(wasm_import_module = "host")]
extern "C" {
    fn h_starts(idle: u32, out: *mut f64, cap: u32) -> u32;
    fn h_sample_grid(starts: *const f64, n: u32, t: f64, idle: u32, out: *mut u8);
    fn h_chain(starts: *const f64, busy: *const f64, n: u32, out: *mut u32) -> u32;
    fn h_in_windows(ws: *const f64, we: *const f64, nw: u32, out: *mut i32);
}

static mut EDGES: Vec<i64> = Vec::new();
static mut INITIAL: u8 = 0;
static mut NSAMP: i64 = 0;
static mut SCRATCH: Option<Scratch> = None;
static mut KEY: Option<(u64, u8)> = None;
static mut STARTS: Vec<f64> = Vec::new();
static mut GRID: Vec<u8> = Vec::new();

#[no_mangle]
pub extern "C" fn alloc(n: u32) -> *mut u8 {
    let mut v = vec![0u8; n as usize];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Copy edges into the plugin (one-time cost measured separately).
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn set_edges(ptr: *const i64, len: u32, initial: u32, n_samples: i64) {
    unsafe {
        EDGES = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        INITIAL = initial as u8;
        NSAMP = n_samples;
        SCRATCH = Some(Scratch::default());
        KEY = None;
    }
}

fn params(t: f64, idle: u32, db: u32, par: u32, sb: u32) -> UartParams {
    UartParams { t, idle: idle as u8, data_bits: db as u8, parity: par as u8, stop_bits: sb as u8 }
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn uart_local(t: f64, idle: u32, db: u32, par: u32, sb: u32, out: *mut UartMetrics) {
    let p = params(t, idle, db, par, sb);
    unsafe {
        let m = uart_eval(&EDGES, INITIAL, NSAMP, &p, SCRATCH.as_mut().unwrap());
        *out = m;
    }
}

#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn uart_kernels(t: f64, idle: u32, db: u32, par: u32, sb: u32, out: *mut UartMetrics) {
    let p = params(t, idle, db, par, sb);
    unsafe {
        let n_edges = EDGES.len();
        let key = (p.t.to_bits(), p.idle);
        if KEY != Some(key) {
            STARTS.resize(n_edges, 0.0);
            let n = h_starts(idle, STARTS.as_mut_ptr(), n_edges as u32) as usize;
            STARTS.truncate(n);
            GRID.resize(n * 13, 0);
            h_sample_grid(STARTS.as_ptr(), n as u32, t, idle, GRID.as_mut_ptr());
            KEY = Some(key);
        }
        let (nbits, frame) = frame_len(&p);
        let busy: Vec<f64> = STARTS.iter().map(|&x| x + (nbits as f64 + 0.5) * t).collect();
        let mut idx = vec![0u32; STARTS.len()];
        let ni = h_chain(STARTS.as_ptr(), busy.as_ptr(), STARTS.len() as u32, idx.as_mut_ptr()) as usize;
        idx.truncate(ni);
        let fs: Vec<f64> = idx.iter().map(|&i| STARTS[i as usize]).collect();
        let fe: Vec<f64> = fs.iter().map(|&s| s + frame * t).collect();
        let mut win = vec![0i32; n_edges];
        h_in_windows(fs.as_ptr(), fe.as_ptr(), fs.len() as u32, win.as_mut_ptr());
        *out = uart_glue(&p, &EDGES, &STARTS, &GRID, &idx, &win, &fs);
    }
}
