//! WireSkein plugin-lang-bench plugin, Rust -> wasm32-unknown-unknown.
//! All pointers are offsets into this module's linear memory. The host either
//! copies inputs into buffers from `alloc`, or (T1 zero-copy) maps the sample
//! file directly into a region of the linear memory and passes its offset.
mod error;

#[link(wasm_import_module = "host")]
extern "C" {
    fn inc(x: i32) -> i32;
    /// Receives the formatted panic message (message + file:line:col).
    fn panic_msg(ptr: *const u8, len: usize);
}

#[no_mangle]
pub extern "C" fn init() {
    std::panic::set_hook(Box::new(|info| {
        let s = info.to_string();
        unsafe { panic_msg(s.as_ptr(), s.len()) }
    }));
}

#[no_mangle]
pub extern "C" fn alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[no_mangle]
pub unsafe extern "C" fn scan(samples: *const u16, n: usize, ch: u32) -> u32 {
    let s = std::slice::from_raw_parts(samples, n);
    let mut c = 0;
    let mut p = s[0] >> ch & 1;
    for &v in &s[1..] {
        let v = v >> ch & 1;
        c += (v != p) as u32;
        p = v;
    }
    c
}

pub enum Rw {
    Read,
    Write,
}
pub struct Record {
    pub addr: u8,
    pub rw: Rw,
    pub addr_ack: bool,
    pub bytes: Vec<u8>,
    pub start: u32,
    pub end: u32,
}
static mut RECORDS: Vec<Record> = Vec::new();

/// Builds the records inside wasm memory (kept in RECORDS); writes
/// (transactions, data_bytes, xor) to out[0..3]; returns the record count.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn i2c(
    bits: *const u8, nbits: usize, t: *const u32, nt: usize, bounds: *const u32, nbounds: usize, out: *mut u64,
) -> usize {
    let bits = std::slice::from_raw_parts(bits, nbits);
    let t = std::slice::from_raw_parts(t, nt);
    let bounds = std::slice::from_raw_parts(bounds, nbounds);
    let recs = &mut RECORDS;
    recs.clear();
    let (mut tx, mut nb, mut x) = (0u64, 0u64, 0u64);
    for f in bounds.chunks_exact(2) {
        let (a, b) = (f[0] as usize, f[1] as usize);
        let nw = (b - a) / 9;
        if nw == 0 {
            continue;
        }
        let mut bytes = Vec::with_capacity(nw - 1);
        let (mut first, mut ack) = (0u8, false);
        for w in 0..nw {
            let mut v = 0u32;
            for k in 0..9 {
                v = (v << 1) | bits[a + w * 9 + k] as u32;
            }
            x ^= v as u64;
            if w == 0 {
                first = (v >> 1) as u8;
                ack = v & 1 == 0;
            } else {
                bytes.push((v >> 1) as u8);
            }
        }
        tx += 1;
        nb += nw as u64 - 1;
        recs.push(Record {
            addr: first >> 1,
            rw: if first & 1 == 1 { Rw::Read } else { Rw::Write },
            addr_ack: ack,
            bytes,
            start: t[a],
            end: t[b - 1],
        });
    }
    *out = tx;
    *out.add(1) = nb;
    *out.add(2) = x;
    recs.len()
}

#[no_mangle]
pub extern "C" fn inc_export(x: i32) -> i32 {
    x + 1
}

/// T4: call the host import `inc` n times.
#[no_mangle]
pub extern "C" fn host_inc_loop(n: u32) -> i32 {
    let mut x = 0;
    for _ in 0..n {
        x = unsafe { inc(x) };
    }
    x
}

#[no_mangle]
pub extern "C" fn error_test() -> u32 {
    error::outer() as u32
}

#[no_mangle]
pub extern "C" fn error_trap() -> u32 {
    error::outer_trap()
}
