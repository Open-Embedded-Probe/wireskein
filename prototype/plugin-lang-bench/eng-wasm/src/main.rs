//! wasmtime host + Rust plugin compiled to wasm32-unknown-unknown (../wasm-plugin).
//!
//! Plugin path: $WSK_WASM_PLUGIN, else
//!   <scripts_dir>/../wasm-plugin/target/wasm32-unknown-unknown/release/wasm_plugin.wasm
//! (build it with `cargo build --release --target wasm32-unknown-unknown` in wasm-plugin/).
//!
//! T1 zero-copy: every linear memory is created by our MemoryCreator (Linux mmap):
//! a PROT_NONE reservation of (reservation + guard) bytes; accessible pages are
//! mprotect'ed RW on grow. After instantiation the host grows the memory by
//! ceil(file/64 KiB) wasm pages and mmaps the sample file MAP_PRIVATE|MAP_FIXED
//! (copy-on-write, like common::load) over that region, so wasm reads page-cache
//! pages directly. The plugin gets the offset as an ordinary pointer.
use std::sync::Arc;
use std::time::Instant;

use common::*;
use wasmtime::*;

struct MmapMem {
    base: *mut u8,
    size: usize,
    reserve: usize, // accessible limit (without guard)
    total: usize,   // reserve + guard, unmapped on drop
}
unsafe impl Send for MmapMem {}
unsafe impl Sync for MmapMem {}
impl Drop for MmapMem {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base as *mut _, self.total) };
    }
}
unsafe impl LinearMemory for MmapMem {
    fn byte_size(&self) -> usize {
        self.size
    }
    fn byte_capacity(&self) -> usize {
        self.reserve
    }
    fn grow_to(&mut self, new: usize) -> Result<()> {
        if new > self.reserve {
            bail!("grow beyond reservation");
        }
        if new > self.size {
            let r = unsafe { libc::mprotect(self.base.add(self.size) as *mut _, new - self.size, libc::PROT_READ | libc::PROT_WRITE) };
            if r != 0 {
                bail!("mprotect: {}", std::io::Error::last_os_error());
            }
        }
        self.size = new;
        Ok(())
    }
    fn as_ptr(&self) -> *mut u8 {
        self.base
    }
}
struct MmapCreator;
unsafe impl MemoryCreator for MmapCreator {
    fn new_memory(&self, _ty: MemoryType, min: usize, _max: Option<usize>, reserved: Option<usize>, guard: usize) -> std::result::Result<Box<dyn LinearMemory>, String> {
        let reserve = reserved.unwrap_or(4 << 30);
        let total = reserve + guard;
        let p = unsafe { libc::mmap(std::ptr::null_mut(), total, libc::PROT_NONE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE, -1, 0) };
        if p == libc::MAP_FAILED {
            return Err(format!("mmap reserve: {}", std::io::Error::last_os_error()));
        }
        let mut m = MmapMem { base: p as *mut u8, size: 0, reserve, total };
        m.grow_to(min).map_err(|e| e.to_string())?;
        Ok(Box::new(m))
    }
}

/// Map `len` bytes of `path` (from file offset 0) at `dst` (page aligned) copy-on-write.
unsafe fn map_file_at(path: &str, dst: *mut u8, len: usize) {
    use std::os::fd::AsRawFd;
    let f = std::fs::File::open(path).unwrap();
    let p = libc::mmap(dst as *mut _, len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_FIXED, f.as_raw_fd(), 0);
    assert!(p == dst as *mut _, "mmap file: {}", std::io::Error::last_os_error());
}

struct Host {
    panic: Option<String>,
}

fn copy_in(store: &mut Store<Host>, alloc: &TypedFunc<u32, u32>, mem: &Memory, bytes: &[u8]) -> u32 {
    let p = alloc.call(&mut *store, bytes.len() as u32).unwrap();
    mem.write(&mut *store, p as usize, bytes).unwrap();
    p
}
fn as_bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let wasm_path = std::env::var("WSK_WASM_PLUGIN")
        .unwrap_or(format!("{dir}/../wasm-plugin/target/wasm32-unknown-unknown/release/wasm_plugin.wasm"));
    let mut r = Report::new("wasm", &cap);

    let mut cfg = Config::new();
    cfg.with_host_memory(Arc::new(MmapCreator));
    // required: with CoW memory images on, wasmtime 49 hits unreachable!() ("memory_image is Some
    // only for mmap-based memories") when a custom MemoryCreator is installed.
    cfg.memory_init_cow(false);
    cfg.wasm_backtrace_details(WasmBacktraceDetails::Enable);
    let engine = Engine::new(&cfg).unwrap();
    let t = Instant::now();
    let module = Module::from_file(&engine, &wasm_path).unwrap();
    let compile_ms = ms(t);
    let mut store = Store::new(&engine, Host { panic: None });
    let mut linker: Linker<Host> = Linker::new(&engine);
    linker.func_wrap("host", "inc", |x: i32| x + 1).unwrap();
    linker
        .func_wrap("host", "panic_msg", |mut c: Caller<'_, Host>, p: u32, n: u32| {
            let mem = c.get_export("memory").unwrap().into_memory().unwrap();
            let s = String::from_utf8_lossy(&mem.data(&c)[p as usize..(p + n) as usize]).into_owned();
            c.data_mut().panic = Some(s);
        })
        .unwrap();
    let t = Instant::now();
    let inst = linker.instantiate(&mut store, &module).unwrap();
    let inst_ms = ms(t);
    let mem = inst.get_memory(&mut store, "memory").unwrap();
    inst.get_typed_func::<(), ()>(&mut store, "init").unwrap().call(&mut store, ()).unwrap();
    let alloc = inst.get_typed_func::<u32, u32>(&mut store, "alloc").unwrap();

    // T1 zero-copy: grow by whole wasm pages and map the file over the new region.
    let t = Instant::now();
    let flen = cap.samples.len();
    let pages = flen.div_ceil(65536) as u64;
    let off = mem.grow(&mut store, pages).unwrap() as usize * 65536;
    let file_pages = flen.div_ceil(4096) * 4096; // don't map whole pages past EOF (SIGBUS)
    unsafe { map_file_at(&format!("{}/{}.u16", a[1], a[2]), mem.data_ptr(&store).add(off), file_pages) };
    let zc_ms = ms(t);
    let zc_ok = &mem.data(&store)[off..off + flen] == &cap.samples[..];
    r.text("t1_method", "sample file mmap'd MAP_PRIVATE|MAP_FIXED into the wasm linear memory (custom MemoryCreator); copy alternative = alloc + Memory::write");
    r.num("t1_zero_copy", zc_ok as u8 as f64);
    r.num("t1_ms", zc_ms);
    // copying alternative
    let t = Instant::now();
    let pcopy = copy_in(&mut store, &alloc, &mem, &cap.samples[..]);
    r.num("t1_copy_ms", ms(t));

    let ch = cap.busiest();
    let n = cap.n.min(2_000_000);
    let scan = inst.get_typed_func::<(u32, u32, u32), u32>(&mut store, "scan").unwrap();
    let t = Instant::now();
    let got = scan.call(&mut store, (off as u32, n as u32, ch as u32)).unwrap();
    r.num("t2_samples", n as f64);
    r.num("t2_ms", ms(t));
    let want = scan_ref(&cap, ch, n);
    let got_copy = scan.call(&mut store, (pcopy, n as u32, ch as u32)).unwrap();
    r.num("t2_ok", (got == want && got_copy == want) as u8 as f64);

    let inp = i2c_input(&cap);
    let want = i2c_ref(&inp);
    r.num("t3_bits", inp.bits.len() as f64);
    let t = Instant::now();
    let pb = copy_in(&mut store, &alloc, &mem, &inp.bits);
    let pt = copy_in(&mut store, &alloc, &mem, as_bytes(&inp.t));
    let pbo = copy_in(&mut store, &alloc, &mem, as_bytes(&inp.bounds));
    let pout = alloc.call(&mut store, 24).unwrap();
    r.num("t3_handoff_ms", ms(t));
    let i2c = inst.get_typed_func::<(u32, u32, u32, u32, u32, u32, u32), u32>(&mut store, "i2c").unwrap();
    let t = Instant::now();
    let nrec = i2c
        .call(&mut store, (pb, inp.bits.len() as u32, pt, inp.t.len() as u32, pbo, inp.bounds.len() as u32, pout))
        .unwrap();
    r.num("t3_ms", ms(t));
    let mut o = [0u8; 24];
    mem.read(&store, pout as usize, &mut o).unwrap();
    let g = |k: usize| u64::from_le_bytes(o[k * 8..k * 8 + 8].try_into().unwrap());
    r.num("t3_records", nrec as f64);
    r.num("t3_ok", ((g(0), g(1), g(2)) == want) as u8 as f64);

    let lp = inst.get_typed_func::<u32, i32>(&mut store, "host_inc_loop").unwrap();
    let t = Instant::now();
    assert_eq!(lp.call(&mut store, 100_000).unwrap(), 100_000);
    r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
    let inc = inst.get_typed_func::<i32, i32>(&mut store, "inc_export").unwrap();
    let t = Instant::now();
    let mut x = 0;
    for _ in 0..100_000 {
        x = inc.call(&mut store, x).unwrap();
    }
    r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);
    assert_eq!(x, 100_000);

    let mut run_err = |name: &str| -> String {
        let f = inst.get_typed_func::<(), u32>(&mut store, name).unwrap();
        match f.call(&mut store, ()) {
            Ok(_) => "no error".into(),
            Err(e) => {
                let p = store.data_mut().panic.take().map(|p| format!("panic: {p}\n")).unwrap_or_default();
                format!("{p}{e:?}")
            }
        }
    };
    let e5 = run_err("error_test");
    r.text("t5_error", &e5);
    let e5b = run_err("error_trap");
    eprintln!("--- error_trap (raw OOB trap, no panic):\n{e5b}");
    r.num("compile_ms", compile_ms);
    r.num("instantiate_ms", inst_ms);
    r.num("zero_copy_offset", off as f64);
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.text("notes", &format!(
        "wasmtime 49 (cranelift JIT), plugin Rust->wasm32-unknown-unknown ({}). T1 zero-copy via custom MemoryCreator: \
linear memory = our PROT_NONE mmap reservation (4 GiB + guard, static so the base never moves); host grows by {} wasm pages \
and maps the .u16 file MAP_PRIVATE|MAP_FIXED at wasm offset {off} (64 KiB-aligned, so OK for 4 KiB/16 KiB/64 KiB OS pages); \
the plugin's dlmalloc just sees a gap (its later memory.grow lands after the file). Caveats: Linux/POSIX mmap only (Windows needs \
VirtualAlloc2 placeholders + MapViewOfFile3); wasm32 limits a capture to <4 GiB total (memory64 needed beyond, and memory64 \
loses guard-page bounds-check elision); unsafe host code must keep the reservation/guard contract; plugin writes COW the pages \
(file not modified); with a MemoryCreator, Config::memory_init_cow(false) is mandatory (else wasmtime 49 panics: unreachable \"memory_image is Some only for mmap-based memories\"), so data segments are copied at instantiation. T3 bits/t/bounds copied via alloc+Memory::write. \
Records built as Vec<Record> in wasm memory, host reads only the count. T5 also: raw OOB load trap text -> stderr.",
        wasm_path.rsplit('/').next().unwrap(), pages
    ));
    r.print();
}
