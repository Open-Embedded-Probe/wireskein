//! Lua 5.4 via mlua (vendored). Lua has no "external buffer" value, so zero-copy
//! access goes through a userdata whose __index reads the host memory. Two flavours:
//!   * mlua typed userdata (UserData impl, safe Rust closure per access)
//!   * raw userdata: a 16-byte (ptr,len) block with a plain lua_CFunction __index
//! plus the copying alternative (a Lua integer table). All three are measured.
use std::ffi::{c_int, CStr};
use std::sync::Arc;
use std::time::Instant;

use common::*;
use memmap2::Mmap;
use mlua::{ffi, AnyUserData, Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};

// ---- typed (safe) userdata -------------------------------------------------
trait Src: 'static {
    fn at(&self, i: usize) -> Option<i64>;
    fn n(&self) -> usize;
}
struct SampleView(Arc<Mmap>, usize);
impl Src for SampleView {
    #[inline]
    fn at(&self, i: usize) -> Option<i64> {
        if i < self.1 {
            let p = self.0.as_ptr() as *const u16;
            Some(unsafe { *p.add(i) } as i64)
        } else {
            None
        }
    }
    fn n(&self) -> usize {
        self.1
    }
}
struct VecView<T>(Arc<Vec<T>>);
impl<T: Copy + Into<i64> + 'static> Src for VecView<T> {
    #[inline]
    fn at(&self, i: usize) -> Option<i64> {
        self.0.get(i).map(|&v| v.into())
    }
    fn n(&self) -> usize {
        self.0.len()
    }
}
struct Ud<S: Src>(S);
impl<S: Src> UserData for Ud<S> {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        // 1-based like a Lua sequence
        m.add_meta_method(MetaMethod::Index, |_, s, i: i64| Ok(if i >= 1 { s.0.at(i as usize - 1) } else { None }));
        m.add_meta_method(MetaMethod::Len, |_, s, ()| Ok(s.0.n()));
    }
}

// ---- raw userdata: lua_CFunction __index, no mlua callback machinery --------
#[repr(C)]
struct Raw {
    ptr: *const u8,
    len: usize,
}
unsafe extern "C-unwind" fn raw_index<T: Copy + Into<i64>>(l: *mut ffi::lua_State) -> c_int {
    let r = &*(ffi::lua_touserdata(l, 1) as *const Raw);
    let i = ffi::lua_tointeger(l, 2);
    if i >= 1 && (i as usize) <= r.len {
        ffi::lua_pushinteger(l, (*(r.ptr as *const T).add(i as usize - 1)).into());
    } else {
        ffi::lua_pushnil(l);
    }
    1
}
unsafe extern "C-unwind" fn raw_len(l: *mut ffi::lua_State) -> c_int {
    let r = &*(ffi::lua_touserdata(l, 1) as *const Raw);
    ffi::lua_pushinteger(l, r.len as _);
    1
}
/// Caller must keep the memory alive while the userdata may be indexed.
unsafe fn raw_view<T: Copy + Into<i64>>(lua: &Lua, name: &CStr, ptr: *const T, len: usize) -> AnyUserData {
    lua.exec_raw::<AnyUserData>((), |l| {
        let p = ffi::lua_newuserdatauv(l, std::mem::size_of::<Raw>(), 0) as *mut Raw;
        p.write(Raw { ptr: ptr as *const u8, len });
        if ffi::luaL_newmetatable(l, name.as_ptr()) != 0 {
            ffi::lua_pushcfunction(l, raw_index::<T>);
            ffi::lua_setfield(l, -2, c"__index".as_ptr());
            ffi::lua_pushcfunction(l, raw_len);
            ffi::lua_setfield(l, -2, c"__len".as_ptr());
        }
        ffi::lua_setmetatable(l, -2);
    })
    .unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let inp = i2c_input(&cap);
    let want = i2c_ref(&inp);
    let (bits, tt, bounds) = (Arc::new(inp.bits), Arc::new(inp.t), Arc::new(inp.bounds));
    let mut r = Report::new("lua54", &cap);
    let lua = Lua::new();
    lua.load(std::fs::read_to_string(format!("{dir}/lua/plugin.lua")).unwrap())
        .set_name(format!("@{dir}/lua/plugin.lua"))
        .exec()
        .unwrap();
    let g = lua.globals();
    let scan: Function = g.get("scan").unwrap();
    let ch = cap.busiest();
    let n = cap.n.min(2_000_000);
    let sref = scan_ref(&cap, ch, n);

    // T1 + T2
    let t = Instant::now();
    let ud = lua.create_userdata(Ud(SampleView(cap.samples.clone(), cap.n))).unwrap();
    let t1_typed = ms(t);
    let t = Instant::now();
    let raw = unsafe { raw_view(&lua, c"wsk.u16", cap.samples.as_ptr() as *const u16, cap.n) };
    let t1_raw = ms(t);
    let t = Instant::now();
    let tbl = lua.create_sequence_from(cap.samples()[..n].iter().copied()).unwrap();
    let t1_copy = ms(t);

    let run_scan = |v: Value| {
        let t = Instant::now();
        let got: u32 = scan.call((v, n, ch)).unwrap();
        (ms(t), got == sref)
    };
    let (t2_typed, ok_typed) = run_scan(Value::UserData(ud));
    let (t2_raw, ok_raw) = run_scan(Value::UserData(raw));
    let (t2_tbl, ok_tbl) = run_scan(Value::Table(tbl));
    lua.gc_collect().unwrap();
    lua.gc_collect().unwrap();

    r.text("t1_method", "userdata with lua_CFunction __index over the mmap (zero-copy); also mlua UserData __index, and table copy");
    r.num("t1_zero_copy", 1.0);
    r.num("t1_ms", t1_raw);
    r.num("t1_typed_ms", t1_typed);
    r.num("t1_copy_ms", t1_copy);
    r.num("t2_samples", n as f64);
    r.num("t2_ms", t2_raw);
    r.num("t2_ok", (ok_raw && ok_typed && ok_tbl) as u8 as f64);
    r.num("t2_typed_ud_ms", t2_typed);
    r.num("t2_table_ms", t2_tbl);

    // T3
    let i2c: Function = g.get("i2c").unwrap();
    let run_i2c = |b: Value, t_: Value, bo: Value| {
        let t = Instant::now();
        let (recs, tx, nb, x): (Table, u64, u64, u64) = i2c.call((b, t_, bo)).unwrap();
        (ms(t), recs.raw_len(), (tx, nb, x) == want)
    };
    let t = Instant::now();
    let v = (
        unsafe { raw_view(&lua, c"wsk.u8", bits.as_ptr(), bits.len()) },
        unsafe { raw_view(&lua, c"wsk.u32", tt.as_ptr(), tt.len()) },
        unsafe { raw_view(&lua, c"wsk.u32", bounds.as_ptr(), bounds.len()) },
    );
    let h_raw = ms(t);
    let (m_raw, recs, ok_raw) = run_i2c(Value::UserData(v.0), Value::UserData(v.1), Value::UserData(v.2));
    let t = Instant::now();
    let v = (
        lua.create_userdata(Ud(VecView(bits.clone()))).unwrap(),
        lua.create_userdata(Ud(VecView(tt.clone()))).unwrap(),
        lua.create_userdata(Ud(VecView(bounds.clone()))).unwrap(),
    );
    let h_typed = ms(t);
    let (m_typed, _, ok_typed) = run_i2c(Value::UserData(v.0), Value::UserData(v.1), Value::UserData(v.2));
    let t = Instant::now();
    let v = (
        lua.create_sequence_from(bits.iter().copied()).unwrap(),
        lua.create_sequence_from(tt.iter().copied()).unwrap(),
        lua.create_sequence_from(bounds.iter().copied()).unwrap(),
    );
    let h_tbl = ms(t);
    let (m_tbl, _, ok_tbl) = run_i2c(Value::Table(v.0), Value::Table(v.1), Value::Table(v.2));
    r.num("t3_bits", bits.len() as f64);
    // report the variant with the lowest handoff+run total
    let best = [("raw userdata (zero-copy)", h_raw, m_raw), ("mlua UserData (zero-copy)", h_typed, m_typed), ("table copy", h_tbl, m_tbl)]
        .into_iter()
        .min_by(|a, b| (a.1 + a.2).total_cmp(&(b.1 + b.2)))
        .unwrap();
    r.text("t3_variant", best.0);
    r.num("t3_handoff_ms", best.1);
    r.num("t3_ms", best.2);
    r.num("t3_records", recs as f64);
    r.num("t3_ok", (ok_raw && ok_typed && ok_tbl) as u8 as f64);
    r.num("t3_raw_ud_handoff_ms", h_raw);
    r.num("t3_raw_ud_ms", m_raw);
    r.num("t3_typed_ud_handoff_ms", h_typed);
    r.num("t3_typed_ud_ms", m_typed);
    r.num("t3_table_handoff_ms", h_tbl);
    r.num("t3_table_ms", m_tbl);

    // T4
    g.set("hostInc", lua.create_function(|_, x: i64| Ok(x + 1)).unwrap()).unwrap();
    let lp: Function = lua.load("return function(n) local x = 0 for i = 1, n do x = hostInc(x) end return x end").eval().unwrap();
    let t = Instant::now();
    let _: i64 = lp.call(100_000).unwrap();
    r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
    let inc: Function = g.get("inc").unwrap();
    let t = Instant::now();
    let mut x = 0i64;
    for _ in 0..100_000 {
        x = inc.call(x).unwrap();
    }
    r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);
    assert_eq!(x, 100_000);

    // T5
    let path = format!("{dir}/lua/error.lua");
    let err = match lua.load(std::fs::read_to_string(&path).unwrap()).set_name(format!("@{path}")).exec() {
        Ok(_) => "no error".to_string(),
        Err(e) => e.to_string(),
    };
    r.text("t5_error", &err);
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.text(
        "notes",
        &format!(
            "{} via mlua 0.10 vendored. t1/t2 = raw userdata (16B ptr+len, lua_CFunction __index, unsafe; Arc kept by host); \
             t2_typed_ud = mlua UserData __index (safe closure, zero-copy); t2_table = scan over a Lua table; t1_copy_ms copies only t2_samples \
             elements into a table (full copy would be ~16 B/sample). Lua sequences are 1-based: userdata maps s[i] -> host[i-1]. \
             T3 handoff/run of all variants reported; t3_variant is the fastest total.",
            g.get::<String>("_VERSION").unwrap()
        ),
    );
    r.print();
}
