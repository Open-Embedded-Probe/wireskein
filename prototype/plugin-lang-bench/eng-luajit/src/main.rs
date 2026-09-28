//! LuaJIT 2.1 via mlua (vendored). Zero-copy through the LuaJIT FFI: the host passes
//! raw addresses as lightuserdata and the script does ffi.cast("const uint16_t*", p).
//! The FFI library is only loaded by Lua::unsafe_new_with (it is not a "safe" stdlib).
//! Memory stays alive because the host owns the Arc/Vec for the whole run.
//! Also measured: raw userdata with lua_CFunction __index, and Lua table copies.
use std::ffi::{c_int, c_void, CStr};
use std::sync::Arc;
use std::time::Instant;

use common::*;
use mlua::{ffi, AnyUserData, Function, LightUserData, Lua, LuaOptions, StdLib, Table, Value};

#[repr(C)]
struct Raw {
    ptr: *const u8,
    len: usize,
}
unsafe extern "C-unwind" fn raw_index<T: Copy + Into<f64>>(l: *mut ffi::lua_State) -> c_int {
    let r = &*(ffi::lua_touserdata(l, 1) as *const Raw);
    let i = ffi::lua_tointeger(l, 2);
    if i >= 1 && (i as usize) <= r.len {
        ffi::lua_pushnumber(l, (*(r.ptr as *const T).add(i as usize - 1)).into());
    } else {
        ffi::lua_pushnil(l);
    }
    1
}
unsafe fn raw_view<T: Copy + Into<f64>>(lua: &Lua, name: &CStr, ptr: *const T, len: usize) -> AnyUserData {
    lua.exec_raw::<AnyUserData>((), |l| {
        let p = ffi::lua_newuserdata(l, std::mem::size_of::<Raw>()) as *mut Raw;
        p.write(Raw { ptr: ptr as *const u8, len });
        if ffi::luaL_newmetatable(l, name.as_ptr()) != 0 {
            ffi::lua_pushcfunction(l, raw_index::<T>);
            ffi::lua_setfield(l, -2, c"__index".as_ptr());
        }
        ffi::lua_setmetatable(l, -2);
    })
    .unwrap()
}

fn lud<T>(p: *const T) -> Value {
    Value::LightUserData(LightUserData(p as *mut c_void))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let inp = i2c_input(&cap);
    let want = i2c_ref(&inp);
    let (bits, tt, bounds) = (Arc::new(inp.bits), Arc::new(inp.t), Arc::new(inp.bounds));
    let mut r = Report::new("luajit", &cap);
    let lua = unsafe { Lua::unsafe_new_with(StdLib::ALL_SAFE | StdLib::FFI | StdLib::JIT, LuaOptions::new()) };
    lua.load(std::fs::read_to_string(format!("{dir}/lua/plugin_jit.lua")).unwrap())
        .set_name(format!("@{dir}/lua/plugin_jit.lua"))
        .exec()
        .unwrap();
    let g = lua.globals();
    let jit: String = lua.load("return jit.version .. ' ' .. jit.arch .. ' jit=' .. tostring((jit.status()))").eval().unwrap();
    let ch = cap.busiest();
    let n = cap.n.min(2_000_000);
    let sref = scan_ref(&cap, ch, n);

    // T1 + T2
    let _keep = cap.samples.clone(); // the Arc outlives every call below
    let t = Instant::now();
    let p = lud(cap.samples.as_ptr());
    let t1 = ms(t);
    let t = Instant::now();
    let raw = unsafe { raw_view(&lua, c"wsk.u16", cap.samples.as_ptr() as *const u16, cap.n) };
    let t1_raw = ms(t);
    let t = Instant::now();
    let tbl = lua.create_sequence_from(cap.samples()[..n].iter().copied()).unwrap();
    let t1_copy = ms(t);
    let scan_ffi: Function = g.get("scan_ffi").unwrap();
    let scan: Function = g.get("scan").unwrap();
    let run = |f: &Function, v: Value| {
        let t = Instant::now();
        let got: u32 = f.call((v, n, ch)).unwrap();
        (ms(t), got == sref)
    };
    let (t2, ok) = run(&scan_ffi, p);
    let (t2_raw, ok_raw) = run(&scan, Value::UserData(raw));
    let (t2_tbl, ok_tbl) = run(&scan, Value::Table(tbl));
    lua.gc_collect().unwrap();
    r.text("t1_method", "lightuserdata address + LuaJIT FFI ffi.cast(\"const uint16_t*\") (zero-copy)");
    r.num("t1_zero_copy", 1.0);
    r.num("t1_ms", t1);
    r.num("t1_raw_ud_ms", t1_raw);
    r.num("t1_copy_ms", t1_copy);
    r.num("t2_samples", n as f64);
    r.num("t2_ms", t2);
    r.num("t2_ok", (ok && ok_raw && ok_tbl) as u8 as f64);
    r.num("t2_raw_ud_ms", t2_raw);
    r.num("t2_table_ms", t2_tbl);

    // T3
    let i2c_ffi: Function = g.get("i2c_ffi").unwrap();
    let i2c: Function = g.get("i2c").unwrap();
    let chk = |(recs, tx, nb, x): (Table, u64, u64, u64)| (recs.raw_len(), (tx, nb, x) == want);
    let t = Instant::now();
    let args = (lud(bits.as_ptr()), lud(tt.as_ptr()), lud(bounds.as_ptr()), bounds.len());
    let h = ms(t);
    let t = Instant::now();
    let (recs, ok) = chk(i2c_ffi.call(args).unwrap());
    let m = ms(t);
    let t = Instant::now();
    let args = (
        lua.create_sequence_from(bits.iter().copied()).unwrap(),
        lua.create_sequence_from(tt.iter().copied()).unwrap(),
        lua.create_sequence_from(bounds.iter().copied()).unwrap(),
    );
    let h_tbl = ms(t);
    let t = Instant::now();
    let (_, ok_tbl) = chk(i2c.call(args).unwrap());
    let m_tbl = ms(t);
    let t = Instant::now();
    let args = unsafe {
        (
            raw_view(&lua, c"wsk.u8", bits.as_ptr(), bits.len()),
            raw_view(&lua, c"wsk.u32", tt.as_ptr(), tt.len()),
            raw_view(&lua, c"wsk.u32", bounds.as_ptr(), bounds.len()),
        )
    };
    let h_raw = ms(t);
    // plain i2c() uses #bounds; raw userdata has no __len here, so pass a table for bounds
    let bt = lua.create_sequence_from(bounds.iter().copied()).unwrap();
    let t = Instant::now();
    let (_, ok_raw) = chk(i2c.call((args.0, args.1, bt)).unwrap());
    let m_raw = ms(t);
    r.num("t3_bits", bits.len() as f64);
    r.text("t3_variant", "FFI pointers (zero-copy)");
    r.num("t3_handoff_ms", h);
    r.num("t3_ms", m);
    r.num("t3_records", recs as f64);
    r.num("t3_ok", (ok && ok_tbl && ok_raw) as u8 as f64);
    r.num("t3_table_handoff_ms", h_tbl);
    r.num("t3_table_ms", m_tbl);
    r.num("t3_raw_ud_handoff_ms", h_raw);
    r.num("t3_raw_ud_ms", m_raw);

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
            "{jit} via mlua 0.10 vendored; Lua::unsafe_new_with(ALL_SAFE|FFI|JIT) needed for ffi (FFI = scripts can read/write any address). \
             t2/t3 = FFI pointer scripts (0-based, *_ffi in plugin_jit.lua); t*_raw_ud = userdata with lua_CFunction __index (C calls are not JIT-compiled); \
             t*_table = Lua table copies (t1_copy_ms copies only t2_samples). LuaJIT numbers are doubles, bit ops are 32-bit via the bit library \
             (separate script from Lua 5.4: 5.4 bitwise operators do not parse in LuaJIT)."
        ),
    );
    r.print();
}
