//! Core kernels (the Rust counterpart of prototype/wsproto/kernels.py) and a
//! UART hypothesis evaluation built from them. Throwaway benchmark code.

/// Number of edges <= x (edges sorted).
#[inline]
fn upper_bound(edges: &[i64], x: i64) -> usize {
    edges.partition_point(|&e| e <= x)
}

/// Start candidates: edges after which the line leaves the idle level.
pub fn k_starts(edges: &[i64], initial: u8, idle: u8, out: &mut Vec<f64>) {
    out.clear();
    for (i, &e) in edges.iter().enumerate() {
        let after = initial ^ (((i + 1) & 1) as u8);
        if after == 1 - idle {
            out.push(e as f64);
        }
    }
}

/// Levels at starts[i] + (k + 0.5) * t for k in 0..nbits, row-major, inverted when idle == 0
/// so that 1 always means "idle level".
pub fn k_sample_grid(edges: &[i64], initial: u8, n: i64, starts: &[f64], t: f64, nbits: usize, idle: u8, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(starts.len() * nbits);
    // starts are sorted and each row's positions increase: walk a pointer
    // instead of a binary search per sample.
    let mut j = 0usize;
    for &s in starts {
        let first = ((s + 0.5 * t).floor() as i64).min(n - 1);
        if j > 0 && edges[j - 1] > first {
            j = upper_bound(edges, first);
        }
        let mut k2 = j;
        for k in 0..nbits {
            let pos = ((s + (k as f64 + 0.5) * t).floor() as i64).min(n - 1);
            while k2 < edges.len() && edges[k2] <= pos {
                k2 += 1;
            }
            if k == 0 {
                j = k2;
            }
            let lv = initial ^ ((k2 & 1) as u8);
            out.push(if idle == 1 { lv } else { 1 - lv });
        }
    }
}

/// Greedy non-overlapping chain of frames.
pub fn k_chain(starts: &[f64], busy: &[f64], out: &mut Vec<u32>) {
    out.clear();
    // busy[i] increases with i, so the next start can be found by a forward scan
    let mut i = 0usize;
    let mut j = 0usize;
    while i < starts.len() {
        out.push(i as u32);
        j = j.max(i + 1);
        while j < starts.len() && starts[j] < busy[i] {
            j += 1;
        }
        i = j;
    }
}

/// For each x: index of the window [ws, we) containing it, or -1.
pub fn k_in_windows(ws: &[f64], we: &[f64], x: &[i64], out: &mut Vec<i32>) {
    // both inputs sorted: linear merge
    out.clear();
    out.reserve(x.len());
    let mut k = 0usize;
    for &v in x {
        let v = v as f64;
        while k < ws.len() && ws[k] <= v {
            k += 1;
        }
        out.push(if k > 0 && v < we[k - 1] { (k - 1) as i32 } else { -1 });
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UartMetrics {
    pub frames: u32,
    pub frames_ok: u32,
    pub explained_edges: u32,
    pub n_edges: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UartParams {
    pub t: f64,       // samples per bit
    pub idle: u8,
    pub data_bits: u8,
    pub parity: u8,   // 0 none, 1 even, 2 odd
    pub stop_bits: u8,
}

/// The "plugin glue" part: given kernel outputs, compute the metrics.
pub fn uart_glue(p: &UartParams, edges: &[i64], starts: &[f64], grid: &[u8], idx: &[u32], win_of_edge: &[i32],
                 fs: &[f64]) -> UartMetrics {
    let n_par = if p.parity == 0 { 0 } else { 1 };
    let nbits = 1 + p.data_bits as usize + n_par;
    let stride = 13usize;
    let mut ok = vec![false; idx.len()];
    let mut frames_ok = 0;
    for (j, &i) in idx.iter().enumerate() {
        let row = &grid[i as usize * stride..i as usize * stride + stride];
        let start_ok = row[0] == 0;
        let stop_ok = row[nbits..nbits + p.stop_bits as usize].iter().all(|&b| b == 1);
        let parity_ok = if n_par == 1 {
            let s: u32 = row[1..1 + p.data_bits as usize].iter().map(|&b| b as u32).sum::<u32>() + row[1 + p.data_bits as usize] as u32;
            (s & 1) == if p.parity == 1 { 0 } else { 1 }
        } else { true };
        ok[j] = start_ok && stop_ok && parity_ok;
        frames_ok += ok[j] as u32;
    }
    let tol = (0.75 / p.t).clamp(0.1, 0.3);
    let mut explained = 0;
    for (e, &k) in edges.iter().zip(win_of_edge) {
        if k >= 0 && ok[k as usize] {
            let ph = (*e as f64 - fs[k as usize]) / p.t;
            if (ph - ph.round()).abs() < tol {
                explained += 1;
            }
        }
    }
    let _ = starts;
    UartMetrics { frames: idx.len() as u32, frames_ok, explained_edges: explained, n_edges: edges.len() as u32 }
}

/// Scratch buffers reused across hypotheses (the grid is shared by all formats
/// of one (t, idle), like the Python cache).
#[derive(Default)]
pub struct Scratch {
    pub starts: Vec<f64>,
    pub grid: Vec<u8>,
    pub grid_key: Option<(u64, u8)>,
    pub busy: Vec<f64>,
    pub idx: Vec<u32>,
    pub fs: Vec<f64>,
    pub fe: Vec<f64>,
    pub win: Vec<i32>,
}

pub fn frame_len(p: &UartParams) -> (usize, f64) {
    let nbits = 1 + p.data_bits as usize + if p.parity == 0 { 0 } else { 1 };
    (nbits, (nbits + p.stop_bits as usize) as f64)
}

/// One hypothesis, all inside the core.
pub fn uart_eval(edges: &[i64], initial: u8, n: i64, p: &UartParams, s: &mut Scratch) -> UartMetrics {
    let key = (p.t.to_bits(), p.idle);
    if s.grid_key != Some(key) {
        k_starts(edges, initial, p.idle, &mut s.starts);
        let starts = std::mem::take(&mut s.starts);
        k_sample_grid(edges, initial, n, &starts, p.t, 13, p.idle, &mut s.grid);
        s.starts = starts;
        s.grid_key = Some(key);
    }
    let (nbits, frame) = frame_len(p);
    s.busy.clear();
    s.busy.extend(s.starts.iter().map(|&x| x + (nbits as f64 + 0.5) * p.t));
    k_chain(&s.starts, &s.busy, &mut s.idx);
    s.fs.clear();
    s.fe.clear();
    for &i in &s.idx {
        let st = s.starts[i as usize];
        s.fs.push(st);
        s.fe.push(st + frame * p.t);
    }
    k_in_windows(&s.fs, &s.fe, edges, &mut s.win);
    uart_glue(p, edges, &s.starts, &s.grid, &s.idx, &s.win, &s.fs)
}

/// All hypotheses of one channel in a single call (batched API).
pub fn uart_eval_grid(edges: &[i64], initial: u8, n: i64, ps: &[UartParams], out: &mut Vec<UartMetrics>) {
    let mut s = Scratch::default();
    out.clear();
    for p in ps {
        out.push(uart_eval(edges, initial, n, p, &mut s));
    }
}

pub const FORMATS: [(u8, u8, u8); 18] = {
    let mut f = [(0u8, 0u8, 0u8); 18];
    let mut i = 0;
    let mut db = 7;
    while db <= 9 {
        let mut par = 0;
        while par < 3 {
            let mut sb = 1;
            while sb <= 2 {
                f[i] = (db, par, sb);
                i += 1;
                sb += 1;
            }
            par += 1;
        }
        db += 1;
    }
    f
};

pub fn hypotheses(rate: f64, units: &[f64]) -> Vec<UartParams> {
    let std_bauds = [300.0, 1200.0, 2400.0, 4800.0, 9600.0, 14400.0, 19200.0, 31250.0, 38400.0, 57600.0, 74880.0,
                     115200.0, 230400.0, 250000.0, 460800.0, 500000.0, 921600.0, 1e6, 1.5e6, 2e6];
    let mut ts: Vec<f64> = units.to_vec();
    for b in std_bauds {
        if rate / b >= 3.0 {
            ts.push(rate / b);
        }
    }
    let mut out = vec![];
    for t in ts {
        for idle in [1u8, 0] {
            for (db, par, sb) in FORMATS {
                out.push(UartParams { t, idle, data_bits: db, parity: par, stop_bits: sb });
            }
        }
    }
    out
}
