//! Rhai (pure Rust). The mmap'd samples are exposed without copying through a
//! registered custom type holding an Arc<Mmap> with an indexer getter and len().
//! Kernel outputs (bits/t/bounds) are moved into Arc'd custom types the same way.
use std::sync::Arc;
use std::time::Instant;

use common::*;
use memmap2::Mmap;
use rhai::{Array, CallFnOptions, Dynamic, Engine, EvalAltResult, Map, Scope, AST, INT};

#[derive(Clone)]
struct Samples {
    m: Arc<Mmap>,
    n: usize,
}

impl Samples {
    #[inline]
    fn get(&mut self, i: INT) -> Result<INT, Box<EvalAltResult>> {
        let i = i as usize;
        if i >= self.n {
            return Err(format!("sample index {i} out of range").into());
        }
        // mmap is page aligned; n = len / 2
        Ok(unsafe { *(self.m.as_ptr() as *const u16).add(i) } as INT)
    }
}

#[derive(Clone)]
struct Buf<T: Copy + Into<INT> + Send + Sync + 'static>(Arc<Vec<T>>);

fn register_buf<T: Copy + Into<INT> + Send + Sync + 'static>(e: &mut Engine, name: &str) {
    e.register_type_with_name::<Buf<T>>(name)
        .register_indexer_get(|b: &mut Buf<T>, i: INT| -> Result<INT, Box<EvalAltResult>> {
            b.0.get(i as usize).map(|&v| v.into()).ok_or_else(|| format!("index {i} out of range").into())
        })
        .register_fn("len", |b: &mut Buf<T>| b.0.len() as INT);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let mut r = Report::new("rhai", &cap);

    let mut engine = Engine::new();
    #[cfg(not(feature = "unchecked"))]
    engine.set_max_expr_depths(0, 0);
    engine
        .register_type_with_name::<Samples>("Samples")
        .register_indexer_get(Samples::get)
        .register_fn("len", |s: &mut Samples| s.n as INT);
    register_buf::<u8>(&mut engine, "U8Buf");
    register_buf::<u32>(&mut engine, "U32Buf");
    engine.register_fn("host_inc", |x: INT| x + 1);

    let ast: AST = engine.compile_file(format!("{dir}/rhai/plugin.rhai").into()).unwrap();
    let mut scope = Scope::new();

    let t = Instant::now();
    let samples = Samples { m: cap.samples.clone(), n: cap.n };
    let sv = Dynamic::from(samples);
    r.text("t1_method", "registered custom type (Arc<Mmap>) with indexer getter + len() (zero-copy)");
    r.num("t1_zero_copy", 1.0);
    r.num("t1_ms", ms(t));

    let ch = cap.busiest();
    let n = cap.n.min(2_000_000);
    // copying alternative: rhai Array (Vec<Dynamic>) of the first n samples
    let t = Instant::now();
    let copy: Array = cap.samples()[..n].iter().map(|&v| Dynamic::from_int(v as INT)).collect();
    let copy = Dynamic::from_array(copy);
    r.num("t1_copy_ms", ms(t));

    let want = scan_ref(&cap, ch, n);
    let t = Instant::now();
    let got: INT = engine.call_fn(&mut scope, &ast, "scan", (sv, n as INT, ch as INT)).unwrap();
    r.num("t2_samples", n as f64);
    r.num("t2_ms", ms(t));
    r.num("t2_ok", (got as u32 == want) as u8 as f64);
    let t = Instant::now();
    let got_c: INT = engine.call_fn(&mut scope, &ast, "scan", (copy, n as INT, ch as INT)).unwrap();
    let t2_copy_ms = ms(t);
    r.num("t2_array_ms", t2_copy_ms);
    r.num("t2_array_ok", (got_c as u32 == want) as u8 as f64);

    let inp = i2c_input(&cap);
    let want = i2c_ref(&inp);
    r.num("t3_bits", inp.bits.len() as f64);
    let t = Instant::now();
    let bits = Dynamic::from(Buf(Arc::new(inp.bits)));
    let tt = Dynamic::from(Buf(Arc::new(inp.t)));
    let bounds = Dynamic::from(Buf(Arc::new(inp.bounds)));
    r.num("t3_handoff_ms", ms(t));
    let t = Instant::now();
    let out: Array = engine.call_fn(&mut scope, &ast, "i2c", (bits, tt, bounds)).unwrap();
    r.num("t3_ms", ms(t));
    let recs = out[0].clone().into_typed_array::<Map>().unwrap();
    let tx = out[1].as_int().unwrap() as u64;
    let nb = out[2].as_int().unwrap() as u64;
    let x = out[3].as_int().unwrap() as u64;
    r.num("t3_records", recs.len() as f64);
    r.num("t3_ok", ((tx, nb, x) == want) as u8 as f64);

    let lp = engine.compile("let x = 0; for i in 0..100000 { x = host_inc(x); } x").unwrap();
    let t = Instant::now();
    let _: INT = engine.eval_ast(&lp).unwrap();
    r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
    let t = Instant::now();
    let mut x: INT = 0;
    for _ in 0..100_000 {
        x = engine.call_fn_with_options(CallFnOptions::new().eval_ast(false).rewind_scope(false), &mut scope, &ast, "inc", (x,)).unwrap();
    }
    r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);

    let err = match engine.run_file(format!("{dir}/rhai/error.rhai").into()) {
        Ok(_) => "no error".to_string(),
        Err(e) => e.to_string(),
    };
    r.text("t5_error", &err);
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.text(
        "notes",
        &format!(
            "rhai 1.26 features: {}. Samples: custom type Samples{{Arc<Mmap>}} + register_indexer_get (unsafe raw u16 read after bounds check). \
             T3: bits/t/bounds Vecs moved (no copy) into Arc'd custom types with indexer + len(). t2_array_* = same scan over a copied rhai Array \
             (Vec<Dynamic>, 16 B/elem; t1_copy_ms is that copy of t2_samples elements). Records are rhai object maps. \
             Script ints are i64 only; script functions are pure (cannot see outer/global variables). Every indexer/operator is a dynamic fn dispatch.",
            if cfg!(feature = "unchecked") { "default + unchecked" } else { "default" }
        ),
    );
    r.print();
}
