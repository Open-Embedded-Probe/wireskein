// T5 error test. inner() is two calls deep (error_test -> outer -> inner).
#[inline(never)]
pub fn inner(frame: &Frame) -> usize {
    frame.missing.unwrap().len() + frame.total // line 4: None field unwrapped
}
#[inline(never)]
pub fn outer() -> usize {
    inner(core::hint::black_box(&Frame { missing: None, total: 0 }))
}
pub struct Frame {
    pub missing: Option<&'static str>,
    pub total: usize,
}
/// Variant without a Rust panic: an out-of-bounds load is a genuine wasm trap.
#[inline(never)]
pub fn inner_trap(p: usize) -> u32 {
    unsafe { core::ptr::read_volatile(p as *const u32) }
}
#[inline(never)]
pub fn outer_trap() -> u32 {
    inner_trap(core::hint::black_box(0xffff_fff0))
}
