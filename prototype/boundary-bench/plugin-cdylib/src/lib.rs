//! Native plugin (cdylib, C ABI). The UART glue lives here; every numeric
//! kernel is called back into the host through a table of function pointers.

use wsk::{frame_len, uart_glue, UartMetrics, UartParams};

#[repr(C)]
pub struct HostApi {
    pub ctx: *const core::ffi::c_void,
    pub edges: extern "C" fn(ctx: *const core::ffi::c_void, len: *mut usize) -> *const i64,
    pub starts: extern "C" fn(ctx: *const core::ffi::c_void, idle: u8, out: *mut f64, cap: usize) -> usize,
    pub sample_grid: extern "C" fn(ctx: *const core::ffi::c_void, starts: *const f64, n: usize, t: f64, idle: u8, out: *mut u8),
    pub chain: extern "C" fn(starts: *const f64, busy: *const f64, n: usize, out: *mut u32) -> usize,
    pub in_windows: extern "C" fn(ctx: *const core::ffi::c_void, ws: *const f64, we: *const f64, nw: usize, out: *mut i32),
}

struct State {
    key: Option<(u64, u8)>,
    starts: Vec<f64>,
    grid: Vec<u8>,
}

static mut STATE: State = State { key: None, starts: Vec::new(), grid: Vec::new() };

#[no_mangle]
pub extern "C" fn plugin_uart(host: &HostApi, p: &UartParams, out: &mut UartMetrics) {
    #[allow(static_mut_refs)]
    let st = unsafe { &mut STATE };
    let mut n_edges = 0usize;
    let ep = (host.edges)(host.ctx, &mut n_edges);
    let edges = unsafe { std::slice::from_raw_parts(ep, n_edges) };
    let key = (p.t.to_bits(), p.idle);
    if st.key != Some(key) {
        st.starts.resize(n_edges, 0.0);
        let n = (host.starts)(host.ctx, p.idle, st.starts.as_mut_ptr(), n_edges);
        st.starts.truncate(n);
        st.grid.resize(n * 13, 0);
        (host.sample_grid)(host.ctx, st.starts.as_ptr(), n, p.t, p.idle, st.grid.as_mut_ptr());
        st.key = Some(key);
    }
    let (nbits, frame) = frame_len(p);
    let busy: Vec<f64> = st.starts.iter().map(|&x| x + (nbits as f64 + 0.5) * p.t).collect();
    let mut idx = vec![0u32; st.starts.len()];
    let ni = (host.chain)(st.starts.as_ptr(), busy.as_ptr(), st.starts.len(), idx.as_mut_ptr());
    idx.truncate(ni);
    let fs: Vec<f64> = idx.iter().map(|&i| st.starts[i as usize]).collect();
    let fe: Vec<f64> = fs.iter().map(|&s| s + frame * p.t).collect();
    let mut win = vec![0i32; n_edges];
    (host.in_windows)(host.ctx, fs.as_ptr(), fe.as_ptr(), fs.len(), win.as_mut_ptr());
    *out = uart_glue(p, edges, &st.starts, &st.grid, &idx, &win, &fs);
}
