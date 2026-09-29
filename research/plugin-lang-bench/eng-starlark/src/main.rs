//! Starlark via Meta's starlark-rust. The mmap'd samples are exposed without
//! copying through a custom StarlarkValue (holding the Arc<Mmap>) implementing
//! at()/length(); kernel outputs (bits/t/bounds) are moved into Arc'd custom
//! values the same way. Host functions come from a #[starlark_module].
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use allocative::Allocative;
use common::*;
use memmap2::Mmap;
use starlark::environment::{GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::{Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Value, ValueLike};
use starlark::StarlarkPagablePanic;
use starlark_derive::starlark_value;

fn index(v: Value, n: usize) -> starlark::Result<usize> {
    match v.unpack_i32() {
        Some(i) if i >= 0 && (i as usize) < n => Ok(i as usize),
        _ => Err(anyhow::anyhow!("index {v} out of range (len {n})").into()),
    }
}

#[derive(ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
struct Samples {
    #[allocative(skip)]
    m: Arc<Mmap>,
    n: usize,
}
starlark_simple_value!(Samples);
impl fmt::Debug for Samples {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Samples({})", self.n)
    }
}
impl fmt::Display for Samples {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<samples n={}>", self.n)
    }
}
#[starlark_value(type = "samples")]
impl<'v> StarlarkValue<'v> for Samples {
    fn at(&self, i: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let i = index(i, self.n)?;
        Ok(heap.alloc(unsafe { *(self.m.as_ptr() as *const u16).add(i) } as i32))
    }
    fn length(&self) -> starlark::Result<i32> {
        Ok(self.n as i32)
    }
}

macro_rules! buf {
    ($name:ident, $t:ty, $ty:literal) => {
        #[derive(ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
        struct $name(#[allocative(skip)] Arc<Vec<$t>>);
        starlark_simple_value!($name);
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "{}({})", $ty, self.0.len())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "<{} len={}>", $ty, self.0.len())
            }
        }
        #[starlark_value(type = $ty)]
        impl<'v> StarlarkValue<'v> for $name {
            fn at(&self, i: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
                let i = index(i, self.0.len())?;
                Ok(heap.alloc(self.0[i] as i64))
            }
            fn length(&self) -> starlark::Result<i32> {
                Ok(self.0.len() as i32)
            }
        }
    };
}
buf!(U8Buf, u8, "u8buf");
buf!(U32Buf, u32, "u32buf");

#[starlark_module]
fn host(builder: &mut GlobalsBuilder) {
    fn host_inc(x: i32) -> anyhow::Result<i32> {
        Ok(x + 1)
    }
}

fn int(v: Value) -> u64 {
    v.unpack_i32().map(|x| x as u64).unwrap_or_else(|| v.to_str().parse().unwrap())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = a[3].clone();
    let mut r = Report::new("starlark", &cap);
    let dialect = Dialect::Extended;
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::Print]).with(host).build();
    let src = std::fs::read_to_string(format!("{dir}/starlark/plugin.star")).unwrap();
    let ast = AstModule::parse("plugin.star", src, &dialect).unwrap();

    Module::with_temp_heap(|module| {
        let heap = module.heap();
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals).unwrap();
        let f = |name: &str| module.get(name).unwrap();

        let t = Instant::now();
        let sv = heap.alloc(Samples { m: cap.samples.clone(), n: cap.n });
        r.text("t1_method", "custom StarlarkValue (Arc<Mmap>) implementing at()/length() (zero-copy)");
        r.num("t1_zero_copy", 1.0);
        r.num("t1_ms", ms(t));

        let ch = cap.busiest();
        let n = cap.n.min(2_000_000);
        let t = Instant::now();
        let copy = heap.alloc(cap.samples()[..n].iter().map(|&v| v as i32).collect::<Vec<i32>>());
        r.num("t1_copy_ms", ms(t));

        let want = scan_ref(&cap, ch, n);
        let args = [sv, heap.alloc(n as i32), heap.alloc(ch as i32)];
        let t = Instant::now();
        let got = eval.eval_function(f("scan"), &args, &[]).unwrap();
        r.num("t2_samples", n as f64);
        r.num("t2_ms", ms(t));
        r.num("t2_ok", (int(got) == want as u64) as u8 as f64);
        let args = [copy, heap.alloc(n as i32), heap.alloc(ch as i32)];
        let t = Instant::now();
        let got = eval.eval_function(f("scan"), &args, &[]).unwrap();
        r.num("t2_list_ms", ms(t));
        r.num("t2_list_ok", (int(got) == want as u64) as u8 as f64);

        let inp = i2c_input(&cap);
        let want = i2c_ref(&inp);
        r.num("t3_bits", inp.bits.len() as f64);
        let t = Instant::now();
        let bits = heap.alloc(U8Buf(Arc::new(inp.bits)));
        let tt = heap.alloc(U32Buf(Arc::new(inp.t)));
        let bounds = heap.alloc(U32Buf(Arc::new(inp.bounds)));
        r.num("t3_handoff_ms", ms(t));
        let t = Instant::now();
        let out = eval.eval_function(f("i2c"), &[bits, tt, bounds], &[]).unwrap();
        r.num("t3_ms", ms(t));
        let el = |i: i32| out.at(heap.alloc(i), heap).unwrap();
        r.num("t3_records", el(0).length().unwrap() as f64);
        r.num("t3_ok", ((int(el(1)), int(el(2)), int(el(3))) == want) as u8 as f64);

        let lp_src = "def lp(n):\n    x = 0\n    for i in range(n):\n        x = host_inc(x)\n    return x\n".to_string();
        let lp_ast = AstModule::parse("lp.star", lp_src, &dialect).unwrap();
        eval.eval_module(lp_ast, &globals).unwrap();
        let lp = module.get("lp").unwrap();
        let t = Instant::now();
        eval.eval_function(lp, &[heap.alloc(100_000)], &[]).unwrap();
        r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
        let inc = f("inc");
        let t = Instant::now();
        let mut x = heap.alloc(0);
        for _ in 0..100_000 {
            x = eval.eval_function(inc, &[x], &[]).unwrap();
        }
        r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);
        let _ = x.downcast_ref::<Samples>();
    });

    let err = Module::with_temp_heap(|module| {
        let src = std::fs::read_to_string(format!("{dir}/starlark/error.star")).unwrap();
        let res = AstModule::parse("error.star", src, &dialect).and_then(|ast| {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals).map(|_| ())
        });
        match res {
            Ok(()) => "no error".to_string(),
            Err(e) => format!("{e}"),
        }
    });
    r.text("t5_error", &err);
    // the literal "undefined variable" variant is rejected before execution (name resolution)
    let err2 = Module::with_temp_heap(|module| {
        let src = "def inner(frame):\n    total = 0\n    # line 4 reads an undefined variable\n    total = total + missing_variable\n    return total\n\ndef outer():\n    return inner({})\n\nouter()\n";
        let ast = AstModule::parse("error_undef.star", src.to_string(), &dialect).unwrap();
        let mut eval = Evaluator::new(&module);
        match eval.eval_module(ast, &globals) {
            Ok(_) => "no error".to_string(),
            Err(e) => format!("{e}"),
        }
    });
    r.text("t5_error_undefined_var", &err2);
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.text(
        "notes",
        "starlark 0.14 (Dialect::Extended, standard globals). Samples: custom StarlarkValue {Arc<Mmap>} with at()/length() \
         (unsafe raw u16 read after bounds check); bits/t/bounds Vecs moved (no copy) into Arc'd custom values. \
         t2_list_* = same scan over a copied Starlark list (Vec<i32> -> list). Records are dicts. Ints: inline i32, BigInt beyond. \
         No while loops / recursion (disabled by spec); for-range only. Undefined names are compile-time errors (see t5_error_undefined_var).",
    );
    r.print();
}
