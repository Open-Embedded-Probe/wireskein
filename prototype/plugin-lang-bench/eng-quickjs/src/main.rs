//! QuickJS via rquickjs. Samples and kernel outputs are exposed without copying:
//! an ArrayBuffer over external memory (the mmap, kept alive by an Arc clone)
//! and Vec ownership moved into ArrayBuffers.
use std::ffi::c_void;
use std::time::Instant;

use common::*;
use rquickjs::{qjs, ArrayBuffer, Context, Ctx, Function, Runtime, TypedArray, Value};

unsafe extern "C" fn free_holder<T>(_rt: *mut qjs::JSRuntime, opaque: *mut c_void, _p: *mut c_void) {
    drop(Box::from_raw(opaque as *mut T));
}

fn external<'js, H: 'static>(ctx: &Ctx<'js>, holder: H, ptr: *const u8, len: usize) -> ArrayBuffer<'js> {
    let boxed = Box::into_raw(Box::new(holder));
    unsafe {
        let v = qjs::JS_NewArrayBuffer(ctx.as_raw().as_ptr(), ptr as *mut u8, len as _, Some(free_holder::<H>), boxed as *mut c_void, false);
        ArrayBuffer::from_value(Value::from_raw(ctx.clone(), v)).unwrap()
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let mut r = Report::new("quickjs", &cap);
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|ctx| {
        ctx.eval::<(), _>(std::fs::read_to_string(format!("{dir}/js/plugin.js")).unwrap()).unwrap();
        let g = ctx.globals();
        let t = Instant::now();
        let ab = external(&ctx, cap.samples.clone(), cap.samples.as_ptr(), cap.samples.len());
        let view = TypedArray::<u16>::from_arraybuffer(ab).unwrap();
        r.text("t1_method", "external ArrayBuffer over the mmap (Arc kept by the JS object)");
        r.num("t1_zero_copy", 1.0);
        r.num("t1_ms", ms(t));
        let t = Instant::now();
        let c = TypedArray::<u16>::new_copy(ctx.clone(), cap.samples()).unwrap();
        r.num("t1_copy_ms", ms(t));
        drop(c);
        let ch = cap.busiest();
        let n = cap.n.min(2_000_000);
        let scan: Function = g.get("scan").unwrap();
        let t = Instant::now();
        let got: u32 = scan.call((view, n as u32, ch as u32)).unwrap();
        r.num("t2_samples", n as f64);
        r.num("t2_ms", ms(t));
        r.num("t2_ok", (got == scan_ref(&cap, ch, n)) as u8 as f64);
        let inp = i2c_input(&cap);
        let want = i2c_ref(&inp);
        r.num("t3_bits", inp.bits.len() as f64);
        let t = Instant::now();
        let bits = TypedArray::<u8>::new(ctx.clone(), inp.bits.clone()).unwrap();
        let tt = TypedArray::<u32>::new(ctx.clone(), inp.t.clone()).unwrap();
        let bounds = TypedArray::<u32>::new(ctx.clone(), inp.bounds.clone()).unwrap();
        r.num("t3_handoff_ms", ms(t));
        let i2c: Function = g.get("i2c").unwrap();
        let t = Instant::now();
        let out: rquickjs::Array = i2c.call((bits, tt, bounds)).unwrap();
        r.num("t3_ms", ms(t));
        let recs: rquickjs::Array = out.get(0).unwrap();
        let tx: f64 = out.get(1).unwrap();
        let nb: f64 = out.get(2).unwrap();
        let x: f64 = out.get(3).unwrap();
        r.num("t3_records", recs.len() as f64);
        r.num("t3_ok", ((tx as u64, nb as u64, x as u64) == want) as u8 as f64);
        g.set("hostInc", Function::new(ctx.clone(), |x: i32| x + 1).unwrap()).unwrap();
        let lp: Function = ctx.eval("(function(n){ var x = 0; for (var i = 0; i < n; i++) x = hostInc(x); return x; })").unwrap();
        let t = Instant::now();
        let _: i32 = lp.call((100_000,)).unwrap();
        r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
        let inc: Function = g.get("inc").unwrap();
        let t = Instant::now();
        let mut x = 0i32;
        for _ in 0..100_000 {
            x = inc.call((x,)).unwrap();
        }
        r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);
        let err = match ctx.eval_file::<(), _>(format!("{dir}/js/error.js")) {
            Ok(_) => "no error".to_string(),
            Err(rquickjs::Error::Exception) => {
                let e = ctx.catch();
                let o = e.as_object().cloned();
                let msg = o.as_ref().and_then(|o| o.get::<_, String>("message").ok()).unwrap_or_default();
                let stack = o.as_ref().and_then(|o| o.get::<_, String>("stack").ok()).unwrap_or_default();
                format!("{msg}\n{stack}")
            }
            Err(e) => e.to_string(),
        };
        r.text("t5_error", &err);
    });
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.print();
}
